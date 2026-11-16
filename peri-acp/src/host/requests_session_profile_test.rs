//! 会话级 `set_config_option` 的落盘基线：必须是配置权威的最新快照，而不是会话
//! 创建时复制的 `peri_config`。面板与外部编辑只写权威与宿主句柄；会话副本不会
//! 随之更新，拿它当整份写回基线会把这些编辑覆盖回旧值。
use super::super::*;
use crate::host::requests::config_options::handle_update_config;

/// 与用户报告同形：单个 anthropic provider，只有 fable / opus 两个非默认 profile。
fn user_like_config() -> PeriConfig {
    let mut config = PeriConfig::default();
    config.config.active_alias = "opus".into();
    config.config.providers = vec![ProviderConfig {
        id: "anthropic".into(),
        provider_type: "anthropic".into(),
        api_key: "fixture-key".into(),
        base_url: "https://api.anthropic.com".into(),
        models: ProviderModels {
            opus: "claude-opus-5-5".into(),
            sonnet: "claude-sonnet-5-5".into(),
            haiku: "claude-haiku-5-5".into(),
            fable: "claude-fable-5-1".into(),
        },
        ..Default::default()
    }];
    let opus = &mut config.config.profiles.opus;
    opus.provider = "anthropic".into();
    opus.model = Some("claude-opus-5-5".into());
    opus.effort = "medium".into();
    let fable = &mut config.config.profiles.fable;
    fable.provider = "anthropic".into();
    fable.model = Some("claude-fable-5-1".into());
    config
}

struct Host {
    cfg: AcpServerConfig,
    sessions: HashMap<String, SessionState>,
    transport: Arc<dyn crate::transport::AcpTransport>,
    session_id: String,
    startup: PathBuf,
}

impl Host {
    /// 宿主 + 一个经真实 `session/new` 装配出独立 environment 的会话。
    async fn start(tmp: &tempfile::TempDir, config: &PeriConfig) -> Self {
        let startup = tmp.path().join("startup");
        std::fs::create_dir(&startup).unwrap();
        let provider = LlmProvider::from_config(config).unwrap();
        let mut cfg = make_server_config(config.clone(), provider, tmp).await;
        crate::provider::save_to(config, cfg.config_source.global_path()).unwrap();
        cfg.config_source.reload_merged().unwrap();
        cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
            startup_cwd: startup.to_str().unwrap().to_owned(),
            bare: true,
            drive_cron_tick: false,
            mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
            capabilities: Default::default(),
        });
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let mut sessions = HashMap::new();
        let created = handle_request(
            "session/new",
            &json!({"cwd": startup}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let session_id = created["sessionId"].as_str().unwrap().to_owned();
        Self {
            cfg,
            sessions,
            transport,
            session_id,
            startup,
        }
    }

    /// Model 面板的提交帧：完整配置、不带 sessionId（宿主级更新）。
    async fn panel_commit(&mut self, config: &PeriConfig) {
        handle_update_config(
            &json!({"config": config}),
            &self.cfg,
            &mut self.sessions,
            &self.transport,
        )
        .await
        .unwrap();
    }

    /// 状态栏快速切换：会话级 `session/set_config_option`。
    async fn quick_switch(
        &mut self,
        alias: &str,
    ) -> Result<Value, crate::transport::types::AcpError> {
        handle_request(
            "session/set_config_option",
            &json!({"sessionId": self.session_id, "configId": "model", "value": alias}),
            &self.cfg,
            &mut self.sessions,
            &self.transport,
        )
        .await
    }

    fn disk(&self) -> PeriConfig {
        crate::provider::load_from(self.cfg.config_source.global_path()).unwrap()
    }

    /// 该会话下一轮实际发给 Anthropic 的请求体（与每轮 `into_model` 同一条路径）。
    fn request_body(&self, session_id: &str) -> Value {
        let provider = self.sessions[session_id]
            .environment
            .as_ref()
            .unwrap()
            .cfg
            .provider
            .read()
            .clone();
        provider
            .into_model()
            .prepare_request(&peri_model::ModelRequest::default())
            .unwrap()
            .body()
            .as_value()
            .clone()
    }

    async fn status_bar_metadata(&mut self) -> Value {
        handle_request(
            "session/metadata",
            &json!({"sessionId": self.session_id}),
            &self.cfg,
            &mut self.sessions,
            &self.transport,
        )
        .await
        .unwrap()
    }
}

/// 症状 1：Model 面板已落盘的 effort，不能被随后的会话级切换写回旧值。
#[tokio::test]
#[serial]
async fn session_switch_after_panel_edit_does_not_revert_persisted_effort() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config = user_like_config();
    let mut host = Host::start(&tmp, &config).await;

    let mut edited = config.clone();
    edited.config.profiles.opus.effort = "high".into();
    host.panel_commit(&edited).await;
    assert_eq!(host.disk().config.profiles.opus.effort, "high");

    host.quick_switch("sonnet").await.unwrap();
    host.quick_switch("opus").await.unwrap();

    assert_eq!(
        host.disk().config.profiles.opus.effort,
        "high",
        "会话级切换不得把面板已保存的 effort 覆盖回会话副本里的旧值"
    );
}

/// 症状 2：保存后的 effort 进入 opus-5-5 与 fable-5-2 的实际请求体。新建会话直接读
/// 宿主配置；已有会话经一次会话级切换读到权威配置。
#[tokio::test]
#[serial]
async fn saved_effort_reaches_request_body_for_new_models() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let mut config = user_like_config();
    config.config.providers[0].models.fable = "claude-fable-5-2".into();
    config.config.profiles.fable.model = Some("claude-fable-5-2".into());
    let mut host = Host::start(&tmp, &config).await;
    let session_id = host.session_id.clone();
    assert_eq!(
        host.request_body(&session_id)["output_config"],
        json!({"effort": "medium"})
    );

    let mut edited = config.clone();
    edited.config.active_alias = "fable".into();
    edited.config.profiles.opus.effort = "high".into();
    edited.config.profiles.fable.effort = "max".into();
    host.panel_commit(&edited).await;

    let created = handle_request(
        "session/new",
        &json!({"cwd": host.startup}),
        &host.cfg,
        &mut host.sessions,
        &host.transport,
    )
    .await
    .unwrap();
    let fresh = created["sessionId"].as_str().unwrap().to_owned();
    let body = host.request_body(&fresh);
    assert_eq!(body["model"], "claude-fable-5-2");
    assert_eq!(body["output_config"], json!({"effort": "max"}));

    host.quick_switch("opus").await.unwrap();
    let body = host.request_body(&session_id);
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"], json!({"effort": "high"}));

    host.quick_switch("fable").await.unwrap();
    let body = host.request_body(&session_id);
    assert_eq!(body["model"], "claude-fable-5-2");
    assert_eq!(body["output_config"], json!({"effort": "max"}));
}

/// 症状 3：用户把 fable 手动改成 `claude-fable-5-2`（预设只有 5-1）并让 Peri 重新载入后，
/// 选 fable 必须切到 5-2：状态栏元数据与请求体一致，文件里的手改不被会话旧副本覆盖。
#[tokio::test]
#[serial]
async fn switching_to_hand_edited_fable_model_uses_authority_not_session_copy() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config = user_like_config();
    let mut host = Host::start(&tmp, &config).await;
    let session_id = host.session_id.clone();

    let mut hand_edited = config.clone();
    hand_edited.config.providers[0].models.fable = "claude-fable-5-2".into();
    hand_edited.config.profiles.fable.model = Some("claude-fable-5-2".into());
    crate::provider::save_to(&hand_edited, host.cfg.config_source.global_path()).unwrap();
    // 打开 Model 面板时的显式重载。
    host.cfg.config_source.reload_merged().unwrap();

    host.quick_switch("fable").await.unwrap();

    let metadata = host.status_bar_metadata().await;
    assert_eq!(metadata["modelAlias"], "fable");
    assert_eq!(metadata["modelName"], "claude-fable-5-2");
    assert_eq!(host.request_body(&session_id)["model"], "claude-fable-5-2");
    let disk = host.disk();
    assert_eq!(disk.config.active_alias, "fable");
    assert_eq!(
        disk.config.profiles.fable.model.as_deref(),
        Some("claude-fable-5-2"),
        "手改的模型名不得被会话副本写回 5-1"
    );
}
