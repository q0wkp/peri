//! Bash 超时规则的端到端行为：有人值守/无人值守的后台时限、转后台时限、
//! `sleep` 前缀不转后台、环境变量默认值。规则数值本身见 `peri-agent` 的 `shell_timeout_test.rs`。

use super::*;
use peri_agent::agent::async_tasks::{DEFAULT_TIMEOUT_ENV, FOREGROUND_DEFAULT_TIMEOUT_MS};

fn policy(
    attendance: SessionAttendance,
    env: &[(&'static str, &'static str)],
) -> ShellTimeoutPolicy {
    let env = env.to_vec();
    ShellTimeoutPolicy::from_lookup(attendance, move |name| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    })
}

fn tool_with_events(
    policy: ShellTimeoutPolicy,
    registry: Arc<dyn peri_acp_types::tasks::TaskManager>,
) -> (
    BashTool,
    tokio::sync::mpsc::UnboundedReceiver<BackgroundTaskResult>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_timeout_policy(policy)
        .with_task_manager(registry)
        .with_on_bg_complete(Arc::new(move |result, _kind| {
            tx.send(result.clone()).map_err(|error| error.to_string())
        }));
    (tool, rx)
}

async fn run(tool: &BashTool, input: serde_json::Value) -> peri_agent::tools::ToolOutput {
    tool.invoke_output(input, peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .unwrap()
}

/// [回归] 有人值守会话的后台任务没有时限：`timeout` 不再被压成 10 分钟或在到时杀进程，
/// 命令跑完为止；回执明说请求被忽略，不静默。
#[cfg(unix)]
#[tokio::test]
async fn attended_background_task_ignores_timeout_and_runs_to_completion() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, mut rx) =
        tool_with_events(policy(SessionAttendance::Attended, &[]), registry.clone());

    let started = run(
        &tool,
        serde_json::json!({
            "command": "sh -c 'sleep 1; printf finished'",
            "run_in_background": true,
            "timeout": 300,
        }),
    )
    .await;
    assert!(
        started
            .text
            .contains("`timeout` (300ms) has no effect on background tasks"),
        "被忽略的 timeout 必须写进回执: {}",
        started.text
    );
    assert!(!started.text.contains("Time limit"), "{}", started.text);

    let result = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("后台任务应在 10s 内自然结束")
        .expect("completion callback");
    assert!(result.success, "不应被 300ms 的 timeout 杀掉");
    assert!(!result.timed_out);
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

/// 有人值守下超大的 `timeout`（事故中的 3000000）同样只是被说明、不生效，也不被改写成别的数值。
#[tokio::test]
async fn attended_background_receipt_explains_ignored_large_timeout() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, _rx) = tool_with_events(policy(SessionAttendance::Attended, &[]), registry.clone());
    let started = run(
        &tool,
        serde_json::json!({"command": "echo hi", "run_in_background": true, "timeout": 3_000_000}),
    )
    .await;
    assert!(started.text.contains("3000000ms"), "{}", started.text);
    assert!(
        started.text.contains("without a time limit"),
        "{}",
        started.text
    );
    assert!(!started.text.contains("600000"), "{}", started.text);
    peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await;
}

/// 无人值守的后台回执写明实际时限：默认 30 分钟、显式值原样生效、超上限被界到 2 小时并说明。
#[tokio::test]
async fn unattended_background_receipt_states_the_effective_limit() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, _rx) =
        tool_with_events(policy(SessionAttendance::Unattended, &[]), registry.clone());
    for (timeout, expected, capped) in [
        (None, "1800000ms", false),
        (Some(3_000_000u64), "3000000ms", false),
        (Some(99_999_999), "7200000ms", true),
    ] {
        let mut input = serde_json::json!({"command": "echo hi", "run_in_background": true});
        if let Some(timeout) = timeout {
            input["timeout"] = serde_json::json!(timeout);
        }
        let started = run(&tool, input).await;
        assert!(
            started.text.contains(expected),
            "{}: {}",
            expected,
            started.text
        );
        // 被界到上限时由改写说明承担时限表述；否则回执给出时限行。
        assert_eq!(
            started.text.contains("Time limit"),
            !capped,
            "{}",
            started.text
        );
        assert_eq!(
            started
                .text
                .contains("exceeds the maximum background time limit"),
            capped,
            "{}",
            started.text
        );
    }
    peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await;
}

/// 无人值守：前台超时转入后台的任务从转入时起受时限约束；到时停止并通知模型。
#[cfg(unix)]
#[tokio::test]
async fn unattended_moved_task_is_stopped_at_the_background_limit() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, mut rx) = tool_with_events(
        policy(SessionAttendance::Unattended, &[]).with_moved_limit_for_test(800),
        registry.clone(),
    );

    let started = Instant::now();
    let moved = run(
        &tool,
        serde_json::json!({"command": "sh -c 'echo started; sleep 30'", "timeout": 200}),
    )
    .await;
    assert!(
        moved.text.contains("moved to the background"),
        "{}",
        moved.text
    );
    assert!(
        moved
            .text
            .contains("stopped automatically 800ms after the move"),
        "回执应说明转入后任务的时限: {}",
        moved.text
    );

    let result = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("到时限后任务必须被停止并通知")
        .expect("completion callback");
    assert!(result.timed_out && !result.success);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "应在时限附近停止，而不是等 sleep 30 结束: {:?}",
        started.elapsed()
    );
    let notification = result.to_notification();
    assert!(notification.contains("state: timed_out"), "{notification}");
    assert!(
        notification.contains(
            "Background command \"sh -c 'echo started; sleep 30'\" was stopped after reaching its background time limit"
        ),
        "{notification}"
    );
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

/// 有人值守：转入后台的任务不限时，回执不带时限说明，任务自然结束。
#[cfg(unix)]
#[tokio::test]
async fn attended_moved_task_has_no_time_limit() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, mut rx) =
        tool_with_events(policy(SessionAttendance::Attended, &[]), registry.clone());
    let moved = run(
        &tool,
        serde_json::json!({"command": "sh -c 'echo started; sleep 1'", "timeout": 200}),
    )
    .await;
    assert!(
        moved.text.contains("moved to the background"),
        "{}",
        moved.text
    );
    assert!(!moved.text.contains("Time limit"), "{}", moved.text);
    let result = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("completion")
        .expect("completion callback");
    assert!(result.success && !result.timed_out);
    peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await;
}

/// 以 `sleep` 开头的命令前台超时直接终止，不转后台，也不登记后台任务。
#[cfg(unix)]
#[tokio::test]
async fn sleep_prefixed_foreground_timeout_is_stopped_not_moved() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, _rx) = tool_with_events(policy(SessionAttendance::Attended, &[]), registry.clone());
    let output = run(
        &tool,
        serde_json::json!({"command": "sleep 30", "timeout": 300}),
    )
    .await;
    assert!(
        output.text.contains("Command timed out after 0.3s"),
        "{}",
        output.text
    );
    assert!(
        output.text.contains("commands starting with `sleep`"),
        "应说明为什么没有转后台: {}",
        output.text
    );
    assert!(!output.text.contains("task_id:"), "{}", output.text);
    let evidence = output.execution.expect("evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::TimedOut);
    assert_eq!(evidence.task_id, None);
    assert_eq!(registry.active_count(), 0, "不应登记后台任务");
}

/// `BASH_DEFAULT_TIMEOUT_MS` 经规则进入执行路径：未传 timeout 时按它转后台，秒数按实际生效值写入回执。
#[cfg(unix)]
#[tokio::test]
async fn foreground_default_comes_from_the_environment_policy() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    assert_ne!(FOREGROUND_DEFAULT_TIMEOUT_MS, 300);
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tool, _rx) = tool_with_events(
        policy(SessionAttendance::Attended, &[(DEFAULT_TIMEOUT_ENV, "300")]),
        registry.clone(),
    );
    let output = run(&tool, serde_json::json!({"command": "sh -c 'sleep 30'"})).await;
    assert!(
        output.text.contains(
            "Command did not complete within its 0.3s timeout and was moved to the background"
        ),
        "{}",
        output.text
    );
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}
