//! 会话是否有人值守：部署入口一次声明、各层按需消费的契约。
//!
//! 值由入口（print、TUI、stdio…）在装配期决定，执行路径只读不改；
//! 当前唯一消费者是 Bash 后台时限规则（`peri-agent` 的 `ShellTimeoutPolicy`）。

/// 会话的值守形态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionAttendance {
    /// 有人在场的会话（终端 TUI 等）。缺省值：未声明的入口按有人值守处理。
    #[default]
    Attended,
    /// 无人值守会话（SDK、CI 等非交互入口）。
    Unattended,
    /// 无人值守，且 prompt 以文本一次性传入（`peri -p`）。
    UnattendedTextPrompt,
}

impl SessionAttendance {
    pub fn is_unattended(self) -> bool {
        !matches!(self, Self::Attended)
    }
}
