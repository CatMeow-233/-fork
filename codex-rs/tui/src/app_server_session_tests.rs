use super::*;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use app_test_support::create_fake_paginated_rollout;
use app_test_support::create_fake_rollout;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemsListCursor;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_features::Feature;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::Verbosity;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_READ_ONLY;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::openai_models::ModelAccessPrograms;
use codex_protocol::openai_models::ModelServiceTier;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::turn_input::CyberAccessProgram;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use codex_utils_path_uri::LegacyAppPathString;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

async fn build_config(temp_dir: &TempDir) -> Config {
    ConfigBuilder::default()
        .codex_home(temp_dir.path().to_path_buf())
        .build()
        .await
        .expect("config should build")
}

#[test]
fn picker_views_use_distinct_request_id_namespaces() {
    let (client, _commands, _events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
    let mut session = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        AppServerClient::Cloud(client),
        ThreadParamsMode::Cloud,
    );
    let mut first = session.request_view();
    let mut second = session.request_view();

    let first_id = first.next_request_id();
    assert_ne!(session.next_request_id(), first_id);
    assert_ne!(first_id, second.next_request_id());
}

#[tokio::test]
async fn picker_view_shutdown_preserves_live_event_stream() {
    let (client, _commands, events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
    let backend_id = crate::backend_id::BackendId::new();
    let mut session = AppServerSession::new(
        backend_id,
        AppServerClient::Cloud(client),
        ThreadParamsMode::Cloud,
    );

    session.request_view().shutdown().await.unwrap();
    events
        .send(codex_app_server_client::AppServerEvent::Disconnected {
            message: "still live".to_string(),
        })
        .unwrap();

    let event = session.next_event().await.unwrap();
    assert_eq!(event.backend_id, backend_id);
    assert!(matches!(
        event.event,
        codex_app_server_client::AppServerEvent::Disconnected { .. }
    ));
}

#[tokio::test]
async fn bootstrap_reuses_prefetched_account_without_another_account_read() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let mut config = build_config(&codex_home).await;
    let mut catalog = codex_models_manager::bundled_models_response()?;
    for model in &mut catalog.models {
        model.available_access_programs = Some(ModelAccessPrograms {
            cyber: vec![
                CyberAccessProgram::Standard,
                CyberAccessProgram::DaybreakBlue,
            ],
        });
    }
    config.model_catalog = Some(catalog);
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let next_request_id = app_server.next_request_id;
    let account = GetAccountResponse {
        user_id: None,
        workspace_routing: None,
        account: Some(Account::Chatgpt {
            email: Some("teammate@openai.com".to_string()),
            plan_type: codex_protocol::account::PlanType::Plus,
            workspace_name: None,
        }),
        requires_openai_auth: true,
    };

    let bootstrap = app_server.bootstrap_with_account(&config, account).await?;

    assert_eq!(app_server.next_request_id, next_request_id + 2);
    assert_eq!(
        (
            bootstrap.account_email.as_deref(),
            bootstrap.auth_mode,
            bootstrap.plan_type,
            bootstrap.feedback_audience,
            bootstrap.has_chatgpt_account,
        ),
        (
            Some("teammate@openai.com"),
            Some(TelemetryAuthMode::Chatgpt),
            Some(codex_protocol::account::PlanType::Plus),
            FeedbackAudience::OpenAiEmployee,
            true,
        )
    );

    // Account-specific catalog filtering at startup and before/after thread creation.
    let models = bootstrap.available_models;
    assert!(
        models
            .iter()
            .any(|model| model.available_access_programs.is_some())
    );
    let client_home = tempfile::tempdir()?;
    let key = "features.api_key_cyber_access_programs";
    for (server_enabled, cli_override, forwarding_enabled) in [
        (false, None, false),
        (true, None, true),
        (false, Some(true), true),
        (true, Some(false), false),
    ] {
        std::fs::write(
            codex_home.path().join("config.toml"),
            format!("[features]\napi_key_cyber_access_programs = {server_enabled}\n"),
        )?;
        let client_config = ConfigBuilder::default()
            .codex_home(client_home.path().to_path_buf())
            .cli_overrides(
                cli_override
                    .into_iter()
                    .map(|value| (key.into(), value.into()))
                    .collect(),
            )
            .build()
            .await?;
        let api_key = app_server
            .bootstrap_with_account(
                &client_config,
                GetAccountResponse {
                    user_id: None,
                    account: Some(Account::ApiKey {}),
                    requires_openai_auth: true,
                    workspace_routing: None,
                },
            )
            .await?;
        let mut expected = models.clone();
        if !forwarding_enabled {
            for model in &mut expected {
                model.available_access_programs = None;
            }
        }
        assert_eq!(api_key.available_models, expected);
        let thread = app_server.start_thread(&client_config).await?;
        let refreshed = app_server
            .constrain_api_key_models(
                &client_config,
                Some(thread.session.thread_id),
                /*requirements*/ None,
                models.clone(),
            )
            .await;
        assert_eq!(refreshed, expected);
    }
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn bootstrap_reads_account_when_no_prefetched_account_is_available() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let config = build_config(&codex_home).await;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let next_request_id = app_server.next_request_id;

    app_server.bootstrap(&config).await?;

    assert_eq!(app_server.next_request_id, next_request_id + 3);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn new_thread_bootstrap_reuses_prefetched_account_without_another_account_read() -> Result<()>
{
    let codex_home = tempfile::tempdir()?;
    let config = build_config(&codex_home).await;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let account = app_server.read_account().await?;
    let next_request_id = app_server.next_request_id;

    let reused = app_server
        .bootstrap_for_new_thread_with_account(&config, account)
        .await?;
    assert_eq!(app_server.next_request_id, next_request_id + 2);
    let read = app_server.bootstrap_for_new_thread(&config).await?;
    assert_eq!(app_server.next_request_id, next_request_id + 5);

    assert_eq!(
        (
            reused.account_email,
            reused.auth_mode,
            reused.requires_openai_auth,
            reused.default_model,
            reused.available_models,
        ),
        (
            read.account_email,
            read.auth_mode,
            read.requires_openai_auth,
            read.default_model,
            read.available_models,
        )
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cloud_bootstrap_waits_for_the_thread_to_supply_its_model() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let mut config = build_config(&codex_home).await;
    config.model = Some("local-only-model".to_string());
    let (client, mut commands, _events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 3);
    let mut app_server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        AppServerClient::Cloud(client),
        ThreadParamsMode::Cloud,
    );
    let account = GetAccountResponse {
        user_id: None,
        account: Some(Account::Chatgpt {
            email: Some("teammate@openai.com".to_string()),
            plan_type: codex_protocol::account::PlanType::Plus,
            workspace_name: None,
        }),
        requires_openai_auth: true,
        workspace_routing: None,
    };

    let (bootstrap, ()) =
        tokio::join!(app_server.bootstrap_with_account(&config, account), async {
            for _ in 0..2 {
                let command = commands.recv().await.expect("bootstrap request");
                let codex_app_server_client::CloudOperation::Request(request) = command.operation
                else {
                    panic!("expected bootstrap request")
                };
                let result = match *request {
                    ClientRequest::ConfigRequirementsRead { .. } => Err(JSONRPCErrorError {
                        code: JSONRPC_METHOD_NOT_FOUND,
                        message: "method not found".to_string(),
                        data: None,
                    }),
                    ClientRequest::CollaborationModeList { .. } => {
                        Ok(serde_json::json!({"data": []}))
                    }
                    request => panic!("unexpected bootstrap request: {request:?}"),
                };
                command.response.send(Ok(result)).unwrap();
            }
        });
    let bootstrap = bootstrap?;

    assert_eq!(bootstrap.default_model, None);
    assert!(bootstrap.available_models.is_empty());
    Ok(())
}

#[tokio::test]
async fn bootstrap_empty_catalog_reports_configuration_error() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let mut config = build_config(&codex_home).await;
    config.model = None;
    config.model_catalog = Some(codex_protocol::openai_models::ModelsResponse::default());
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;

    let error = app_server
        .bootstrap(&config)
        .await
        .err()
        .expect("bootstrap requires a model");
    insta::assert_snapshot!(error.to_string(), @"No models are available. Set `model` explicitly or check your model catalog configuration.");

    app_server.shutdown().await?;
    Ok(())
}

fn rate_limit_snapshot(limit_id: &str) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some(limit_id.to_string()),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(codex_app_server_protocol::RateLimitWindow {
            used_percent: 0,
            window_duration_mins: Some(10_080),
            resets_at: None,
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

fn api_model_with_upgrade_retirement_at(retirement_at: Option<i64>) -> ApiModel {
    ApiModel {
        id: "model-id".to_string(),
        model: "current-model".to_string(),
        upgrade: Some("replacement-model".to_string()),
        upgrade_info: Some(codex_app_server_protocol::ModelUpgradeInfo {
            model: "replacement-model".to_string(),
            upgrade_copy: None,
            model_link: None,
            migration_markdown: None,
            retirement_at,
        }),
        availability_nux: None,
        display_name: "Current model".to_string(),
        description: "A test model".to_string(),
        model_specialty: None,
        hidden: false,
        supported_reasoning_efforts: Vec::new(),
        default_reasoning_effort: ReasoningEffort::Medium,
        input_modalities: Vec::new(),
        supports_personality: false,
        multi_agent_version: None,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        available_access_programs: None,
        is_default: false,
    }
}

#[test]
fn model_preset_from_api_model_preserves_upgrade_retirement_at() {
    let retirement_at = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .expect("valid RFC 3339 timestamp")
        .with_timezone(&chrono::Utc);
    let expected_upgrade = |retirement_at| {
        Some(ModelUpgrade {
            id: "replacement-model".to_string(),
            migration_config_key: "current-model".to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
            retirement_at,
        })
    };

    assert_eq!(
        vec![
            model_preset_from_api_model(api_model_with_upgrade_retirement_at(Some(
                retirement_at.timestamp(),
            )))
            .upgrade,
            model_preset_from_api_model(api_model_with_upgrade_retirement_at(
                /*retirement_at*/ None,
            ))
            .upgrade,
            model_preset_from_api_model(api_model_with_upgrade_retirement_at(Some(i64::MAX)))
                .upgrade,
        ],
        vec![
            expected_upgrade(Some(retirement_at)),
            expected_upgrade(None),
            expected_upgrade(None),
        ]
    );
}

#[test]
fn app_server_rate_limit_snapshots_deduplicates_top_level_limit_from_map() {
    let response = GetAccountRateLimitsResponse {
        ordinary_usage_allowed: None,
        account_id: None,
        rate_limit_upsell: None,
        rate_limits: rate_limit_snapshot("codex"),
        rate_limits_by_limit_id: Some(HashMap::from([
            ("codex".to_string(), rate_limit_snapshot("codex")),
            ("other".to_string(), rate_limit_snapshot("other")),
        ])),
        rate_limit_reset_credits: None,
    };

    let snapshots = app_server_rate_limit_snapshots(response);

    assert_eq!(
        snapshots
            .iter()
            .map(|snapshot| snapshot.limit_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("codex"), Some("other")]
    );
}

#[test]
fn thread_settings_update_compat_detects_unsupported_errors() {
    let cases = [
        (JSONRPC_METHOD_NOT_FOUND, "method not found", true),
        (
            JSONRPC_INVALID_REQUEST,
            "thread/settings/update requires experimentalApi capability",
            true,
        ),
        (
            JSONRPC_INVALID_REQUEST,
            "Invalid request: unknown variant `thread/settings/update`",
            true,
        ),
        (JSONRPC_INVALID_REQUEST, "invalid thread id", false),
    ];

    for (code, message, expected) in cases {
        let source = JSONRPCErrorError {
            code,
            data: None,
            message: message.to_string(),
        };
        assert_eq!(
            is_thread_settings_update_unsupported(&source),
            expected,
            "{message}"
        );
    }
}

#[test]
fn history_pagination_compat_detects_unsupported_server_fields() {
    let cases = [
        (JSONRPC_INVALID_PARAMS, "unknown field `historyMode`", true),
        (
            JSONRPC_INVALID_REQUEST,
            "thread/resume.excludeTurns requires experimentalApi capability",
            true,
        ),
        (
            JSONRPC_INVALID_REQUEST,
            "thread/fork.excludeTurns requires experimentalApi capability",
            true,
        ),
        (
            JSONRPC_METHOD_NOT_FOUND,
            "unknown method thread/turns/list",
            true,
        ),
        (JSONRPC_METHOD_NOT_FOUND, "method not found", true),
        (
            JSONRPC_INVALID_PARAMS,
            "unknown variant \"paginated\", expected \"legacy\"",
            true,
        ),
        (
            JSONRPC_INVALID_PARAMS,
            "invalid enum value `paginated`",
            true,
        ),
        (
            JSONRPC_INVALID_PARAMS,
            "paginated thread was not found",
            false,
        ),
        (JSONRPC_INVALID_PARAMS, "invalid thread id", false),
    ];

    for (code, message, expected) in cases {
        let source = JSONRPCErrorError {
            code,
            data: None,
            message: message.to_string(),
        };
        assert_eq!(
            is_history_pagination_unsupported(&source),
            expected,
            "{message}"
        );
    }
}

#[tokio::test]
async fn ephemeral_thread_start_does_not_request_paginated_history() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&temp_dir).await;
    config.ephemeral = true;

    let params = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );

    assert_eq!(params.ephemeral, Some(true));
    assert_eq!(params.history_mode, None);
}

#[tokio::test]
async fn cloud_start_resume_and_turn_leave_settings_server_owned() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&temp_dir).await;
    config.base_instructions = Some("local base instructions".to_string());
    config.developer_instructions = Some("local developer instructions".to_string());
    config.daybreak_enabled = true;
    config.ephemeral = true;
    let thread_id = ThreadId::new();
    let (client, mut commands, _events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
    let request_handle = AppServerClient::Cloud(client).request_handle();
    let mut launch_choices = StartupLaunchChoices::default();
    launch_choices.model = Some("local-model".to_string());
    launch_choices.effort = Some(serde_json::json!("high"));
    let local_settings = LocalSettings::from(&config);

    let (start, sent_params) = tokio::join!(
        start_thread_with_request_handle(
            request_handle,
            &local_settings,
            config.clone(),
            ThreadParamsMode::Cloud,
            Some(std::path::PathBuf::from("/local/workspace")),
            ThreadToolTransport::Dynamic,
            launch_choices,
        ),
        async {
            let command = commands.recv().await.expect("cloud thread start");
            let codex_app_server_client::CloudOperation::Request(request) = command.operation
            else {
                panic!("expected cloud request")
            };
            let ClientRequest::ThreadStart { params, .. } = *request else {
                panic!("expected thread/start request")
            };
            command
                .response
                .send(Ok(Err(JSONRPCErrorError {
                    code: JSONRPC_INVALID_PARAMS,
                    data: None,
                    message: "stop after recording request".to_string(),
                })))
                .expect("startup should still be waiting for its response");
            params
        },
    );
    let _ = start.expect_err("recording response should stop startup");
    let resume = thread_resume_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Cloud,
        Some(std::path::Path::new("/local/workspace")),
        ResumeModelSettings::OverrideFromCurrentConfig,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );

    assert_eq!(sent_params, ThreadStartParams::default());
    assert_eq!(
        resume,
        ThreadResumeParams {
            thread_id: thread_id.to_string(),
            ..ThreadResumeParams::default()
        }
    );

    let (client, mut commands, _events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
    let mut app_server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        AppServerClient::Cloud(client),
        ThreadParamsMode::Cloud,
    );
    let client_user_message_id = "local-message-id".to_string();
    let input = vec![UserInput::Text {
        text: "hello from the TUI".to_string(),
        text_elements: Vec::new(),
    }];
    let output_schema = serde_json::json!({"type": "string"});
    let workspace_roots = vec![test_path_buf("/local/workspace").abs()];
    let (turn, sent_params) = tokio::join!(
        app_server.turn_start(
            thread_id,
            "user",
            client_user_message_id.clone(),
            input.clone(),
            test_path_buf("/local/workspace"),
            Some(codex_app_server_protocol::AskForApproval::Never),
            Some(codex_app_server_protocol::ApprovalsReviewer::User),
            TurnPermissionsOverride::ActiveProfile(ActivePermissionProfile::new(
                "local-permission-profile".to_string(),
            )),
            &workspace_roots,
            "local-model".to_string(),
            Some(ReasoningEffort::High),
            Some(ReasoningSummary::Detailed),
            Some(Some("local-service-tier".to_string())),
            Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Plan,
                settings: codex_protocol::config_types::Settings {
                    model: "local-plan-model".to_string(),
                    reasoning_effort: Some(ReasoningEffort::High),
                    developer_instructions: Some("local plan instructions".to_string()),
                },
            }),
            Some(output_schema.clone()),
            Some(CyberAccessProgram::DaybreakBlue.into()),
        ),
        async {
            let command = commands.recv().await.expect("cloud turn start");
            let codex_app_server_client::CloudOperation::Request(request) = command.operation
            else {
                panic!("expected cloud request")
            };
            let ClientRequest::TurnStart { params, .. } = *request else {
                panic!("expected turn/start request")
            };
            command
                .response
                .send(Ok(Err(JSONRPCErrorError {
                    code: JSONRPC_INVALID_PARAMS,
                    data: None,
                    message: "stop after recording request".to_string(),
                })))
                .expect("turn start should still be waiting for its response");
            params
        },
    );
    let _ = turn.expect_err("recording response should stop turn start");
    assert_eq!(
        sent_params,
        TurnStartParams {
            thread_id: thread_id.to_string(),
            client_user_message_id: Some(client_user_message_id),
            input,
            turn_trigger: Some("user".to_string()),
            output_schema: Some(output_schema),
            cyber_access_program: Some(CyberAccessProgram::DaybreakBlue.into()),
            ..TurnStartParams::default()
        }
    );
}

#[tokio::test]
async fn shared_thread_start_preserves_explicit_session_overrides() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let workspace = codex_home.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "sandbox_mode = \"workspace-write\"\n[sandbox_workspace_write]\nnetwork_access = true\n",
    )?;
    let server_config = build_config(&codex_home).await;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(workspace.clone()),
            ..ConfigOverrides::default()
        })
        .cli_overrides(vec![
            (
                "suppress_unstable_features_warning".to_string(),
                toml::Value::Boolean(true),
            ),
            (
                "features.multi_agent_mode".to_string(),
                toml::Value::Boolean(true),
            ),
            (
                "sandbox_workspace_write.network_access".to_string(),
                toml::Value::Boolean(false),
            ),
            (
                "instructions".to_string(),
                toml::Value::String("unsafe ".repeat(10_000)),
            ),
            ("model".to_string(), "gpt-5".into()),
            ("approval_policy".to_string(), "never".into()),
        ])
        .build()
        .await?;

    let params = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Remote,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );

    let overrides = params.config.expect("config overrides");
    assert_eq!(
        (
            overrides.get("features").cloned(),
            overrides.get("sandbox_workspace_write").cloned(),
            overrides.get("instructions").cloned(),
            overrides.get("suppress_unstable_features_warning").cloned(),
        ),
        (
            Some(serde_json::json!({ "multi_agent_mode": true })),
            Some(serde_json::json!({ "network_access": false })),
            None,
            Some(serde_json::json!(true)),
        )
    );
    for mode in [ThreadParamsMode::Embedded, ThreadParamsMode::Remote] {
        let mut app_server = crate::start_embedded_app_server_for_picker(&server_config).await?;
        app_server.thread_params_mode = mode;
        app_server.remote_cwd_override = Some(workspace.clone());

        let started = app_server.start_thread(&config).await?;

        assert_eq!(
            (
                started.session.permission_profile.network_sandbox_policy(),
                started.session.model.as_str(),
                started.session.approval_policy,
                started.session.cwd.as_path(),
            ),
            (
                NetworkSandboxPolicy::Restricted,
                "gpt-5",
                AskForApproval::Never,
                workspace.as_path(),
            )
        );
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn thread_start_params_include_cwd_for_embedded_sessions() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = ConfigBuilder::default()
        .codex_home(temp_dir.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
            ..ConfigOverrides::default()
        })
        .build()
        .await
        .expect("config should build");

    let params = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );

    assert_eq!(params.cwd, Some(config.cwd.to_string_lossy().to_string()));
    assert_eq!(
        params.runtime_workspace_roots,
        Some(config.workspace_roots.clone())
    );
    assert_eq!(params.sandbox, None);
    assert_eq!(
        params.permissions,
        config
            .permissions
            .active_permission_profile()
            .map(permission_profile_id_from_active_profile)
    );
    assert_eq!(params.model_provider, None);
    assert_eq!(params.thread_source, Some(ThreadSource::User));
    assert_eq!(params.dynamic_tools, None);
}

#[tokio::test]
async fn thread_start_params_can_mark_clear_source() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;

    let params = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        Some(ThreadStartSource::Clear),
    );

    assert_eq!(params.session_start_source, Some(ThreadStartSource::Clear));
}

#[test]
fn embedded_turn_permissions_use_active_profile_selection() {
    let cwd = test_path_buf("/workspace/project").abs();
    let active_permission_profile =
        ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE);
    let expected_permissions =
        permission_profile_id_from_active_profile(active_permission_profile.clone());

    let (sandbox_policy, permissions) = turn_permissions_overrides(
        TurnPermissionsOverride::ActiveProfile(active_permission_profile),
        cwd.as_path(),
    )
    .expect("active permission profile should be supported");

    assert_eq!(sandbox_policy, None);
    assert_eq!(permissions, Some(expected_permissions));
}

#[test]
fn embedded_turn_permissions_select_profile_id_only() {
    let cwd = test_path_buf("/workspace/project").abs();
    let active_permission_profile =
        ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE);

    let (sandbox_policy, permissions) = turn_permissions_overrides(
        TurnPermissionsOverride::ActiveProfile(active_permission_profile),
        cwd.as_path(),
    )
    .expect("active permission profile should be supported");

    assert_eq!(sandbox_policy, None);
    assert_eq!(
        permissions,
        Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string())
    );
}

#[test]
fn turn_permissions_preserve_thread_permissions_without_override() {
    let cwd = test_path_buf("/workspace/project").abs();

    let (sandbox_policy, permissions) =
        turn_permissions_overrides(TurnPermissionsOverride::Preserve, cwd.as_path())
            .expect("preserving permissions should be supported");

    assert_eq!(sandbox_policy, None);
    assert_eq!(permissions, None);
}

#[test]
fn legacy_turn_permissions_project_to_sandbox_when_explicitly_overridden() {
    let cwd = test_path_buf("/workspace/project").abs();

    let (sandbox_policy, permissions) = turn_permissions_overrides(
        TurnPermissionsOverride::LegacySandbox(PermissionProfile::read_only()),
        cwd.as_path(),
    )
    .expect("read-only permission profile should be supported");

    assert_eq!(
        sandbox_policy,
        Some(codex_app_server_protocol::SandboxPolicy::ReadOnly {
            network_access: false
        })
    );
    assert_eq!(permissions, None);
}

#[test]
fn legacy_turn_permissions_preserve_workspace_write() {
    let cwd = test_path_buf("/workspace/project").abs();

    let (sandbox_policy, permissions) = turn_permissions_overrides(
        TurnPermissionsOverride::LegacySandbox(PermissionProfile::workspace_write()),
        cwd.as_path(),
    )
    .expect("workspace-write permission profile should be supported");

    assert_eq!(
        sandbox_policy,
        Some(codex_app_server_protocol::SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        })
    );
    assert_eq!(permissions, None);
}

#[test]
fn legacy_turn_permissions_reject_non_cwd_write_roots() {
    let cwd = test_path_buf("/workspace/project").abs();
    let extra_root = test_path_buf("/workspace/extra").abs();
    let permission_profile = PermissionProfile::Managed {
        network: NetworkSandboxPolicy::Restricted,
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Path {
                        path: extra_root.into(),
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
            ],
            glob_scan_max_depth: None,
        },
    };

    let error = turn_permissions_overrides(
        TurnPermissionsOverride::LegacySandbox(permission_profile),
        cwd.as_path(),
    )
    .expect_err("non-cwd write roots must not grant cwd write access");

    assert_eq!(
        error.to_string(),
        "the selected permission profile cannot be safely represented by the legacy app-server sandbox policy; select a named or legacy-compatible permission profile"
    );
}

#[test]
fn legacy_turn_permissions_reject_restrictions_lost_by_projection() {
    let cwd = test_path_buf("/workspace/project").abs();
    let docs = test_path_buf("/workspace/project/docs").abs();

    for (path, access) in [
        (
            FileSystemPath::Path {
                path: docs.clone().into(),
            },
            FileSystemAccessMode::Read,
        ),
        (
            FileSystemPath::Path { path: docs.into() },
            FileSystemAccessMode::Deny,
        ),
        (
            FileSystemPath::GlobPattern {
                pattern: "**/*.secret".to_string(),
            },
            FileSystemAccessMode::Deny,
        ),
    ] {
        let mut permission_profile = PermissionProfile::workspace_write();
        let PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
            ..
        } = &mut permission_profile
        else {
            unreachable!("workspace-write profiles use restricted managed permissions");
        };
        entries.push(FileSystemSandboxEntry {
            path,
            access,
            missing_path_behavior: None,
        });

        let error = turn_permissions_overrides(
            TurnPermissionsOverride::LegacySandbox(permission_profile),
            cwd.as_path(),
        )
        .expect_err("legacy projection must not discard filesystem restrictions");

        assert!(
            error
                .downcast_ref::<UnsupportedLegacyPermissionProfile>()
                .is_some()
        );
    }
}

#[test]
fn remote_turn_permissions_preserve_active_profile_selection() {
    let cwd = test_path_buf("/workspace/project").abs();
    let active_permission_profile = ActivePermissionProfile::new("strict");
    let expected_permissions =
        permission_profile_id_from_active_profile(active_permission_profile.clone());

    let (sandbox_policy, permissions) = turn_permissions_overrides(
        TurnPermissionsOverride::ActiveProfile(active_permission_profile),
        cwd.as_path(),
    )
    .expect("active permission profile should be supported");

    assert_eq!(sandbox_policy, None);
    assert_eq!(permissions, Some(expected_permissions));
}

#[tokio::test]
async fn thread_lifecycle_params_omit_local_paths_for_remote_sessions() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();
    let expected_sandbox = sandbox_mode_from_permission_profile(
        &config.permissions.effective_permission_profile(),
        config.cwd.as_path(),
    );

    let start = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Remote,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );
    let resume = thread_resume_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Remote,
        /*remote_cwd_override*/ None,
        ResumeModelSettings::OverrideFromCurrentConfig,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );
    let fork = thread_fork_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Remote,
        /*remote_cwd_override*/ None,
    );

    assert_eq!(start.cwd, None);
    assert_eq!(resume.cwd, None);
    assert_eq!(fork.cwd, None);
    assert_eq!(start.runtime_workspace_roots, None);
    assert_eq!(resume.runtime_workspace_roots, None);
    assert_eq!(fork.runtime_workspace_roots, None);
    assert_eq!(start.model_provider, None);
    assert_eq!(resume.model_provider, None);
    assert_eq!(fork.model_provider, None);
    assert_eq!(start.sandbox, expected_sandbox);
    assert_eq!(resume.sandbox, None);
    assert_eq!(resume.approval_policy, None);
    assert_eq!(resume.approvals_reviewer, None);
    assert_eq!(fork.sandbox, expected_sandbox);
    assert_eq!(start.permissions, None);
    assert_eq!(resume.permissions, None);
    assert_eq!(fork.permissions, None);
    assert_eq!(start.thread_source, Some(ThreadSource::User));
    assert_eq!(fork.thread_source, Some(ThreadSource::User));
}

#[tokio::test]
async fn remote_resume_params_keep_cwd_without_overriding_saved_permissions() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let remote_cwd = if cfg!(windows) {
        std::path::PathBuf::from("/srv/remote/project")
    } else {
        std::path::PathBuf::from(r"C:\remote\project")
    };

    let resume = thread_resume_params_from_config(
        config,
        ThreadId::new(),
        ThreadParamsMode::Remote,
        Some(remote_cwd.as_path()),
        ResumeModelSettings::RestoreFromThread,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );

    assert_eq!(resume.cwd, Some(remote_cwd.to_string_lossy().to_string()));
    assert_eq!(resume.runtime_workspace_roots, None);
}

#[test]
fn sandbox_mode_does_not_project_non_cwd_write_roots_for_remote_sessions() {
    let cwd = test_path_buf("/workspace/project").abs();
    let extra_root = test_path_buf("/workspace/cache").abs();
    let permission_profile: PermissionProfile = PermissionProfile::Managed {
        network: NetworkSandboxPolicy::Restricted,
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: extra_root.into(),
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
            ],
            glob_scan_max_depth: None,
        },
    };

    assert_eq!(
        sandbox_mode_from_permission_profile(&permission_profile, cwd.as_path()),
        Some(codex_app_server_protocol::SandboxMode::ReadOnly)
    );
    assert!(!permission_profile_is_safely_represented_by_sandbox_mode(
        &permission_profile,
        cwd.as_path(),
    ));
}

#[test]
fn sandbox_mode_projects_cwd_write_for_remote_sessions() {
    let cwd = test_path_buf("/workspace/project").abs();
    let permission_profile: PermissionProfile = PermissionProfile::Managed {
        network: NetworkSandboxPolicy::Restricted,
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::ProjectRoots { subpath: None },
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
            ],
            glob_scan_max_depth: None,
        },
    };

    assert_eq!(
        sandbox_mode_from_permission_profile(&permission_profile, cwd.as_path()),
        Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite)
    );
}

#[tokio::test]
async fn thread_lifecycle_params_forward_explicit_remote_cwd_override_for_remote_sessions() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();
    let remote_cwd = PathBuf::from("repo/on/server");
    let expected_sandbox = sandbox_mode_from_permission_profile(
        &config.permissions.effective_permission_profile(),
        config.cwd.as_path(),
    );

    let start = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Remote,
        Some(remote_cwd.as_path()),
        /*session_start_source*/ None,
    );
    let resume = thread_resume_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Remote,
        Some(remote_cwd.as_path()),
        ResumeModelSettings::OverrideFromCurrentConfig,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );
    let fork = thread_fork_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Remote,
        Some(remote_cwd.as_path()),
    );

    assert_eq!(start.cwd.as_deref(), Some("repo/on/server"));
    assert_eq!(resume.cwd.as_deref(), Some("repo/on/server"));
    assert_eq!(fork.cwd.as_deref(), Some("repo/on/server"));
    assert_eq!(start.runtime_workspace_roots, None);
    assert_eq!(resume.runtime_workspace_roots, None);
    assert_eq!(fork.runtime_workspace_roots, None);
    assert_eq!(start.model_provider, None);
    assert_eq!(resume.model_provider, None);
    assert_eq!(fork.model_provider, None);
    assert_eq!(start.sandbox, expected_sandbox);
    assert_eq!(resume.sandbox, None);
    assert_eq!(fork.sandbox, expected_sandbox);
    assert_eq!(start.permissions, None);
    assert_eq!(resume.permissions, None);
    assert_eq!(fork.permissions, None);
    assert_eq!(start.thread_source, Some(ThreadSource::User));
    assert_eq!(fork.thread_source, Some(ThreadSource::User));
}

#[tokio::test]
async fn thread_lifecycle_params_forward_config_overrides_and_service_tier() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&temp_dir).await;
    config.model_reasoning_effort = Some(ReasoningEffort::High);
    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
    config.model_verbosity = Some(Verbosity::Low);
    config
        .web_search_mode
        .set(WebSearchMode::Disabled)
        .expect("test web search mode should be allowed");
    config.bypass_hook_trust = true;
    config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    let thread_id = ThreadId::new();

    let start = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );
    let resume = thread_resume_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        ResumeModelSettings::OverrideFromCurrentConfig,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );
    let fork = thread_fork_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
    );

    let expected_service_tier = Some(Some(ServiceTier::Fast.request_value().to_string()));
    assert_eq!(start.service_tier, expected_service_tier);
    assert_eq!(resume.service_tier, expected_service_tier);
    assert_eq!(fork.service_tier, expected_service_tier);
    let string = |value: &str| serde_json::Value::String(value.to_string());
    let expected_config = HashMap::from([
        ("model_reasoning_effort".to_string(), string("high")),
        ("bypass_hook_trust".to_string(), true.into()),
    ]);
    assert_eq!(start.config, Some(expected_config.clone()));
    assert_eq!(resume.config, Some(expected_config.clone()));
    assert_eq!(fork.config, Some(expected_config));
}

#[tokio::test]
async fn config_overrides_forward_explicit_summary_verbosity_and_web_search() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".codex"))?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "model_reasoning_summary = \"concise\"\nmodel_verbosity = \"low\"\nweb_search = \"disabled\"\npersonality = \"none\"\n[projects.{}]\ntrust_level = \"trusted\"\n",
            toml::Value::String(workspace.to_string_lossy().into_owned()),
        ),
    )?;
    let profile = AbsolutePathBuf::from_absolute_path(home.path().join("work.config.toml"))?;
    std::fs::write(
        &profile,
        "model_reasoning_summary = \"detailed\"\nmodel_verbosity = \"high\"\nweb_search = \"cached\"\n",
    )?;
    for (selected_profile, project, cli, expected) in [
        (false, false, false, [None, None, None]),
        (
            true,
            false,
            false,
            [Some("detailed"), Some("high"), Some("cached")],
        ),
        (
            false,
            false,
            true,
            [Some("auto"), Some("medium"), Some("live")],
        ),
        (
            true,
            false,
            true,
            [Some("auto"), Some("medium"), Some("live")],
        ),
        (true, true, false, [None, None, None]),
        (
            true,
            true,
            true,
            [Some("auto"), Some("medium"), Some("live")],
        ),
    ] {
        std::fs::write(
            workspace.join(".codex/config.toml"),
            if project {
                "model_reasoning_summary = \"concise\"\nmodel_verbosity = \"low\"\nweb_search = \"disabled\"\n"
            } else {
                ""
            },
        )?;
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                cwd: Some(workspace.clone()),
                ..ConfigOverrides::default()
            })
            .loader_overrides(codex_config::LoaderOverrides {
                user_config_path: selected_profile.then(|| profile.clone()),
                user_config_profile: selected_profile.then(|| "work".parse().unwrap()),
                ..codex_config::LoaderOverrides::without_managed_config_for_tests()
            })
            .cli_overrides(if cli {
                vec![
                    ("model_reasoning_summary".to_string(), "auto".into()),
                    ("model_verbosity".to_string(), "medium".into()),
                    ("web_search".to_string(), "live".into()),
                    ("personality".to_string(), "none".into()),
                ]
            } else {
                Vec::new()
            })
            .build()
            .await?;
        let overrides = config_request_overrides_from_config(&config, ThreadParamsMode::Remote)
            .expect("config overrides");
        assert_eq!(
            (
                ["model_reasoning_summary", "model_verbosity", "web_search"]
                    .map(|key| overrides.get(key).and_then(serde_json::Value::as_str)),
                overrides
                    .get("personality")
                    .and_then(serde_json::Value::as_str),
            ),
            (expected, cli.then_some("none")),
        );
    }
    Ok(())
}

#[tokio::test]
async fn thread_resume_params_can_restore_persisted_model_settings() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&temp_dir).await;
    config.model = Some("configured-model".to_string());
    config.model_provider_id = "configured-provider".to_string();
    config.model_reasoning_effort = Some(ReasoningEffort::Ultra);
    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);

    let params = thread_resume_params_from_config(
        config,
        ThreadId::new(),
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        ResumeModelSettings::RestoreFromThread,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );

    assert_eq!(params.model, None);
    assert_eq!(params.model_provider, None);
    assert_eq!(params.config, None);
}

#[tokio::test]
async fn thread_resume_params_can_rejoin_without_overriding_existing_settings() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();

    let params = thread_resume_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        ResumeModelSettings::PreserveExistingThread,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );

    assert_eq!(
        params,
        ThreadResumeParams {
            thread_id: thread_id.to_string(),
            ..ThreadResumeParams::default()
        }
    );
}

#[tokio::test]
async fn connected_thread_start_preserves_flex_without_catalog_support() -> Result<()> {
    for fast_mode_enabled in [false, true] {
        let codex_home = tempfile::tempdir()?;
        let server_config = build_config(&codex_home).await;
        let mut config = server_config.clone();
        config.model = Some("gpt-5.5".to_string());
        config.service_tier = Some(ServiceTier::Flex.request_value().to_string());
        config
            .features
            .set_enabled(Feature::FastMode, fast_mode_enabled)?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&server_config).await?;
        app_server.thread_params_mode = ThreadParamsMode::Remote;
        let mut preset = crate::test_support::TEST_MODEL_PRESETS
            .iter()
            .find(|preset| preset.model == "gpt-5.5")
            .expect("gpt-5.5 test preset")
            .clone();
        preset.service_tiers.clear();
        preset.default_service_tier = None;
        app_server.available_models = vec![preset];

        let started = app_server.start_thread(&config).await?;

        assert_eq!(
            started.session.service_tier,
            Some(ServiceTier::Flex.request_value().to_string())
        );
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn persisted_resume_does_not_forward_implicit_service_tier() -> Result<()> {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&codex_home).await;
    config.model = Some("gpt-5.5".to_string());
    config.service_tier = None;
    config
        .features
        .enable(Feature::FastMode)
        .expect("enable fast mode");
    let thread_id = ThreadId::from_string(
        &create_fake_rollout(
            codex_home.path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .expect("create source rollout"),
    )?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let mut preset = crate::test_support::TEST_MODEL_PRESETS
        .iter()
        .find(|preset| preset.model == "gpt-5.5")
        .expect("gpt-5.5 test preset")
        .clone();
    preset.service_tiers = vec![ModelServiceTier {
        id: ServiceTier::Fast.request_value().to_string(),
        name: "fast".to_string(),
        description: "Fast tier".to_string(),
    }];
    preset.default_service_tier = Some(ServiceTier::Fast.request_value().to_string());
    app_server.available_models = vec![preset];

    let resumed = app_server
        .resume_thread(
            &LocalSettings::from(&config),
            config,
            thread_id,
            ResumeModelSettings::RestoreFromThread,
        )
        .await?;

    assert_eq!(resumed.session.service_tier, None);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn side_fork_skips_parent_title_lookup_but_normal_ephemeral_fork_keeps_it() -> Result<()> {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let config = build_config(&codex_home).await;
    let source_thread_id = ThreadId::from_string(
        &create_fake_rollout(
            codex_home.path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .expect("create source rollout"),
    )?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    app_server
        .resume_thread(
            &LocalSettings::from(&config),
            config.clone(),
            source_thread_id,
            ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    app_server
        .thread_set_name(source_thread_id, "Source thread".to_string())
        .await?;

    let mut ephemeral_config = config;
    ephemeral_config.ephemeral = true;
    let normal_ephemeral_fork = app_server
        .fork_thread(
            &LocalSettings::from(&ephemeral_config),
            ephemeral_config.clone(),
            source_thread_id,
        )
        .await?;
    let side_fork = app_server
        .fork_side_thread(
            &LocalSettings::from(&ephemeral_config),
            ephemeral_config,
            source_thread_id,
            /*selected_profile*/ None,
            crate::app_server_session::SideChatStorage::Temporary,
        )
        .await?;

    assert_eq!(
        normal_ephemeral_fork.session.fork_parent_title.as_deref(),
        Some("Source thread")
    );
    assert_eq!(side_fork.session.fork_parent_title, None);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn ephemeral_paginated_fork_skips_unsupported_history_hydration() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let config = build_config(&codex_home).await;
    let source_thread_id = ThreadId::from_string(
        &create_fake_paginated_rollout(
            codex_home.path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .map_err(|error| color_eyre::eyre::eyre!("failed to create paginated rollout: {error}"))?,
    )?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let mut ephemeral_config = config;
    ephemeral_config.ephemeral = true;

    let fork = app_server
        .fork_thread(
            &LocalSettings::from(&ephemeral_config),
            ephemeral_config,
            source_thread_id,
        )
        .await?;

    assert_eq!(fork.session.forked_from_id, Some(source_thread_id));
    assert!(fork.turns.is_empty());
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn side_fork_uses_one_request_for_long_paginated_history() -> Result<()> {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&codex_home).await;
    config.terminal_resize_reflow.max_rows =
        crate::legacy_core::config::TerminalResizeReflowMaxRows::Limit(100);
    let filename_ts = "2025-01-05T12-00-00";
    let source_id = create_fake_paginated_rollout(
        codex_home.path(),
        filename_ts,
        "2025-01-05T12:00:00Z",
        "Saved user message",
        Some(config.model_provider_id.as_str()),
        /*git_info*/ None,
    )
    .expect("create long paginated source rollout");
    let source_path =
        app_test_support::rollout_path(codex_home.path(), filename_ts, source_id.as_str());
    let mut contents = std::fs::read_to_string(&source_path)?;
    let rollout_line = |ordinal: usize, payload: serde_json::Value| {
        serde_json::json!({
            "timestamp": "2025-01-05T12:00:00Z",
            "type": "event_msg",
            "payload": payload,
            "ordinal": ordinal,
        })
    };
    let started = rollout_line(
        /*ordinal*/ 3,
        serde_json::json!({
            "type": "task_started",
            "turn_id": "long-history-turn",
            "model_context_window": null,
        }),
    );
    contents.push_str(&format!("{started}\n"));
    for index in 0..256 {
        let item = rollout_line(
            index + 4,
            serde_json::json!({
                "type": "item_completed",
                "thread_id": source_id,
                "turn_id": "long-history-turn",
                "item": {
                    "type": "UserMessage",
                    "id": format!("long-history-user-{index}"),
                    "content": [{
                        "type": "text",
                        "text": format!("long history message {index}"),
                    }],
                },
            }),
        );
        contents.push_str(&format!("{item}\n"));
    }
    std::fs::write(source_path, contents)?;

    let source_thread_id = ThreadId::from_string(source_id.as_str())?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let resumed = app_server
        .resume_thread(
            &LocalSettings::from(&config),
            config.clone(),
            source_thread_id,
            ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    let loaded_items: usize = resumed.turns.iter().map(|turn| turn.items.len()).sum();
    assert!(loaded_items <= HISTORY_ITEM_PAGE_LIMIT as usize);
    assert!(app_server.has_older_history(source_thread_id));

    let mut side_config = config;
    side_config.ephemeral = true;
    let next_request_id = app_server.next_request_id;
    let side = app_server
        .fork_side_thread(
            &LocalSettings::from(&side_config),
            side_config,
            source_thread_id,
            /*selected_profile*/ None,
            crate::app_server_session::SideChatStorage::Temporary,
        )
        .await?;

    assert_eq!(app_server.next_request_id, next_request_id + 1);
    assert_eq!(side.session.forked_from_id, Some(source_thread_id));
    assert_eq!(side.turns, Vec::<Turn>::new());
    assert!(app_server.has_older_history(source_thread_id));
    assert!(
        !app_server
            .history_pagination
            .contains_key(&side.session.thread_id)
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn thread_fork_params_forward_instruction_overrides() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&temp_dir).await;
    config.base_instructions = Some("Base override.".to_string());
    config.base_instructions_provenance = Some(BaseInstructionsProvenance::Custom);
    config.developer_instructions = Some("Developer override.".to_string());
    let thread_id = ThreadId::new();

    let params = thread_fork_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
    );

    assert_eq!(params.base_instructions.as_deref(), Some("Base override."));
    assert_eq!(
        params.developer_instructions.as_deref(),
        Some("Developer override.")
    );

    config.base_instructions_provenance = Some(BaseInstructionsProvenance::Model {
        model: "gpt-5.2".to_string(),
    });
    let params = thread_fork_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Remote,
        /*remote_cwd_override*/ None,
    );

    assert_eq!(params.base_instructions, None);
}

#[tokio::test]
async fn cloud_fork_is_rejected_before_sending_local_settings() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let (client, mut commands, _events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
    let mut app_server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        AppServerClient::Cloud(client),
        ThreadParamsMode::Cloud,
    );

    let error = app_server
        .fork_thread(&LocalSettings::from(&config), config, ThreadId::new())
        .await
        .expect_err("cloud fork should be unavailable");

    assert_eq!(
        error.to_string(),
        "Cloud conversations cannot be forked in the TUI yet."
    );
    assert!(commands.try_recv().is_err());
}

#[tokio::test]
async fn side_fork_excludes_turns_without_clearing_regular_ephemeral_fork() -> Result<()> {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&codex_home).await;
    config.ephemeral = true;
    let thread_id = ThreadId::from_string(
        &create_fake_rollout(
            codex_home.path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .expect("create rollout"),
    )?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;

    let regular = app_server
        .fork_thread(&LocalSettings::from(&config), config.clone(), thread_id)
        .await?;
    let side = app_server
        .fork_side_thread(
            &LocalSettings::from(&config),
            config,
            thread_id,
            /*selected_profile*/ None,
            crate::app_server_session::SideChatStorage::Temporary,
        )
        .await?;

    assert_eq!(regular.turns.len(), 1);
    assert!(matches!(
        regular.turns[0].items.as_slice(),
        [codex_app_server_protocol::ThreadItem { payload: codex_app_server_protocol::ThreadItemPayload::UserMessage { content, .. }, .. }]
            if content == &[UserInput::Text {
                text: "Saved user message".to_string(),
                text_elements: Vec::new(),
            }]
    ));
    assert_eq!(side.turns, Vec::<Turn>::new());
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn terminal_visualization_instructions_are_gated_for_all_tui_thread_flows() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut config = build_config(&temp_dir).await;
    config.developer_instructions = Some("Developer override.".to_string());
    let thread_id = ThreadId::new();

    let control_start = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );
    let control_resume = thread_resume_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        ResumeModelSettings::OverrideFromCurrentConfig,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );
    let control_fork = thread_fork_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
    );

    assert_eq!(control_start.developer_instructions, None);
    assert_eq!(control_resume.developer_instructions, None);
    assert_eq!(
        control_fork.developer_instructions.as_deref(),
        Some("Developer override.")
    );

    let _ = config
        .features
        .enable(Feature::TerminalVisualizationInstructions);
    let treatment_start = thread_start_params_from_config(
        &config,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        /*session_start_source*/ None,
    );
    let treatment_resume = thread_resume_params_from_config(
        config.clone(),
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
        ResumeModelSettings::OverrideFromCurrentConfig,
        crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
    );
    let treatment_fork = thread_fork_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
    );
    let expected = format!(
        "Developer override.\n\n{}",
        crate::terminal_visualization_instructions::TERMINAL_VISUALIZATION_INSTRUCTIONS
    );

    assert_eq!(
        treatment_start.developer_instructions.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        treatment_resume.developer_instructions.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        treatment_fork.developer_instructions.as_deref(),
        Some(expected.as_str())
    );
}

#[tokio::test]
async fn resume_response_restores_turns_from_thread_items() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();
    let forked_from_id = ThreadId::new();
    let read_only_profile = PermissionProfile::read_only();
    let response = ThreadResumeResponse {
        disabled_plugin_ids: Vec::new(),
        thread: codex_app_server_protocol::Thread {
            originator: None,
            environments: None,
            id: thread_id.to_string(),
            extra: None,
            session_id: ThreadId::new().to_string(),
            forked_from_id: Some(forked_from_id.to_string()),
            parent_thread_id: None,
            preview: "hello".to_string(),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            project_id: None,
            daybreak_enabled: None,
            history_mode: Default::default(),
            model_provider: "openai".to_string(),
            model: None,
            reasoning_effort: None,
            created_at: 1,
            updated_at: 2,
            recency_at: Some(2),
            status: ThreadStatus::Idle,
            path: None,
            cwd: test_path_buf("/tmp/project").abs(),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: vec![Turn {
                id: "turn-1".to_string(),
                root_turn_id: None,
                items_view: codex_app_server_protocol::TurnItemsView::Full,
                items: vec![
                    ThreadItem::untimed(
                        codex_app_server_protocol::ThreadItemPayload::UserMessage {
                            id: "user-1".to_string(),
                            client_id: None,
                            content: vec![codex_app_server_protocol::UserInput::Text {
                                text: "hello from history".to_string(),
                                text_elements: Vec::new(),
                            }],
                        },
                    ),
                    ThreadItem::untimed(
                        codex_app_server_protocol::ThreadItemPayload::AgentMessage {
                            moderation: None,
                            id: "assistant-1".to_string(),
                            text: "assistant reply".to_string(),
                            phase: None,
                            memory_citation: None,
                            delivery: None,
                            questions: None,
                        },
                    ),
                ],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        },
        model: "gpt-5.5".to_string(),
        model_provider: "openai".to_string(),
        service_tier: None,
        cwd: test_path_buf("/tmp/project").abs(),
        runtime_workspace_roots: vec![
            test_path_buf("/tmp/project").abs(),
            test_path_buf("/tmp/project/extra").abs(),
        ],
        instruction_sources: vec![LegacyAppPathString::from_abs_path(
            &test_path_buf("/tmp/project/AGENTS.md").abs(),
        )],
        approval_policy: codex_app_server_protocol::AskForApproval::Never,
        approvals_reviewer: codex_app_server_protocol::ApprovalsReviewer::User,
        sandbox: read_only_profile
            .to_legacy_sandbox_policy(test_path_buf("/tmp/project").as_path())
            .expect("read-only profile must be legacy-compatible")
            .into(),
        active_permission_profile: None,
        reasoning_effort: None,
        collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
            mode: codex_protocol::config_types::ModeKind::Plan,
            settings: codex_protocol::config_types::Settings {
                model: "gpt-5.5".to_string(),
                reasoning_effort: None,
                developer_instructions: Some("Keep planning".to_string()),
            },
        }),
        multi_agent_mode: Default::default(),
        initial_turns_page: None,
        turns_backwards_cursor: None,
        items_backwards_cursor: None,
    };

    let started = started_thread_from_resume_response(
        response.clone(),
        &LocalSettings::from(&config),
        ThreadParamsMode::Remote,
    )
    .await
    .expect("resume response should map");
    assert_eq!(started.session.forked_from_id, Some(forked_from_id));
    assert_eq!(
        started.session.runtime_workspace_roots,
        response.runtime_workspace_roots
    );
    assert_eq!(
        started.session.instruction_source_paths,
        response.instruction_source_path_uris()
    );
    assert_eq!(started.session.permission_profile, read_only_profile);
    assert_eq!(started.turns.len(), 1);
    assert_eq!(started.turns[0], response.thread.turns[0]);
    assert!(!started.blocks_direct_input);

    // The first prompt after resume must preserve the server's restored mode.
    let (mut chat, _sender, _events, mut commands) =
        crate::chatwidget::tests::helpers::make_chatwidget_manual_with_sender().await;
    chat.handle_thread_session(started.session);
    insta::assert_snapshot!(
        "resumed_plan_mode",
        crate::chatwidget::tests::helpers::normalize_snapshot_paths(
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 40),
        )
    );
    chat.handle_paste("Continue planning".to_string());
    chat.handle_key_event(crossterm::event::KeyCode::Enter.into());
    let submitted_mode = std::iter::from_fn(|| commands.try_recv().ok()).find_map(|command| {
        if let crate::app_command::AppCommand::UserTurn {
            collaboration_mode, ..
        } = command
        {
            Some(collaboration_mode)
        } else {
            None
        }
    });
    assert_eq!(submitted_mode, Some(response.collaboration_mode.clone()));

    let embedded_config = ConfigBuilder::default()
        .codex_home(temp_dir.path().join("embedded-codex-home"))
        .harness_overrides(ConfigOverrides {
            default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
            ..ConfigOverrides::default()
        })
        .build()
        .await
        .expect("config should build");
    let started = started_thread_from_resume_response(
        response.clone(),
        &LocalSettings::from(&embedded_config),
        ThreadParamsMode::Embedded,
    )
    .await
    .expect("embedded resume response should map");
    assert_eq!(started.session.permission_profile, read_only_profile);

    let mut thread = response.thread.clone();
    let mut empty_roots_response = response;
    empty_roots_response.runtime_workspace_roots = Vec::new();
    let started = started_thread_from_resume_response(
        empty_roots_response,
        &LocalSettings::from(&config),
        ThreadParamsMode::Remote,
    )
    .await
    .expect("resume response should map");
    assert_eq!(started.session.runtime_workspace_roots, Vec::new());

    // Cloud can filter every item in its newest page while older prompts remain.
    thread.history_mode = ThreadHistoryMode::Paginated;
    let mut turn = thread.turns.pop().unwrap();
    let prompt = turn.items.remove(0);
    turn.items.clear();
    turn.items_view = codex_app_server_protocol::TurnItemsView::NotLoaded;
    let (client, mut commands, _events) =
        codex_app_server_client::CloudClient::channel(/*capacity*/ 1);
    let mut server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        AppServerClient::Cloud(client),
        ThreadParamsMode::Cloud,
    );
    let worker = tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            let codex_app_server_client::CloudOperation::Request(request) = command.operation
            else {
                panic!("expected history request");
            };
            let reply = match *request {
                ClientRequest::ThreadTurnsList { .. } => json!({"data": [turn]}),
                ClientRequest::ThreadItemsList { params, .. } if params.cursor.is_none() => {
                    json!({"data": [], "nextCursor": "older"})
                }
                ClientRequest::ThreadItemsList { params, .. }
                    if params.cursor == Some(ThreadItemsListCursor::Opaque("older".into())) =>
                {
                    assert_eq!(params.limit, Some(HISTORY_ITEM_PAGE_LIMIT));
                    json!({"data": [{"turnId": turn.id, "item": prompt}], "nextCursor": "oldest"})
                }
                request => panic!("unexpected history request: {request:?}"),
            };
            command.response.send(Ok(Ok(reply))).unwrap();
        }
    });
    let mut settings = LocalSettings::from(&config);
    settings.transcript_mode = crate::transcript_mode::TranscriptMode::Terminal;
    settings.tui.terminal_resize_reflow_max_rows = Some(1);
    server
        .hydrate_initial_thread_history(
            &mut thread,
            /*turn_cursor*/ None,
            /*item_cursor*/ None,
            Some(&config),
            Some(&settings),
            HistoryHydrationScope::Initial,
        )
        .await
        .expect("Cloud history should load past a hidden page");
    let cells = crate::thread_transcript::thread_to_transcript_cells(
        thread,
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        /*config*/ None,
    );
    let lines = cells[0].display_lines(/*width*/ 80);
    insta::assert_snapshot!(lines.get(1).map(ToString::to_string).unwrap_or_default(), @"› hello from history");
    let next = server.begin_older_history_page(thread_id);
    assert_eq!(next.as_deref(), Some("oldest"));
    drop(server);
    worker.await.unwrap();
}

#[tokio::test]
async fn remote_thread_response_uses_legacy_sandbox_fallback() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let cwd = test_path_buf("/tmp/project").abs();
    let sandbox = PermissionProfile::read_only()
        .to_legacy_sandbox_policy(cwd.as_path())
        .expect("read-only profile must be legacy-compatible")
        .into();

    assert_eq!(
        display_permission_profile_from_thread_response(
            &sandbox,
            cwd.as_path(),
            &config,
            ThreadParamsMode::Remote,
        ),
        PermissionProfile::read_only()
    );
}

#[tokio::test]
async fn embedded_thread_response_uses_local_config_profile() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = ConfigBuilder::default()
        .codex_home(temp_dir.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_READ_ONLY.to_string()),
            ..ConfigOverrides::default()
        })
        .build()
        .await
        .expect("config should build");
    let cwd = test_path_buf("/tmp/project").abs();

    assert_eq!(
        display_permission_profile_from_thread_response(
            &codex_app_server_protocol::SandboxPolicy::DangerFullAccess,
            cwd.as_path(),
            &config,
            ThreadParamsMode::Embedded,
        ),
        PermissionProfile::read_only()
    );
}

#[tokio::test]
async fn session_configured_populates_history_metadata() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();

    let history_config =
        codex_message_history::HistoryConfig::new(config.codex_home.clone(), &config.history);

    codex_message_history::append_entry("older", &thread_id, &history_config)
        .await
        .expect("history append should succeed");
    codex_message_history::append_entry("newer", &thread_id, &history_config)
        .await
        .expect("history append should succeed");

    let session = thread_session_state_from_thread_response(
        &thread_id.to_string(),
        crate::app::WindowsSandboxHost::Local,
        /*forked_from_id*/ None,
        Some("restore".to_string()),
        /*rollout_path*/ None,
        "gpt-5.4".to_string(),
        "openai".to_string(),
        /*service_tier*/ None,
        AskForApproval::Never,
        codex_protocol::config_types::ApprovalsReviewer::User,
        PermissionProfile::read_only(),
        /*active_permission_profile*/ None,
        test_path_buf("/tmp/project").abs(),
        Vec::new(),
        Vec::new(),
        /*reasoning_effort*/ None,
        &LocalSettings::from(&config),
    )
    .await
    .expect("session should map");

    let metadata = session
        .message_history
        .expect("session should include message-history metadata");
    assert_ne!(metadata.log_id, 0);
    assert_eq!(metadata.entry_count, 2);
}

#[tokio::test]
async fn session_configured_preserves_fork_source_thread_id() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();
    let forked_from_id = ThreadId::new();

    let session = thread_session_state_from_thread_response(
        &thread_id.to_string(),
        crate::app::WindowsSandboxHost::Local,
        Some(forked_from_id.to_string()),
        Some("restore".to_string()),
        /*rollout_path*/ None,
        "gpt-5.4".to_string(),
        "openai".to_string(),
        /*service_tier*/ None,
        AskForApproval::Never,
        codex_protocol::config_types::ApprovalsReviewer::User,
        PermissionProfile::read_only(),
        /*active_permission_profile*/ None,
        test_path_buf("/tmp/project").abs(),
        Vec::new(),
        Vec::new(),
        /*reasoning_effort*/ None,
        &LocalSettings::from(&config),
    )
    .await
    .expect("session should map");

    assert_eq!(session.forked_from_id, Some(forked_from_id));
}

#[test]
fn status_account_display_from_auth_mode_uses_remapped_plan_labels() {
    let business = status_account_display_from_auth_mode(
        Some(AuthMode::Chatgpt),
        Some(codex_protocol::account::PlanType::EnterpriseCbpUsageBased),
    );
    assert!(matches!(
        business,
        Some(StatusAccountDisplay::ChatGpt {
            email: None,
            plan: Some(ref plan),
            ..
        }) if plan == "Enterprise"
    ));

    let team = status_account_display_from_auth_mode(
        Some(AuthMode::Chatgpt),
        Some(codex_protocol::account::PlanType::SelfServeBusinessUsageBased),
    );
    assert!(matches!(
        team,
        Some(StatusAccountDisplay::ChatGpt {
            email: None,
            plan: Some(ref plan),
            ..
        }) if plan == "Business"
    ));

    let business_prolite = status_account_display_from_auth_mode(
        Some(AuthMode::Chatgpt),
        Some(codex_protocol::account::PlanType::SelfServeBusinessProLite),
    );
    assert!(matches!(
        business_prolite,
        Some(StatusAccountDisplay::ChatGpt {
            email: None,
            plan: Some(ref plan),
            ..
        }) if plan == "Business Premium"
    ));
}
