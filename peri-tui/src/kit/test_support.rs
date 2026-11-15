//! 跨模块复用的测试 guard，仅在 `cfg(test)` 下编译。

use crate::kit::atoms::{NOTIFICATION, Notification};

/// 取走并清空全局 NOTIFICATION，Drop 时原样放回（含 panic）。
///
/// 通知是进程级单槽：读通知的测试靠它拥有「初始为空」的前提，写通知的测试靠它
/// 不把提示留给后续用例。异步写入（如 notifier 兜底）须在 guard 释放前结束，
/// 例如先 `notifier.await`。
pub(crate) struct NotificationGuard(Option<Notification>);

impl NotificationGuard {
    pub(crate) fn install() -> Self {
        Self(NOTIFICATION.state().write().take())
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        *NOTIFICATION.state().write() = self.0.take();
    }
}
