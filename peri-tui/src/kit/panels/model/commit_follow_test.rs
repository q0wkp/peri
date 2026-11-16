//! 面板提交后当前会话的跟随：顺序、alias 选择与失败提示。
//! 帧序列的逐项断言在 `commit_test.rs` 的各提交用例里；这里只放跟随本身的契约。

use super::*;
use crate::app::App;
use crate::kit::atoms::PERI_CONFIG_HANDLE;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 会话只能在宿主级 `update_config` 的响应返回之后重建：宿主在响应前已保存并更新
/// 权威快照，提前发出会让会话读到旧配置。
#[test]
fn model_commit_session_follows_only_after_host_update_is_answered() {
    isolated_in(
        module_path!(),
        "model_commit_session_follows_only_after_host_update_is_answered",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            edit_field("sonnet".into(), FIELD_EFFORT, true);
            let IncomingMessage::Request { id, method, .. } = fixture.recv().await else {
                panic!("expected update request")
            };
            assert_eq!(method, "session/update_config");
            assert!(
                tokio::time::timeout(Duration::from_millis(300), fixture.server.recv())
                    .await
                    .is_err(),
                "宿主级更新未应答前，会话不得跟随"
            );
            fixture
                .server
                .send_response(id, Ok(json!({})))
                .await
                .unwrap();
            fixture
                .expect_follow(Follow::CurrentSession(SESSION_ALIAS))
                .await;
            fixture.finish().await;
        },
    );
}

/// 字段编辑不改会话档位：沿用宿主报告的会话 alias，而不是面板里的宿主 alias。
#[test]
fn model_commit_field_edit_keeps_the_sessions_own_alias() {
    isolated_in(
        module_path!(),
        "model_commit_field_edit_keeps_the_sessions_own_alias",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            assert_eq!(fixture.config.read().config.active_alias, SESSION_ALIAS);
            edit_field("sonnet".into(), FIELD_EFFORT, true);
            fixture.expect_commit(Follow::CurrentSession("opus")).await;
            fixture.finish().await;
        },
    );
}

/// 没有活跃会话时只有宿主级推送，不发任何会话请求。
#[test]
fn model_commit_without_session_sends_only_host_update() {
    isolated_in(
        module_path!(),
        "model_commit_without_session_sends_only_host_update",
        async {
            let fixture = Fixture::new(false);
            edit_field("sonnet".into(), FIELD_EFFORT, true);
            fixture.expect_notification().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(300), fixture.server.recv())
                    .await
                    .is_err(),
                "无会话时不得发送会话级请求"
            );
            fixture.finish().await;
        },
    );
}

/// 宿主拒绝会话重建（修订冲突、保存失败等）时必须让用户看见，不能只落日志。
#[test]
fn model_commit_session_switch_rejection_is_reported() {
    isolated_in(
        module_path!(),
        "model_commit_session_switch_rejection_is_reported",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            edit_field("sonnet".into(), FIELD_EFFORT, true);
            fixture.expect_request(false).await;
            let IncomingMessage::Request { id, .. } = fixture.recv().await else {
                panic!("expected session metadata request")
            };
            fixture
                .server
                .send_response(id, Ok(json!({"modelAlias": SESSION_ALIAS})))
                .await
                .unwrap();
            let IncomingMessage::Request { id, method, .. } = fixture.recv().await else {
                panic!("expected session model switch")
            };
            assert_eq!(method, "session/set_config_option");
            fixture
                .server
                .send_response(id, Err(AcpError::new(-32603, "Failed to persist config")))
                .await
                .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let message = NOTIFICATION
                        .state()
                        .read()
                        .as_ref()
                        .map(|note| note.message.clone());
                    if let Some(message) = message
                        && message.starts_with("Model switch failed")
                    {
                        return message;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("会话跟随被拒绝必须有可见提示");
            assert!(message.contains("Failed to persist config"), "{message}");
            fixture.finish().await;
        },
    );
}

// ── 端到端：真实宿主 + 真实会话 + 本地 Anthropic 端点 ─────────────────────────

const SSE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
const SSE_SIGNED_THINKING: &str = concat!(
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-1\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
);
const SSE_TEXT_AND_STOP: &str = concat!(
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 回复是否以带签名的 thinking 开头，决定下一轮历史能否满足手动 thinking 的门限。
#[derive(Clone, Copy)]
enum Reply {
    SignedThinking,
    TextOnly,
}

impl Reply {
    fn sse(self) -> String {
        match self {
            Self::SignedThinking => {
                format!("{SSE_START}{SSE_SIGNED_THINKING}{SSE_TEXT_AND_STOP}")
            }
            // 文本块从 index 0 开始：没有 thinking 块的流。
            Self::TextOnly => format!(
                "{SSE_START}{}",
                SSE_TEXT_AND_STOP.replace("\"index\":1", "\"index\":0")
            ),
        }
    }
}

/// 记录每个请求体并回放固定 SSE 回复的本地端点：测试看到的就是线上发出的请求。
struct AnthropicEndpoint {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl AnthropicEndpoint {
    async fn start(reply: Reply) -> Self {
        let reply: Arc<str> = reply.sse().into();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let seen = seen.clone();
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let body = loop {
                        let read = stream.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        let Some(split) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buffer[..split]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buffer.len() >= split + 4 + length {
                            break buffer[split + 4..split + 4 + length].to_vec();
                        }
                    };
                    if let Ok(body) = serde_json::from_slice::<Value>(&body) {
                        seen.lock().unwrap().push(body);
                    }
                    let _ = stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{reply}"
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { url, bodies }
    }

    /// 带该标记、且携带工具定义的主 Agent 请求体（排除标题等辅助模型请求）。
    fn main_request_for(&self, marker: &str) -> Value {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .find(|body| {
                body.to_string().contains(marker)
                    && body["tools"]
                        .as_array()
                        .is_some_and(|tools| !tools.is_empty())
            })
            .cloned()
            .unwrap_or_else(|| panic!("端点没有收到含 {marker} 的主请求"))
    }
}

/// 与用户报告同形的配置，provider 指向本地端点。
fn user_like_config(endpoint: &str) -> PeriConfig {
    let mut config = PeriConfig::default();
    config.config.active_alias = "opus".into();
    config.config.providers = vec![ProviderConfig {
        id: "anthropic".into(),
        provider_type: "anthropic".into(),
        api_key: "fixture-key".into(),
        base_url: endpoint.into(),
        models: ProviderModels {
            opus: "claude-opus-5-5".into(),
            sonnet: "claude-sonnet-5-5".into(),
            haiku: "claude-haiku-5-5".into(),
            fable: "claude-fable-5-2".into(),
        },
        ..Default::default()
    }];
    let opus = &mut config.config.profiles.opus;
    opus.provider = "anthropic".into();
    opus.model = Some("claude-opus-5-5".into());
    opus.effort = "medium".into();
    let fable = &mut config.config.profiles.fable;
    fable.provider = "anthropic".into();
    fable.model = Some("claude-fable-5-2".into());
    fable.effort = "max".into();
    config
}

/// 轮询宿主的会话元数据（状态栏的数据源）直到满足条件。
async fn wait_for_session_metadata(
    client: &AcpTuiClient,
    session: &str,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let metadata = client
                .send_raw_request("session/metadata", json!({"sessionId": session}))
                .await
                .expect("session/metadata");
            if ready(&metadata) {
                return metadata;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("宿主会话没有跟随面板提交")
}

async fn prompt(client: &AcpTuiClient, marker: &str) {
    client
        .prompt_with_response(
            &peri_acp_types::messages::MessageContent::text(marker),
            None,
        )
        .await
        .expect("prompt");
}

/// 面板改 opus 的 effort、再把 alias 切到 fable：运行中的会话下一轮请求体带新 effort，
/// 状态栏元数据和请求体都是 `claude-fable-5-2`（真实 TUI 动作 → 宿主 → 会话 → 线上请求）。
/// `reply` 决定模型回复是否以带签名 thinking 开头：前者下一轮历史满足手动 thinking 的
/// 门限，后者不满足——effort 都必须出现在每一轮请求体里。
async fn panel_edits_reach_the_next_request(reply: Reply) {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: 隔离子进程里只有本用例；在启动任何后台任务前改 HOME，宿主的插件
    // 发现因此不会读取真实的 ~/.claude。
    unsafe { std::env::set_var("HOME", home.path()) };
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::env::set_current_dir(&workspace).unwrap();
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    let endpoint = AnthropicEndpoint::start(reply).await;
    let settings = home.path().join("settings.json");
    crate::config::set_global_config_path(Some(settings.clone()));
    crate::config::save_to(&user_like_config(&endpoint.url), &settings).unwrap();
    let mut app = App::new(
        peri_acp_types::session_store::SessionStoreDeployment::local_path(
            home.path().join("threads.db"),
        ),
    )
    .await
    .unwrap();
    let (client, mut notifications) = crate::launch::attach_acp(&mut app).await.unwrap();
    tokio::spawn(async move { while notifications.recv().await.is_some() {} });
    client.register_ui_commands(&[]).await.unwrap();
    assert!(
        PERI_CONFIG_HANDLE
            .set(app.services.peri_config.clone())
            .is_ok()
    );
    assert!(CONFIG_SOURCE_HANDLE.set(app.config_source.clone()).is_ok());
    assert!(ACP_CLIENT_HANDLE.set(Arc::new(client.clone())).is_ok());
    let session = client
        .new_session(workspace.to_str().unwrap(), None)
        .await
        .unwrap();

    prompt(&client, "marker-baseline").await;
    let body = endpoint.main_request_for("marker-baseline");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"], json!({"effort": "medium"}));

    // 面板：opus 的 effort medium → high。
    edit_field("opus".into(), FIELD_EFFORT, true);
    let metadata = wait_for_session_metadata(&client, &session, |m| m["effort"] == "high").await;
    assert_eq!(metadata["modelAlias"], "opus");
    assert_eq!(metadata["modelName"], "claude-opus-5-5");
    prompt(&client, "marker-effort").await;
    let body = endpoint.main_request_for("marker-effort");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    if matches!(reply, Reply::TextOnly) {
        assert!(
            !body["messages"]
                .to_string()
                .contains("\"type\":\"thinking\""),
            "历史里没有 thinking 块：{}",
            body["messages"]
        );
    }

    // 面板：选 fable（PROFILE_KEYS 第 0 档）。
    switch_active_alias(0);
    let metadata =
        wait_for_session_metadata(&client, &session, |m| m["modelAlias"] == "fable").await;
    assert_eq!(metadata["modelName"], "claude-fable-5-2");
    assert_eq!(metadata["effort"], "max");
    prompt(&client, "marker-fable").await;
    let body = endpoint.main_request_for("marker-fable");
    assert_eq!(body["model"], "claude-fable-5-2");
    assert_eq!(body["output_config"], json!({"effort": "max"}));

    crate::launch::teardown_app(&mut app).await;
}

#[test]
fn model_panel_edits_reach_the_running_sessions_next_request() {
    isolated_in(
        module_path!(),
        "model_panel_edits_reach_the_running_sessions_next_request",
        panel_edits_reach_the_next_request(Reply::SignedThinking),
    );
}

/// 上一轮回复没有 thinking 块：手动 thinking 的门限不满足，thinking 被省略，但面板保存的
/// effort 仍出现在每一轮请求体里。
#[test]
fn model_panel_effort_reaches_next_request_without_thinking_in_history() {
    isolated_in(
        module_path!(),
        "model_panel_effort_reaches_next_request_without_thinking_in_history",
        panel_edits_reach_the_next_request(Reply::TextOnly),
    );
}
