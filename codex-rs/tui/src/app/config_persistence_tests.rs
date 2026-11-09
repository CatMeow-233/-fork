use std::sync::Arc;

use super::*;
use crate::app::test_support::app_enabled_in_effective_config;
use crate::app::test_support::make_test_app;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::test_support::PathBufExt;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerStack;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffortPreset;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn update_reasoning_effort_updates_collaboration_mode() {
    let mut app = make_test_app().await;
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

    app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        app.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn conversation_reasoning_uses_compatible_default_for_new_threads() {
    for (configured_effort, expected_default_effort) in [
        (ReasoningEffortConfig::Low, ReasoningEffortConfig::Low),
        (
            ReasoningEffortConfig::Custom("unsupported".to_string()),
            ReasoningEffortConfig::Medium,
        ),
        (ReasoningEffortConfig::Ultra, ReasoningEffortConfig::Medium),
    ] {
        let mut app = make_test_app().await;
        app.config.model = Some("gpt-5.5".to_string());
        app.config.model_reasoning_effort = Some(configured_effort.clone());
        app.chat_widget
            .set_reasoning_effort(Some(configured_effort));

        let default_effort =
            app.on_apply_advanced_reasoning("gpt-5.5", ReasoningEffortConfig::Ultra);
        let new_thread_config = &app.config;

        assert_eq!(default_effort, Some(expected_default_effort.clone()));
        assert_eq!(app.chat_widget.current_model(), "gpt-5.5");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Ultra)
        );
        assert_eq!(
            (
                new_thread_config.model.as_deref(),
                new_thread_config.model_reasoning_effort.clone(),
            ),
            (Some("gpt-5.5"), Some(expected_default_effort))
        );
    }
}

#[tokio::test]
async fn conversation_reasoning_keeps_previous_default_for_ultra_only_model() {
    let mut app = make_test_app().await;
    app.config.model = Some("gpt-5.5".to_string());
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
    let mut preset = app
        .model_catalog
        .try_list_models()
        .expect("model catalog is infallible")
        .into_iter()
        .find(|preset| preset.model == "gpt-5.5")
        .expect("gpt-5.5 preset");
    preset.model = "ultra-only".to_string();
    preset.default_reasoning_effort = ReasoningEffortConfig::Ultra;
    preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::Ultra,
        description: "Ultra reasoning".to_string(),
    }];
    app.model_catalog = Arc::new(ModelCatalog::new(vec![preset]));

    let default_effort =
        app.on_apply_advanced_reasoning("ultra-only", ReasoningEffortConfig::Ultra);
    let new_thread_config = &app.config;

    assert_eq!(default_effort, None);
    assert_eq!(app.chat_widget.current_model(), "ultra-only");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Ultra)
    );
    assert_eq!(
        (
            new_thread_config.model.as_deref(),
            new_thread_config.model_reasoning_effort.clone(),
        ),
        (Some("gpt-5.5"), Some(ReasoningEffortConfig::Low))
    );
}

#[tokio::test]
async fn conversation_reasoning_updates_active_plan_without_changing_plan_default() {
    let mut app = make_test_app().await;
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
    app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::High);
    app.chat_widget
        .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    app.chat_widget
        .set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::BackTab));

    let default_effort = app.on_apply_advanced_reasoning("gpt-5.5", ReasoningEffortConfig::Ultra);

    assert_eq!(default_effort, Some(ReasoningEffortConfig::Low));
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Ultra)
    );
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Ultra)
    );
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Ultra)
    );
    assert_eq!(
        (
            app.config.model_reasoning_effort.clone(),
            app.config.plan_mode_reasoning_effort.clone(),
        ),
        (
            Some(ReasoningEffortConfig::Low),
            Some(ReasoningEffortConfig::High),
        )
    );
}

#[tokio::test]
async fn leaving_conversation_ultra_in_default_clears_the_ephemeral_plan_effort() {
    let mut app = make_test_app().await;
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
    app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::High);
    app.chat_widget
        .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    app.chat_widget
        .set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::High));

    app.on_apply_advanced_reasoning("gpt-5.4", ReasoningEffortConfig::Ultra);
    app.on_update_reasoning_effort(Some(ReasoningEffortConfig::Medium));
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::BackTab));

    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        app.config.plan_mode_reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn leaving_conversation_ultra_in_plan_clears_the_ephemeral_default_effort() {
    let mut app = make_test_app().await;
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
    app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::High);
    app.chat_widget
        .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    app.chat_widget
        .set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::BackTab));

    app.on_apply_advanced_reasoning("gpt-5.4", ReasoningEffortConfig::Ultra);
    app.on_update_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::Medium));
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::BackTab));

    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        app.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::Low)
    );
}

#[tokio::test]
async fn resume_model_settings_preserves_only_explicit_model_overrides() {
    let mut app = make_test_app().await;

    assert_eq!(
        app.resume_model_settings(),
        crate::app_server_session::ResumeModelSettings::RestoreFromThread
    );
    let profile_path = test_path_buf("/tmp/work.config.toml").abs();
    let profile = "work"
        .parse::<codex_config::ProfileV2Name>()
        .expect("valid profile name");
    for (key, expected) in [
        (
            "model",
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
        ),
        (
            "model_provider",
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
        ),
        (
            "model_reasoning_effort",
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
        ),
        (
            "sandbox_mode",
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        ),
    ] {
        let config = TomlValue::Table(toml::map::Map::from_iter([(
            key.to_string(),
            TomlValue::String("value".to_string()),
        )]));
        app.config.config_layer_stack = ConfigLayerStack::new(
            vec![ConfigLayerEntry::new(
                ConfigLayerSource::SessionFlags,
                config.clone(),
            )],
            Default::default(),
            Default::default(),
        )
        .expect("session flags layer stack");
        assert_eq!(app.resume_model_settings(), expected);

        app.config.config_layer_stack = ConfigLayerStack::default()
            .with_user_config_profile(&profile_path, Some(&profile), config)
            .expect("user config profile layer stack");
        assert_eq!(app.resume_model_settings(), expected);
    }

    app.config.config_layer_stack = ConfigLayerStack::default()
        .with_user_config(
            &profile_path,
            TomlValue::Table(toml::map::Map::from_iter([(
                "model_reasoning_effort".to_string(),
                TomlValue::String("high".to_string()),
            )])),
        )
        .expect("user config layer stack");
    assert_eq!(
        app.resume_model_settings(),
        crate::app_server_session::ResumeModelSettings::RestoreFromThread
    );

    app.harness_overrides.model_provider = Some("custom-provider".to_string());
    assert_eq!(
        app.resume_model_settings(),
        crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig
    );
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_loads_latest_apps_state() -> Result<()> {
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let app_id = "unit_test_refresh_in_memory_config_connector".to_string();

    assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

    ConfigEditsBuilder::for_config(&app.config)
        .with_edits([
            ConfigEdit::SetPath {
                segments: vec!["apps".to_string(), app_id.clone(), "enabled".to_string()],
                value: false.into(),
            },
            ConfigEdit::SetPath {
                segments: vec![
                    "apps".to_string(),
                    app_id.clone(),
                    "disabled_reason".to_string(),
                ],
                value: "user".into(),
            },
        ])
        .apply()
        .await
        .expect("persist app toggle");

    assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

    app.refresh_in_memory_config_from_disk().await?;

    assert_eq!(
        app_enabled_in_effective_config(&app.config, &app_id),
        Some(false)
    );
    Ok(())
}

// Regression coverage for `/new` and `/clear`: cloud requirements
// must survive the config refresh that runs before thread transitions.
#[tokio::test]
async fn refresh_in_memory_config_from_disk_keeps_cloud_requirements_for_thread_transitions()
-> Result<()> {
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    let required_policy = codex_protocol::protocol::AskForApproval::Never;
    let cloud_config_bundle =
        codex_config::test_support::CloudConfigBundleFixture::loader_with_enterprise_requirement(
            r#"allowed_approval_policies = ["never"]"#,
        );

    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .cloud_config_bundle(cloud_config_bundle.clone())
        .build()
        .await?;
    app.config = config;
    app.cloud_config_bundle = cloud_config_bundle;
    let app_id = "unit_test_cloud_requirements_reload_marker";
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"
[apps.{app_id}]
enabled = false
"#
        ),
    )?;

    let assert_cloud_requirements = |app: &App| {
        let config = &app.config;
        assert_eq!(
            config
                .config_layer_stack
                .requirements_toml()
                .allowed_approval_policies
                .clone(),
            Some(vec![required_policy])
        );
        assert_eq!(config.permissions.approval_policy.value(), required_policy);
    };

    assert_cloud_requirements(&app);
    assert_eq!(app_enabled_in_effective_config(&app.config, app_id), None);

    // This is the fallible reload that the best-effort `/new`, `/clear`,
    // `/fork`, side-conversation, and session-picker paths wrap.
    app.refresh_in_memory_config_from_disk().await?;

    assert_eq!(
        app_enabled_in_effective_config(&app.config, app_id),
        Some(false)
    );
    assert_cloud_requirements(&app);
    Ok(())
}

#[tokio::test]
async fn new_session_and_config_rebuilds_keep_the_live_application_network_policy() -> Result<()> {
    use codex_http_client::ClientRouteClass;
    use codex_http_client::DestinationPolicy;
    use codex_http_client::HttpError;
    use codex_http_client::NetworkPolicyController;
    use codex_http_client::NetworkPolicyDenied;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;

    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let controller = NetworkPolicyController::default();
    let denied = DestinationPolicy::Restricted {
        allowed_hosts: Default::default(),
    };
    assert!(controller.publish(controller.policy().revision(), denied.clone()));
    app.config.application_network_policy = controller.policy();
    let app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    app.refresh_in_memory_config_from_disk().await?;
    let (new_config, _) = app.load_new_session_config(&app_server).await?;
    let permission_config = app
        .rebuild_config_for_permission_profile(":workspace")
        .await?;
    let pet_url =
        "https://persistent.oaistatic.com/codex/pets/v1/dewey-spritesheet-v4.webp".parse()?;
    for config in [&app.config, &new_config, &permission_config] {
        assert_eq!(config.application_network_policy, controller.policy());
        assert_eq!(
            config
                .http_client_factory()
                .network_policy()
                .acquire(&pet_url)
                .map(|_| ()),
            Err(NetworkPolicyDenied::Destination),
        );
    }
    let client = new_config
        .http_client_factory()
        .build_client(&server.uri(), ClientRouteClass::Other)?;
    assert!(matches!(
        client.get(server.uri()).send().await,
        Err(HttpError::Policy(NetworkPolicyDenied::Destination))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());

    assert!(controller.publish(
        controller.policy().revision(),
        DestinationPolicy::Unrestricted
    ));
    assert!(client.get(server.uri()).send().await?.status().is_success());
    assert!(controller.publish(controller.policy().revision(), denied));
    assert!(matches!(
        client.get(server.uri()).send().await,
        Err(HttpError::Policy(NetworkPolicyDenied::Destination))
    ));
    server.verify().await;
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_best_effort_keeps_current_config_on_error() -> Result<()>
{
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
    let original_config = app.config.clone();

    app.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
        .await;

    assert_eq!(app.config, original_config);
    Ok(())
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_uses_active_chat_widget_cwd() -> Result<()> {
    let mut app = make_test_app().await;
    let original_cwd = app.config.cwd.clone();
    let next_cwd_tmp = tempdir()?;
    let next_cwd = next_cwd_tmp.path().to_path_buf();

    app.chat_widget
        .handle_thread_session(crate::session_state::ThreadSessionState {
            daybreak_enabled: false,
            windows_sandbox_host: crate::app::WindowsSandboxHost::Local,
            thread_id: ThreadId::new(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: next_cwd.clone().abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            message_history: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        });

    assert_eq!(app.chat_widget.config_ref().cwd.to_path_buf(), next_cwd);
    assert_eq!(app.config.cwd, original_cwd);

    std::fs::write(app.config.codex_home.join("config.toml"), "")?;
    crate::legacy_core::config::set_project_trust_level(
        &app.config.codex_home,
        &next_cwd,
        codex_protocol::config_types::TrustLevel::Trusted,
    )
    .map_err(std::io::Error::other)?;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let target = SessionTarget {
        backend_id: server.backend_id(),
        thread_id: ThreadId::new(),
        path: None,
        cwd: None,
        history_mode: None,
    };
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let candidate = app
        .resume_config_for_target(
            &mut tui,
            &mut server,
            &target,
            super::resume_preparation::ResumeSource::Active,
        )
        .await
        .map_err(|_| color_eyre::eyre::eyre!("resume configuration should succeed"))?;
    assert_eq!(candidate.0.cwd.to_path_buf(), next_cwd);
    assert_eq!(app.config.cwd.to_path_buf(), next_cwd);
    server.shutdown().await?;

    app.refresh_in_memory_config_from_disk().await?;

    assert_eq!(app.config.cwd, app.chat_widget.config_ref().cwd);
    Ok(())
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_updates_live_widget_preferences() -> Result<()> {
    let mut app = make_test_app().await;
    let mut expected_widget_settings = app.chat_widget.local_settings.clone();
    expected_widget_settings.tui.terminal_resize_reflow_max_rows = Some(9000);
    expected_widget_settings.tui.session_picker_view =
        Some(codex_config::types::SessionPickerViewMode::Comfortable);
    expected_widget_settings.tui.resume_cwd = Some(codex_config::types::ResumeCwdMode::Current);
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
[tui]
terminal_resize_reflow_max_rows = 9000
session_picker_view = "comfortable"
resume_cwd = "current"
theme = "dracula"
"#,
    )?;

    app.refresh_in_memory_config_from_disk().await?;

    assert_eq!(
        app.local_settings.terminal_resize_reflow().max_rows,
        crate::legacy_core::config::TerminalResizeReflowMaxRows::Limit(9000)
    );
    assert_eq!(
        app.local_settings.tui.session_picker_view,
        Some(codex_config::types::SessionPickerViewMode::Comfortable)
    );
    assert_eq!(app.local_settings.tui.theme.as_deref(), Some("dracula"));
    assert_eq!(app.chat_widget.local_settings, expected_widget_settings);
    Ok(())
}

#[tokio::test]
async fn overridden_disabled_guardian_does_not_apply_auto_review_companions() -> Result<()> {
    let mut app = make_test_app().await;
    let original_policy = app.config.permissions.approval_policy.value();
    let effective_config: ConfigReadResponse = serde_json::from_value(serde_json::json!({
        "config": {
            "approval_policy": AskForApproval::OnRequest,
            "approvals_reviewer": codex_app_server_protocol::ApprovalsReviewer::AutoReview,
            "sandbox_mode": AppServerSandboxMode::WorkspaceWrite,
            "features": {
                "guardian_approval": false,
            },
        },
        "origins": {},
    }))?;

    app.sync_feature_state_from_effective_config(
        &effective_config,
        &[(Feature::GuardianApproval, /*enabled*/ true)],
    );

    assert!(!app.config.features.enabled(Feature::GuardianApproval));
    assert!(
        !app.chat_widget
            .config_ref()
            .features
            .enabled(Feature::GuardianApproval)
    );
    assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        ApprovalsReviewer::User
    );
    assert_eq!(
        app.config.permissions.approval_policy.value(),
        original_policy
    );
    Ok(())
}

#[tokio::test]
async fn new_thread_keeps_live_settings_after_failed_reload() -> Result<()> {
    let mut app = make_test_app().await;
    let server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let home = tempdir()?;
    app.config.codex_home = home.path().to_path_buf().abs();
    let config_path = home.path().join("config.toml");
    std::fs::write(&config_path, "[tui]\ntheme = 'dracula'\n")?;
    app.refresh_in_memory_config_from_disk().await?;
    // A live setting can be newer than the last successful disk load.
    app.local_settings.tui.rendering.math = false;
    std::fs::write(config_path, "[broken")?;

    let (_, settings) = app.load_new_session_config(&server).await?;
    assert_eq!(settings, app.local_settings);
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn rebuild_config_for_resume_or_fallback_uses_current_config_on_same_cwd_error() -> Result<()>
{
    let mut app = make_test_app().await;
    app.sync_tui_theme_selection("dracula".to_string());
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
    let current_config = app.config.clone();
    let current_cwd = current_config.cwd.clone();

    let resume_config = app
        .rebuild_config_for_resume_or_fallback(
            &current_cwd,
            current_cwd.to_path_buf(),
            (&app.config, &app.local_settings),
        )
        .await?;

    assert_eq!(resume_config, (current_config, app.local_settings.clone()));
    Ok(())
}

#[tokio::test]
async fn rebuild_config_for_resume_or_fallback_errors_when_cwd_changes() -> Result<()> {
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
    let current_cwd = app.config.cwd.clone();
    let next_cwd_tmp = tempdir()?;
    let next_cwd = next_cwd_tmp.path().to_path_buf();

    let result = app
        .rebuild_config_for_resume_or_fallback(
            &current_cwd,
            next_cwd,
            (&app.config, &app.local_settings),
        )
        .await;

    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn sync_tui_theme_selection_updates_chat_widget_config_copy() {
    let mut app = make_test_app().await;

    app.sync_tui_theme_selection("dracula".to_string());

    assert_eq!(app.local_settings.tui.theme.as_deref(), Some("dracula"));
    assert_eq!(
        app.chat_widget.local_settings.tui.theme.as_deref(),
        Some("dracula")
    );
    assert_eq!(
        app.chat_widget.config_panel_tui.theme.as_deref(),
        Some("dracula")
    );
}

#[tokio::test]
async fn replacement_preserves_live_local_settings_and_server_auth_requirement() -> Result<()> {
    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.sync_tui_theme_selection("dracula".to_string());
    app.chat_widget.requires_openai_auth = false;
    crate::markdown_render::preferences::init(Default::default());
    app.local_settings.tui.rendering.math = false;
    let mut legacy_config = app.config.clone();
    legacy_config.tui_theme = Some("nord".to_string());
    legacy_config.model_provider.requires_openai_auth = true;
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        legacy_config,
        /*initial_user_message*/ None,
    );
    let replacement = ChatWidget::new_with_app_event(init);
    assert_eq!(replacement.local_settings, app.local_settings);
    assert!(!replacement.requires_openai_auth);
    app.replace_chat_widget(replacement);
    let source = r"Math: \(x^2\)";
    assert_eq!(
        crate::markdown_render::render_markdown_text(source).to_string(),
        source
    );
    Ok(())
}

#[tokio::test]
async fn sync_tui_pet_selection_updates_chat_widget_config_copy() {
    let mut app = make_test_app().await;

    app.sync_tui_pet_selection("chefito".to_string());

    assert_eq!(app.local_settings.tui.pet.as_deref(), Some("chefito"));
    assert_eq!(
        app.chat_widget.local_settings.tui.pet.as_deref(),
        Some("chefito")
    );
}

#[tokio::test]
async fn sync_tui_pet_disabled_updates_chat_widget_config_copy() {
    let mut app = make_test_app().await;

    app.sync_tui_pet_disabled();

    assert_eq!(
        app.local_settings.tui.pet.as_deref(),
        Some(crate::pets::DISABLED_PET_ID)
    );
    assert_eq!(
        app.chat_widget.local_settings.tui.pet.as_deref(),
        Some(crate::pets::DISABLED_PET_ID)
    );
}
