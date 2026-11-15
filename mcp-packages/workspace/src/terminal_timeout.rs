//! Bash 超时相关的模型可见文案：schema 描述与回执说明。
//!
//! 数值与规则只来自 [`ShellTimeoutPolicy`]；这里不判断规则，只把规则结果如实讲给模型，
//! 保证请求值被忽略/改写时模型总能从回执得知。

use peri_agent::agent::async_tasks::{BackgroundTimeout, ShellTimeoutPolicy};

/// 秒数标签：整秒显示 `120s`，否则保留一位小数。
pub(crate) fn seconds_label(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// 前台超时转入后台时回执的开头句（与 Claude Code 同文，秒数为实际生效值）。
pub(crate) fn moved_to_background_headline(timeout_ms: u64) -> String {
    format!(
        "Command did not complete within its {} timeout and was moved to the background",
        seconds_label(timeout_ms)
    )
}

/// 前台 timeout 请求被界到上限时的回执说明（未改写时为空串）。
/// 同步执行没有禁用超时的路径：`0` 与超过上限的请求都按上限执行。
pub(crate) fn foreground_timeout_note(adjusted_from: Option<u64>, ceiling_ms: u64) -> String {
    match adjusted_from {
        None => String::new(),
        Some(0) => format!(
            "\n- Note: `timeout: 0` cannot disable the timeout on the synchronous path; it was \
             treated as the foreground maximum ({ceiling_ms}ms)."
        ),
        Some(requested) => format!(
            "\n- Note: the requested timeout ({requested}ms) exceeds the foreground maximum \
             ({ceiling_ms}ms); synchronous execution is capped there. Use \
             `run_in_background: true` for work that needs longer."
        ),
    }
}

/// 后台任务回执的时限说明：时限、请求值被忽略/改写三种情况都明说（不限时且无请求为空串）。
pub(crate) fn background_timeout_note(
    timeout: &BackgroundTimeout,
    policy: &ShellTimeoutPolicy,
) -> String {
    match (timeout.limit_ms, timeout.requested) {
        (None, None) => String::new(),
        (None, Some(requested)) => format!(
            "\n- Note: `timeout` ({requested}ms) has no effect on background tasks in this \
             session; they run without a time limit until they exit or are stopped."
        ),
        (Some(limit), Some(0)) => format!(
            "\n- Note: `timeout: 0` does not mean no limit here; the default background time \
             limit ({limit}ms) applies."
        ),
        (Some(limit), Some(requested)) => format!(
            "\n- Note: the requested timeout ({requested}ms) exceeds the maximum background \
             time limit; it was capped at {limit}ms."
        ),
        (Some(limit), None) => format!(
            "\n- Time limit: the task is stopped automatically after {limit}ms. Pass a larger \
             `timeout` (up to {}ms) when starting it if it needs longer.",
            policy.background_max_ms().unwrap_or(limit)
        ),
    }
}

/// 前台超时转入后台任务的时限说明（有人值守会话不限时，返回空串）。
pub(crate) fn moved_task_limit_note(policy: &ShellTimeoutPolicy) -> String {
    match policy.moved_to_background_limit_ms() {
        Some(limit) => format!(
            "\n- Time limit: the moved task is stopped automatically {limit}ms after the move (background time limit)."
        ),
        None => String::new(),
    }
}

/// `timeout` 参数的 schema 描述：数值取自当前会话生效的规则。
pub(crate) fn timeout_param_description(policy: &ShellTimeoutPolicy) -> String {
    let mut text = format!(
        "Optional timeout in milliseconds for a foreground command. It defaults to {default}ms \
         and is capped at {ceiling}ms: a larger request runs at the cap, and `timeout: 0` is \
         treated as the cap instead of disabling the timeout. A foreground command that reaches \
         its timeout is not killed: it is moved to the background and the result reports the \
         task id and output location (a command starting with `sleep` is stopped instead). \
         Check the returned status before retrying; do not duplicate a task that is still running.",
        default = policy.foreground_default_ms(),
        ceiling = policy.foreground_ceiling_ms(),
    );
    match (policy.background_default_ms(), policy.background_max_ms()) {
        (Some(default), Some(max)) => text.push_str(&format!(
            " In this session background tasks have a time limit: a command started with \
             `run_in_background: true` is stopped after {default}ms unless `timeout` sets another \
             limit (up to {max}ms), and a foreground command moved to the background gets \
             {default}ms counted from the move."
        )),
        _ => text.push_str(
            " Commands started with `run_in_background: true` have no time limit in this \
             session and ignore `timeout`.",
        ),
    }
    text
}
