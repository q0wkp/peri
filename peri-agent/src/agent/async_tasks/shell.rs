use std::sync::Arc;

use super::registry::BackgroundTaskRegistry;
use crate::agent::events::BackgroundTaskResult;

pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

// ── bg shell 执行链 ──────────────────────────────────────────────────────────

/// 生成 bg shell 任务 id：`shell-{完整 UUID v7}`。
///
/// **禁止截断 UUID**（issue 2026-08-05）：UUID v7 前 48 位是毫秒时间戳，
/// 同一毫秒内生成的前 8 字符必然相同。agent 连续多次 `run_in_background`
/// Bash 调用落在同一毫秒时，截断前缀会导致 task_id 碰撞——registry 覆盖
/// 注册（Started 事件重复、cancel 句柄丢失），且首个 `complete()` 的 retain
/// 清理后其余 `complete()` 因 existed=false 静默跳过，TUI 残留任务条目。
/// 与 bg agent（`bg-{完整 UUID}`）保持一致，用完整 UUID（122 位熵）。
pub fn bg_shell_task_id() -> String {
    format!("shell-{}", uuid::Uuid::now_v7())
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn finalize_bg_shell(
    registry: &Arc<BackgroundTaskRegistry>,
    on_bg_complete: &Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ticket: peri_acp_types::tasks::TaskRegistrationTicket,
    prompt_summary: String,
    success: bool,
    output: String,
    duration_ms: u64,
    timed_out: bool,
    shell_output: Option<peri_acp_types::event::ShellOutput>,
) {
    let task_id = ticket.task_id().to_owned();
    if let Err(error) = registry.validate_ticket(&ticket) {
        tracing::error!(%task_id, %error, "shell result has no registration capability");
        return;
    }
    if ticket.is_settled() {
        return;
    }
    let result = BackgroundTaskResult {
        task_id: task_id.clone(),
        agent_name: "bg-shell".to_string(),
        prompt_summary,
        success,
        // Shell output is projected through the typed file reference below;
        // keep this field short so callback/reminder size is independent of
        // process output volume.
        output: if shell_output.is_some() {
            if success {
                "Shell command completed; read the output files as needed.".into()
            } else {
                "Shell command failed; read the output files as needed.".into()
            }
        } else {
            output
        },
        tool_calls_count: 0,
        duration_ms,
        child_thread_id: None,
        timed_out,
        subagent_failure: None,
        shell_output: shell_output.map(Box::new),
    };
    if let Some(delivery) = on_bg_complete {
        if let Err(error) = registry.settle_completed(&ticket, result, Arc::clone(delivery)) {
            tracing::error!(%task_id, %error, "shell terminal delivery is pending");
        }
        return;
    }
    if registry.claim_completion(&ticket) {
        registry.complete(&ticket, result);
    } else {
        if registry.claim_cancelled_cleanup(&ticket).is_some() {
            registry.mark_registration_settled(&ticket);
            registry.retire_settled_projection(&ticket);
            registry.notify_activity_change();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::finalize_bg_shell;
    use crate::agent::async_tasks::{
        BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle,
    };
    use peri_acp_types::tasks::{BgTaskKind, TaskRegistrationTicket};
    use std::sync::Arc;

    fn registered_shell() -> (Arc<BackgroundTaskRegistry>, TaskRegistrationTicket) {
        let registry = Arc::new(BackgroundTaskRegistry::new());
        let ticket = registry
            .register_with_kind(BackgroundTask {
                id: "shell-panic".into(),
                agent_name: "bg-shell".into(),
                prompt_summary: "test".into(),
                status: BackgroundTaskStatus::Running,
                started_at: std::time::Instant::now(),
                chrono_started_at: chrono::Utc::now(),
                kind: BgTaskKind::Shell,
                cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
                cancel_token: None,
                pid: None,
                output_preview: None,
                agent_inbox: None,
                initiator_session_id: None,
                owner_session_id: None,
                owner_identity: None,
            })
            .expect("register test shell");
        registry.confirm_external_stopped(&ticket);
        (registry, ticket)
    }

    #[test]
    fn callback_panic_retains_registered_shell_until_delivery_retry() {
        let (registry, ticket) = registered_shell();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        registry.set_event_sender(sender, "fixture".into());
        let callback_registry = Arc::clone(&registry);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = Arc::clone(&attempts);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |_, _| {
            assert_eq!(callback_registry.active_count(), 1);
            assert!(matches!(
                callback_registry.cancel("shell-panic"),
                Err(crate::agent::async_tasks::BackgroundRegistryError::TaskCompleting(_))
            ));
            if called.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                panic!("callback failure");
            }
            Ok(())
        });
        finalize_bg_shell(
            &registry,
            &Some(Arc::clone(&callback)),
            ticket.clone(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        assert_eq!(registry.active_count(), 1);
        assert!(matches!(
            events.try_recv().unwrap(),
            peri_acp_types::tasks::BgRegistryEvent::Updated { .. }
        ));
        assert_eq!(registry.retry_pending_deliveries(), 1);
        assert_eq!(registry.active_count(), 0);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        let peri_acp_types::tasks::BgRegistryEvent::Completed { result, .. } =
            events.try_recv().expect("one terminal event")
        else {
            panic!("completion claim must win over cancellation");
        };
        assert!(
            result.success,
            "callback panic must not change process outcome"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn cancelled_shell_reports_owner_cleanup_once_without_second_registry_terminal() {
        let (registry, ticket) = registered_shell();
        let mut changes = registry.subscribe_events();
        registry.cancel("shell-panic").unwrap();
        let called = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_called = Arc::clone(&called);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |result, _| {
            assert!(!result.success);
            callback_called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        });
        finalize_bg_shell(
            &registry,
            &Some(Arc::clone(&callback)),
            ticket.clone(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        finalize_bg_shell(
            &registry,
            &Some(callback),
            ticket.clone(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(registry.active_count(), 0);
        assert!(matches!(
            changes.try_recv().unwrap().event,
            peri_acp_types::tasks::BgRegistryEvent::Cancelled { .. }
        ));
        assert!(changes.try_recv().is_err());
    }
}
