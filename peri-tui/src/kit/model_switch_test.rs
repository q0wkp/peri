//! 会话级模型切换被宿主拒绝时，用户必须看到原因（此前只落 `tracing::warn`，状态栏
//! 停在旧模型且没有任何提示，表现为「选了没反应」）。

use super::send;
use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::NOTIFICATION;
use crate::kit::test_support::NotificationGuard;
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};
use serde_json::json;
use serial_test::serial;
use std::time::Duration;

/// 以给定宿主回复发起切换，最多等待 `wait` 观察提示；没有提示返回 `None`。
async fn switch_with_host_reply(
    reply: Result<serde_json::Value, AcpError>,
    wait: Duration,
) -> Option<String> {
    crate::i18n::init(Some("en"));
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("model-session", false);

    send(client.clone(), "fable".into());

    let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
        panic!("模型切换必须向宿主发出请求")
    };
    assert_eq!(method, "session/set_config_option");
    assert_eq!(
        params,
        json!({"sessionId": "model-session", "configId": "model", "value": "fable"})
    );
    server.send_response(id, reply).await.unwrap();
    tokio::time::timeout(wait, async {
        loop {
            if let Some(note) = NOTIFICATION.state().read().as_ref() {
                return Some(note.message.clone());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
#[serial]
async fn rejected_switch_is_reported_to_the_user() {
    let _guard = NotificationGuard::install();
    let message = switch_with_host_reply(
        Err(AcpError::new(-32603, "Failed to persist config")),
        Duration::from_secs(3),
    )
    .await
    .expect("宿主拒绝切换必须有可见提示");
    assert_eq!(
        message,
        "Model switch failed: ACP error [-32603]: Failed to persist config"
    );
}

#[tokio::test]
#[serial]
async fn accepted_switch_stays_silent() {
    let _guard = NotificationGuard::install();
    let message = switch_with_host_reply(Ok(json!({})), Duration::from_millis(300)).await;
    assert_eq!(message, None, "切换成功不得产生失败提示");
}
