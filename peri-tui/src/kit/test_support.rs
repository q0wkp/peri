//! 跨模块复用的测试 guard，仅在 `cfg(test)` 下编译。

use crate::kit::atoms::{ACP_STATE, INPUT_BUFFER, NOTIFICATION, PROGRAM_STATUS, RENDER_HEARTBEAT};
use crate::kit::program_status::ProgramStatus;
use ratatui_kit::prelude::Atom as AtomStatic;

/// 进程级全局 atom 的保存/恢复：设置前提值，Drop 时按逆序原样放回（含 panic）。
///
/// 全局 atom 跨测试共享：读状态的测试靠它拥有「初始为干净」的前提，写状态的测试靠它
/// 不把结果留给后续用例。异步写入（如 notifier 兜底、游离线程）须在 guard 释放前结束，
/// 例如先 `notifier.await`。
pub(crate) struct AtomsGuard(Vec<Box<dyn FnOnce()>>);

impl AtomsGuard {
    pub(crate) fn new() -> Self {
        Self(Vec::new())
    }

    /// 取走并留下 `Default`。
    pub(crate) fn take<T: Default + Send + Sync + 'static>(
        self,
        atom: &'static AtomStatic<T>,
    ) -> Self {
        self.replace(atom, T::default())
    }

    /// 换成前提值（初始值非 `Default` 的 atom 用它）。
    pub(crate) fn replace<T: Send + Sync + 'static>(
        mut self,
        atom: &'static AtomStatic<T>,
        value: T,
    ) -> Self {
        let old = std::mem::replace(&mut *atom.state().write(), value);
        self.0.push(Box::new(move || *atom.state().write() = old));
        self
    }
}

impl Drop for AtomsGuard {
    fn drop(&mut self) {
        while let Some(restore) = self.0.pop() {
            restore();
        }
    }
}

/// 取走并清空全局 NOTIFICATION，Drop 时原样放回（含 panic）。
pub(crate) struct NotificationGuard {
    _atoms: AtomsGuard,
}

impl NotificationGuard {
    pub(crate) fn install() -> Self {
        Self {
            _atoms: AtomsGuard::new().take(&NOTIFICATION),
        }
    }

    /// 当前通知文案；`Notification` 非 `Clone`，断言只需要文本。
    pub(crate) fn message() -> Option<String> {
        NOTIFICATION
            .state()
            .read()
            .as_ref()
            .map(|notice| notice.message.clone())
    }
}

/// notifier 在通知 channel 关闭时的兜底会写这几个 atom：清 loading 与排队输入、提示断连、
/// 触发重渲染，并把程序状态置为 Error——过期的 Error 会让 `publish_turn_end` 不再改回
/// Working。走到该路径的用例用它取走并恢复。
pub(crate) fn notifier_close_atoms() -> AtomsGuard {
    AtomsGuard::new()
        .take(&NOTIFICATION)
        .take(&ACP_STATE)
        .take(&INPUT_BUFFER)
        .replace(&PROGRAM_STATUS, ProgramStatus::Idle)
        .replace(&RENDER_HEARTBEAT, RENDER_HEARTBEAT.get())
}
