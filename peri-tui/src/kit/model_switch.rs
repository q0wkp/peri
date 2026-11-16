//! Session model selection addresses the active session through the ACP lifecycle gate.
//!
//! 与 `permission_mode` 同构：宿主拒绝切换（配置修订冲突、保存失败等）时切换没有发生，
//! 用户必须看见原因，不能只落日志后让状态栏悄悄停在旧模型。

use crate::{acp_client::AcpTuiClient, kit::atoms};
use peri_acp::transport::types::AcpError;

pub(crate) fn send(client: AcpTuiClient, alias: String) {
    tokio::spawn(async move {
        if let Err(error) = client.set_model(&alias).await {
            report(&error, &alias);
        }
    });
}

/// Model 面板提交（宿主级 `update_config` 已被宿主接受）之后，让当前会话按最新权威
/// 配置重建。`activated` 是面板刚激活的 alias，`None` 表示沿用会话自己的 alias。
///
/// 没有会话时不发请求。调用方必须在 `update_config` 的响应返回后再调用：宿主在
/// 响应前已完成保存与快照更新，这里读到的就是面板提交的内容。
pub(crate) async fn follow_panel_commit(client: &AcpTuiClient, activated: Option<&str>) {
    if let Err(error) = client.follow_host_config(activated).await {
        report(&error, activated.unwrap_or("session alias"));
    }
}

fn report(error: &AcpError, alias: &str) {
    tracing::warn!(%error, alias = %alias, "session model switch failed");
    atoms::NOTIFICATION.set(Some(atoms::Notification {
        message: crate::i18n::tr_args(
            "model-switch-failed",
            &[("error".into(), error.to_string().into())],
        ),
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(5),
    }));
}

#[cfg(test)]
#[path = "model_switch_test.rs"]
mod tests;
