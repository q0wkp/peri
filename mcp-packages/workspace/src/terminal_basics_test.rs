use super::*;
use peri_agent::agent::async_tasks::{
    DEFAULT_TIMEOUT_ENV, FOREGROUND_DEFAULT_TIMEOUT_MS, FOREGROUND_MAX_TIMEOUT_MS, MAX_TIMEOUT_ENV,
};

#[tokio::test]
async fn test_bash_nonzero_exit_code() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "exit 42"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("42"), "应包含退出码: {result}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_bash_cd_does_not_persist_between_invocations() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().canonicalize().unwrap();
    let nested = cwd.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let tool = BashTool::new(cwd.to_str().unwrap());

    for (command, expected) in [("cd nested && pwd -P", &nested), ("pwd -P", &cwd)] {
        let output = tool
            .invoke(
                serde_json::json!({"command": command}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert_eq!(output.trim(), expected.to_str().unwrap());
    }
}

/// 验证超时后在合理时间内返回，且进程组（bash + 全部子进程）被清理
#[tokio::test]
async fn test_bash_timeout_returns_quickly() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let start = Instant::now();

    // Windows 用 ping 模拟 sleep，Unix 用 sleep
    let (sleep_cmd, timeout_ms) = if cfg!(target_os = "windows") {
        ("ping -n 60 127.0.0.1", 1000)
    } else {
        ("sleep 60", 1000)
    };

    let result = tool
        .invoke(
            serde_json::json!({
                "command": sleep_cmd,
                "timeout": timeout_ms
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    let elapsed = start.elapsed();

    // 应在约 1 秒内返回（不超过 3 秒），不等待 sleep 60 完成
    assert!(
        elapsed.as_secs() < if cfg!(target_os = "windows") { 8 } else { 3 },
        "超时后应快速返回，实际耗时 {:?}",
        elapsed
    );
    assert!(
        err_msg.contains("timed out"),
        "返回值应包含超时提示: {err_msg}"
    );
}

#[tokio::test]
async fn test_bash_stderr_captured() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "echo err >&2"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("err"), "stderr 应被捕获: {result}");
}

#[test]
fn test_truncate_output_line_count_accurate() {
    // 生成不含末尾换行的多行文本，避免 split('\n') 产生额外空行
    let lines: Vec<String> = (0..3000).map(|i| format!("line {}", i)).collect();
    let input = lines.join("\n");
    assert_eq!(input.split('\n').count(), 3000);
    let result = truncate_output(&input);
    assert!(
        result.contains("3000 total lines"),
        "应显示正确的总行数: {result}"
    );
    // 应保留头部和尾部
    assert!(result.contains("line 0"), "应保留第一行: {result}");
    assert!(result.contains("line 2999"), "应保留最后一行: {result}");
    assert!(
        result.contains("lines truncated"),
        "应显示截断信息: {result}"
    );
}

#[test]
fn test_truncate_output_no_truncation_when_small() {
    let result = truncate_output("hello\nworld");
    assert_eq!(result, "hello\nworld");
}

#[test]
fn test_truncate_output_char_limit() {
    let long_line = "x".repeat(200_000);
    let result = truncate_output(&long_line);
    assert!(result.contains("byte limit"), "应截断超长输出: {result}");
}

#[test]
fn test_truncate_output_preserves_tail() {
    // 3000 行，尾部包含关键信息
    let mut lines: Vec<String> = (0..2999).map(|i| format!("line {}", i)).collect();
    lines.push("CRITICAL ERROR: test failed".to_string());
    let input = lines.join("\n");
    let result = truncate_output(&input);
    // 尾部关键行应保留
    assert!(
        result.contains("CRITICAL ERROR"),
        "截断后应保留尾部关键信息: {result}"
    );
    assert!(result.contains("line 0"), "应保留头部: {result}");
}

#[test]
fn test_bash_description_extended() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let desc = tool.description();
    assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
    assert!(
        desc.contains("dedicated tool"),
        "description 应强调优先使用专用工具"
    );
    assert!(desc.contains("timeout"), "description 应提及超时");
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

/// `timeout: 0` 在前台被界到前台上限（不再表示不超时）；显式正超时按请求值生效。
#[tokio::test]
async fn test_bash_timeout_clamped_to_minimum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let start = Instant::now();
    // timeout = 2000 → clamp 不生效，echo quick 应正常完成（PowerShell 冷启动较慢）
    let result = tool
        .invoke(
            serde_json::json!({
                "command": "echo quick",
                "timeout": 5000
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert!(result.contains("quick"), "echo quick 应正常输出: {result}");
    assert!(
        elapsed.as_millis() < 8000,
        "应快速完成，实际耗时 {:?}",
        elapsed
    );
}

/// 固定环境的前台规则（不读真实进程环境变量）。
fn fixed_policy(env: &[(&'static str, &'static str)]) -> ShellTimeoutPolicy {
    let env = env.to_vec();
    ShellTimeoutPolicy::from_lookup(SessionAttendance::Attended, move |name| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    })
}

/// 超过前台上限的请求被接受并按上限执行（不再是无界）。
/// 命令本身瞬时完成，因此这里只验证请求被接受且正常执行。
#[tokio::test]
async fn test_bash_oversized_timeout_is_capped_by_foreground_maximum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let policy = fixed_policy(&[]);
    assert_eq!(
        policy
            .parse_foreground(&serde_json::json!({"timeout": 3_000_000}))
            .expect("valid timeout"),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(3_000_000)),
        "超过前台上限的请求必须界到上限"
    );
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap()).with_timeout_policy(policy);
    let result = tool
        .invoke(
            serde_json::json!({
                "command": "echo ok",
                "timeout": 3_000_000
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("ok"));
}

/// `timeout: 0` 不能禁用同步超时：解析结果恒有界，请求仍走同步路径正常执行。
#[tokio::test]
async fn test_bash_sync_timeout_zero_is_bounded_by_foreground_maximum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let policy = fixed_policy(&[]);
    assert_eq!(
        policy
            .parse_foreground(&serde_json::json!({"timeout": 0}))
            .expect("valid timeout"),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(0)),
        "timeout: 0 必须界到前台上限，不能表示不超时"
    );
    assert_eq!(
        policy
            .parse_foreground(&serde_json::json!({}))
            .expect("missing timeout"),
        (FOREGROUND_DEFAULT_TIMEOUT_MS, None)
    );

    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap()).with_timeout_policy(policy);
    let output = tool
        .invoke_output(
            serde_json::json!({"command": "echo bounded", "timeout": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(output.text.contains("bounded"), "{}", output.text);
    let evidence = output.execution.expect("sync evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Completed);
}

/// 前台 `timeout: 0` 在上限处转后台：不早于上限、不晚于上限太多，回传 task_id，
/// 回执写明被界到上限，取消时清理整个进程组。
/// 用环境变量把默认与上限都压到 2s，真实等待约 2s。
#[cfg(unix)]
#[tokio::test]
async fn test_foreground_timeout_zero_moves_to_background_at_ceiling() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_timeout_policy(fixed_policy(&[
            (DEFAULT_TIMEOUT_ENV, "2000"),
            (MAX_TIMEOUT_ENV, "2000"),
        ]))
        .with_task_manager(registry.clone());

    let start = Instant::now();
    let err = tool
        .invoke(
            serde_json::json!({"command": "sh -c 'echo alive; sleep 600'", "timeout": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let elapsed = start.elapsed();

    assert!(
        err.contains(
            "Command did not complete within its 2s timeout and was moved to the background"
        ),
        "回执应写明实际生效秒数与转入后台: {err}"
    );
    assert!(
        err.lines().any(|l| l.starts_with("task_id: ")),
        "转入后台应回传 task_id: {err}"
    );
    assert!(err.contains("Live output"), "应给出输出位置: {err}");
    assert!(
        err.contains("cannot disable the timeout"),
        "应说明 `timeout: 0` 被界到前台上限: {err}"
    );
    assert!(
        elapsed >= Duration::from_millis(1800),
        "不应早于前台上限转入后台，实际 {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "不应晚于前台上限太多，实际 {elapsed:?}"
    );
    assert_eq!(registry.active_count(), 1, "转入后台后应注册为后台任务");
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

/// 前台 timeout 被界到上限时的回执说明（`0` 与超上限两种请求）。
#[test]
fn test_foreground_timeout_note_explains_bounded_sync_requests() {
    assert!(
        foreground_timeout_note(None, 600_000).is_empty(),
        "未改写时不应附加说明"
    );
    let zero = foreground_timeout_note(Some(0), 600_000);
    assert!(zero.contains("cannot disable the timeout"), "{zero}");
    assert!(zero.contains("600000"), "{zero}");
    let oversized = foreground_timeout_note(Some(3_000_000), 900_000);
    assert!(oversized.contains("3000000"), "{oversized}");
    assert!(
        oversized.contains("900000"),
        "说明应使用实际生效上限: {oversized}"
    );
    assert!(oversized.contains("run_in_background"), "{oversized}");
}

/// 工具描述与实现一致：数值取自当前会话生效的规则，不再出现旧的 15s / 2 分钟上限 /
/// 600000 后台上限描述，也不得宣传"0 = 不超时"。
#[test]
fn test_bash_timeout_description_matches_implementation() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_timeout_policy(fixed_policy(&[]));
    let timeout_schema = &tool.parameters()["properties"]["timeout"];
    assert_eq!(timeout_schema["type"].as_str(), Some("integer"));
    assert_eq!(timeout_schema["minimum"].as_u64(), Some(0));
    assert!(
        timeout_schema.get("maximum").is_none(),
        "schema 不得再写死 maximum：生效上限随环境变量变化，超限请求由回执说明"
    );
    let schema = timeout_schema["description"]
        .as_str()
        .expect("timeout 参数描述")
        .to_string();
    for text in [tool.description().to_string(), schema.clone()] {
        assert!(
            text.contains(&FOREGROUND_DEFAULT_TIMEOUT_MS.to_string()),
            "描述应写明前台默认超时: {text}"
        );
        assert!(
            text.contains(&FOREGROUND_MAX_TIMEOUT_MS.to_string()),
            "描述应写明前台上限: {text}"
        );
        assert!(
            !text.contains("0 = no timeout") && !text.contains("disable the timeout entirely"),
            "同步路径不得宣传 0 可禁用超时: {text}"
        );
        for stale in [
            "15000ms",
            "15s",
            "capped at 120000ms",
            "up to 600000ms) requests termination",
        ] {
            assert!(
                !text.contains(stale),
                "不得保留旧规则描述 {stale:?}: {text}"
            );
        }
    }
    assert!(
        schema.contains("no time limit") && schema.contains("ignore `timeout`"),
        "有人值守会话应说明后台不限时且忽略 timeout: {schema}"
    );
}

/// 无人值守会话的 schema 描述写明后台时限、默认与上限；环境变量改变的数值同步变化。
#[test]
fn test_bash_timeout_description_reflects_unattended_session_limits() {
    let unattended = BashTool::new(std::env::temp_dir().to_str().unwrap()).with_timeout_policy(
        ShellTimeoutPolicy::from_lookup(SessionAttendance::Unattended, |name| {
            (name == MAX_TIMEOUT_ENV).then(|| "9000000".to_string())
        }),
    );
    let schema = unattended.parameters()["properties"]["timeout"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(schema.contains("1800000"), "{schema}");
    assert!(schema.contains("9000000"), "{schema}");
    assert!(!schema.contains("no time limit"), "{schema}");
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_Bash() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    assert_eq!(tool.name(), "Bash");
}

#[tokio::test]
async fn test_bash_default_timeout_is_two_minutes() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_timeout_policy(fixed_policy(&[]));
    // 不传 timeout → 默认 120000ms = 2 分钟
    assert_eq!(tool.timeout_policy.foreground_default_ms(), 120_000);
    let result = tool
        .invoke(
            serde_json::json!({"command": "echo ok"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("ok"));
}

#[tokio::test]
async fn test_bash_legacy_params_ignored() {
    // description 是 schema 未声明的字段，残留应被静默忽略（不影响执行）
    // 注：run_in_background 现已支持（见 issue bg-tasks-unified-management），不再是 legacy
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "command": "echo ok",
                "description": "test description",
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("ok"));
}

#[test]
fn test_bash_schema_no_legacy_params() {
    // description 从未声明为 BashTool 参数；run_in_background 现已支持（见 bg-tasks-unified-management）
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let params = tool.parameters();
    let props = params["properties"].as_object().unwrap();
    assert!(
        !props.contains_key("description"),
        "schema 不应声明 description 参数"
    );
    assert!(props.contains_key("command"), "command 应保留");
    assert!(props.contains_key("timeout"), "timeout 应保留");
    assert!(
        props.contains_key("run_in_background"),
        "run_in_background 现已支持，schema 应声明"
    );
}

#[test]
fn test_truncate_output_persists_full_content_on_lines_truncation() {
    let lines: Vec<String> = (0..3000).map(|i| format!("line {}", i)).collect();
    let input = lines.join("\n");
    let result = truncate_output(&input);
    // 落盘提示必须用**模型面名字**引导读取（当前 direct 工具使用原名）：逐字断言注册表的
    // 冻结字面量，不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        result.contains("use `Read` to view complete content"),
        "应包含模型面名字的读取提示: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "应包含临时文件路径: {result}"
    );
}

#[test]
fn test_truncate_output_persists_full_content_on_byte_truncation() {
    let long_line = "x".repeat(200_000);
    let result = truncate_output(&long_line);
    assert!(
        result.contains("use `Read` to view complete content"),
        "字节截断也应持久化并引导读取: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "字节截断应包含文件路径: {result}"
    );
}
