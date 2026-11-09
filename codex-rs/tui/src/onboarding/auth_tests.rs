use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_client::InProcessClientStartArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_features::Feature;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tempfile::TempDir;

const PRODUCTION_LENGTH_AUTH_URL: &str = concat!(
    "https://auth.openai.com/oauth/authorize?",
    "response_type=code&",
    "client_id=app_EMoamEEZ73f0CkXaXp7hrann&",
    "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&",
    "scope=openid%20profile%20email%20offline_access%20",
    "api.connectors.read%20api.connectors.invoke&",
    "code_challenge=1YM3Z8QbrLbdt9C3eX3j7UQ4GmFRmKz4OeVYwD6s5xA&",
    "code_challenge_method=S256&",
    "id_token_add_organizations=true&",
    "codex_cli_simplified_flow=true&",
    "state=8cHjQ4nVx2Yp7Lm9Rk3Wf6Ta1Bs5Du0Ei4Go7Nz2PqM&",
    "originator=codex_cli_rs"
);

#[test]
fn opens_login_browser_for_local_app_servers() -> color_eyre::Result<()> {
    let endpoint = RemoteAppServerEndpoint::UnixSocket {
        socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock")?,
    };
    let url = "https://example.test/authorize";
    for (target, expected) in [
        (crate::AppServerTarget::Embedded, true),
        (
            crate::AppServerTarget::LocalDaemon {
                endpoint: endpoint.clone(),
                allow_embedded_fallback: true,
            },
            true,
        ),
        (
            crate::AppServerTarget::LocalDaemon {
                endpoint: endpoint.clone(),
                allow_embedded_fallback: false,
            },
            true,
        ),
        (crate::AppServerTarget::Remote { endpoint }, false),
    ] {
        let mut opened_url = None;
        show_chatgpt_login_in_browser(
            &target,
            &RwLock::new(SignInState::PickMode),
            &FrameRequester::test_dummy(),
            ContinueInBrowserState {
                login_id: "login-1".to_string(),
                auth_url: url.to_string(),
            },
            |url| {
                opened_url = Some(url.to_owned());
                Ok(())
            },
        );
        assert_eq!(opened_url.as_deref(), expected.then_some(url));
    }
    Ok(())
}

#[tokio::test]
async fn login_can_complete_while_browser_is_opening() {
    let (mut widget, _tmp) = widget_forced_chatgpt().await;
    let sign_in_state = widget.sign_in_state.clone();
    let request_frame = widget.request_frame.clone();
    show_chatgpt_login_in_browser(
        &crate::AppServerTarget::Embedded,
        &sign_in_state,
        &request_frame,
        ContinueInBrowserState {
            login_id: "login-1".to_string(),
            auth_url: "https://example.test/authorize".to_string(),
        },
        |_| {
            widget.on_account_login_completed(AccountLoginCompletedNotification {
                login_id: Some("login-1".to_string()),
                success: true,
                error: None,
                onboarding_entrypoint: None,
            });
            Ok(())
        },
    );
    assert!(matches!(
        &*sign_in_state.read().unwrap(),
        SignInState::ChatGptSuccessMessage
    ));
}

pub(super) async fn widget_forced_chatgpt() -> (AuthModeWidget, TempDir) {
    let codex_home = TempDir::new().unwrap();
    let codex_home_path = codex_home.path().to_path_buf();
    // Embedded startup can refresh cloud config using the host's workload identity.
    let loader_overrides = LoaderOverrides {
        ignore_managed_requirements: true,
        managed_config_path: Some(codex_home_path.join("managed_config.toml")),
        ..LoaderOverrides::without_managed_config_for_tests()
    };
    let config = ConfigBuilder::default()
        .loader_overrides(loader_overrides.clone())
        .codex_home(codex_home_path.clone())
        .build()
        .await
        .unwrap();
    let mut auth_config = config.auth_config();
    let client = InProcessAppServerClient::start(InProcessClientStartArgs {
        required_human_approval: true,
        arg0_paths: Arg0DispatchPaths::default(),
        config: Arc::new(config),
        cli_overrides: Vec::new(),
        loader_overrides,
        strict_config: false,
        cloud_config_bundle: CloudConfigBundleLoader::default(),
        embedded_network_policy: Default::default(),
        feedback: codex_feedback::CodexFeedback::new(),
        log_db: None,
        state_db: None,
        environment_manager: Arc::new(
            codex_app_server_client::EnvironmentManager::default_for_tests(),
        ),
        config_warnings: Vec::new(),
        session_source: serde_json::from_value(serde_json::json!("cli"))
            .expect("cli session source should deserialize"),
        enable_codex_api_key_env: false,
        client_name: "test".to_string(),
        client_version: "test".to_string(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await
    .unwrap();
    auth_config.forced_login_method = Some(ForcedLoginMethod::Chatgpt);
    let widget = AuthModeWidget {
        request_frame: FrameRequester::test_dummy(),
        highlighted_mode: SignInOption::ChatGpt,
        activation_key_guard: None,
        error: Arc::new(RwLock::new(None)),
        sign_in_state: Arc::new(RwLock::new(SignInState::PickMode)),
        login_status: LoginStatus::NotAuthenticated,
        app_server_target: crate::AppServerTarget::Embedded,
        app_server_request_handle: AppServerRequestHandle::InProcess(client.request_handle()),
        auth_config,
        bedrock_setup_enabled: false,
        gcp_setup_enabled: false,
        gateway_setup_enabled: false,
        gateway: None,
        animations_enabled: true,
        animations_suppressed: std::cell::Cell::new(false),
    };
    (widget, codex_home)
}

#[tokio::test]
async fn api_key_entry_snapshots() {
    let (mut widget, _tmp) = widget_forced_chatgpt().await;
    widget.auth_config.forced_login_method = None;
    *widget.sign_in_state.write().unwrap() = SignInState::ApiKeyEntry(ApiKeyInputState::default());

    for (name, width) in [("api_key_entry", 80), ("api_key_entry_narrow", 40)] {
        let height = 14;
        let area = Rect::new(/*x*/ 0, /*y*/ 0, width, height);
        let mut terminal = crate::custom_terminal::Terminal::with_options(
            crate::test_backend::VT100Backend::new(width, height),
        )
        .expect("terminal");
        terminal.set_viewport_area(area);
        terminal
            .draw(|frame| widget.render_ref(area, frame.buffer_mut()))
            .expect("draw");

        insta::assert_snapshot!(name, terminal.backend().to_string());
    }
}

#[tokio::test]
async fn api_key_flow_disabled_when_chatgpt_forced() {
    let (mut widget, _tmp) = widget_forced_chatgpt().await;

    widget.start_api_key_entry();

    assert_eq!(
        widget.error_message().as_deref(),
        Some(API_KEY_DISABLED_MESSAGE)
    );
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
}

#[tokio::test]
async fn bedrock_option_requires_feature_and_api_login_permission() {
    let (mut widget, codex_home) = widget_forced_chatgpt().await;
    widget.auth_config.forced_login_method = None;
    assert_eq!(
        widget.displayed_sign_in_options(),
        vec![
            SignInOption::ChatGpt,
            SignInOption::DeviceCode,
            SignInOption::OtherMethods,
        ]
    );

    std::fs::write(
        codex_home.path().join("config.toml"),
        "model_provider = \"openai\"\n[features]\nbedrock_setup_wizard = true\n",
    )
    .unwrap();
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .unwrap();
    widget.bedrock_setup_enabled = crate::should_show_provider_setup_wizard(
        widget.login_status,
        /*requires_openai_auth*/ true,
        &config,
        &widget.app_server_target,
        Feature::BedrockSetupWizard,
    );
    widget.handle_key_event(KeyCode::Char('3').into());
    assert_eq!(
        widget.displayed_sign_in_options(),
        vec![SignInOption::ApiKey, SignInOption::Bedrock]
    );
    widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    widget.move_highlight(/*delta*/ 1);

    let area = Rect::new(0, 0, 76, 19);
    let mut buffer = Buffer::empty(area);
    widget.render_pick_mode(area, &mut buffer);
    let mut rows = (area.top()..area.bottom())
        .map(|row| {
            (area.left()..area.right())
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    insta::assert_snapshot!(rows.join("\n"), @r###"
          Choose how you want to use Codex.

        > 1. Sign in with ChatGPT
             Usage included with Plus, Pro, Business, and Enterprise plans

          2. Sign in with Device Code
             Sign in from another device with a one-time code

          3. Sign in another way
             Use an API key or another provider

          Press enter to continue
        "###);

    widget.auth_config.forced_login_method = Some(ForcedLoginMethod::Chatgpt);
    assert_eq!(
        widget.displayed_sign_in_options(),
        vec![SignInOption::ChatGpt, SignInOption::DeviceCode]
    );
}

#[tokio::test]
async fn saving_api_key_is_blocked_when_chatgpt_forced() {
    let (mut widget, _tmp) = widget_forced_chatgpt().await;

    widget.save_api_key("sk-test".to_string());

    assert_eq!(
        widget.error_message().as_deref(),
        Some(API_KEY_DISABLED_MESSAGE)
    );
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
    assert_eq!(widget.login_status, LoginStatus::NotAuthenticated);
}

#[tokio::test]
async fn existing_non_oauth_chatgpt_login_counts_as_signed_in() {
    for auth_mode in [AuthMode::ChatgptAuthTokens, AuthMode::PersonalAccessToken] {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.login_status = LoginStatus::AuthMode(auth_mode);

        let handled = widget.handle_existing_chatgpt_login();

        assert_eq!(handled, true);
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::ChatGptSuccess
        ));
    }
}

#[tokio::test]
async fn cancel_active_attempt_resets_browser_login_state() {
    let (widget, _tmp) = widget_forced_chatgpt().await;
    *widget.error.write().unwrap() = Some("still logging in".to_string());
    *widget.sign_in_state.write().unwrap() =
        SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
            login_id: "login-1".to_string(),
            auth_url: "https://auth.example.com".to_string(),
        });

    widget.cancel_active_attempt();

    assert_eq!(widget.error_message(), None);
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
}

#[tokio::test]
async fn cancel_active_attempt_notifies_device_code_login() {
    let (widget, _tmp) = widget_forced_chatgpt().await;
    *widget.error.write().unwrap() = Some("still logging in".to_string());
    *widget.sign_in_state.write().unwrap() =
        SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
            "request-1".to_string(),
            "login-1".to_string(),
            "https://chatgpt.com/device".to_string(),
            "ABCD-EFGH".to_string(),
        ));

    widget.cancel_active_attempt();

    assert_eq!(widget.error_message(), None);
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
}

/// Collects all buffer cell symbols that contain the OSC 8 open sequence
/// for the given URL.  Returns the concatenated "inner" characters.
fn collect_osc8_chars(buf: &Buffer, area: Rect, url: &str) -> String {
    let open = format!("\x1B]8;;{url}\x07");
    let close = "\x1B]8;;\x07";
    let mut chars = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let sym = buf[(x, y)].symbol();
            if let Some(rest) = sym.strip_prefix(open.as_str())
                && let Some(ch) = rest.strip_suffix(close)
            {
                chars.push_str(ch);
            }
        }
    }
    chars
}

#[test]
fn continue_in_browser_preserves_long_link_and_footer_at_narrow_width() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
    widget.set_animations_suppressed(/*suppressed*/ true);
    *widget.sign_in_state.write().unwrap() =
        SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
            login_id: "login-1".to_string(),
            auth_url: PRODUCTION_LENGTH_AUTH_URL.to_string(),
        });

    let width = 44;
    let height = 30;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    widget.render_ref(area, &mut buf);

    let found = collect_osc8_chars(&buf, area, PRODUCTION_LENGTH_AUTH_URL);
    assert_eq!(
        found, PRODUCTION_LENGTH_AUTH_URL,
        "OSC 8 hyperlink should cover the full URL"
    );

    let mut terminal = crate::custom_terminal::Terminal::with_options(
        crate::test_backend::VT100Backend::new(width, height),
    )
    .expect("terminal");
    terminal.set_viewport_area(area);

    terminal
        .draw(|frame| widget.render_ref(area, frame.buffer_mut()))
        .expect("draw");

    let contents = terminal.backend().to_string();
    insta::assert_snapshot!("continue_in_browser_narrow_long_url", contents);
    assert!(contents.contains("On a remote or headless machine?"));
    assert!(contents.contains("Press esc to cancel"));
}

#[test]
fn chatgpt_success_message_renders_osc8_hyperlinks() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);

    widget.render_chatgpt_success_message(area, &mut buf);

    assert_eq!(
        collect_osc8_chars(&buf, area, "https://developers.openai.com/codex/security"),
        "Codex docs"
    );
    assert_eq!(
        collect_osc8_chars(&buf, area, "https://chatgpt.com/#settings"),
        "training data preferences"
    );
    assert_eq!(
        (0..57).map(|x| buf[(x, 3)].modifier).collect::<Vec<_>>(),
        [
            vec![Modifier::DIM; 47],
            vec![Modifier::DIM | Modifier::UNDERLINED; 10],
        ]
        .concat()
    );
    assert_eq!(
        (0..61).map(|x| buf[(x, 9)].modifier).collect::<Vec<_>>(),
        [
            vec![Modifier::DIM; 30],
            vec![Modifier::DIM | Modifier::UNDERLINED; 25],
            vec![Modifier::DIM; 6],
        ]
        .concat()
    );

    let visible = (area.top()..area.bottom())
        .map(|y| {
            let row = (area.left()..area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>();
            crate::terminal_hyperlinks::strip_osc8(&row)
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(visible, @r###"
        ✓ Signed in with your ChatGPT account

          You're in control
          Learn about permissions and approvals in the Codex docs

          Codex can make mistakes
          Review the code it writes and commands it runs

          Included with your ChatGPT plan
          Your plan's rate limits and training data preferences apply

          Press enter to continue
        "###);
}

#[test]
fn auth_widget_suppresses_animations_when_device_code_is_visible() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
    *widget.sign_in_state.write().unwrap() =
        SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
            "request-1".to_string(),
            "login-1".to_string(),
            "https://chatgpt.com/device".to_string(),
            "ABCD-EFGH".to_string(),
        ));

    assert_eq!(widget.should_suppress_animations(), true);
}

#[test]
fn auth_widget_suppresses_animations_while_requesting_device_code() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
    *widget.sign_in_state.write().unwrap() = SignInState::ChatGptDeviceCode(
        ContinueWithDeviceCodeState::pending("request-1".to_string()),
    );

    assert_eq!(widget.should_suppress_animations(), true);
}

#[tokio::test]
async fn device_code_login_completion_advances_to_success_message() {
    let (mut widget, _tmp) = widget_forced_chatgpt().await;
    *widget.sign_in_state.write().unwrap() =
        SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
            "request-1".to_string(),
            "login-1".to_string(),
            "https://chatgpt.com/device".to_string(),
            "ABCD-EFGH".to_string(),
        ));

    widget.on_account_login_completed(AccountLoginCompletedNotification {
        login_id: Some("login-1".to_string()),
        success: true,
        error: None,
        onboarding_entrypoint: None,
    });

    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::ChatGptSuccessMessage
    ));
}

#[test]
fn mark_url_hyperlink_wraps_cyan_underlined_cells() {
    let url = "https://example.com";
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);

    // Manually write some cyan+underlined characters to simulate a rendered URL.
    for (i, ch) in "example".chars().enumerate() {
        let cell = &mut buf[(i as u16, 0)];
        cell.set_symbol(&ch.to_string());
        cell.fg = Color::Cyan;
        cell.modifier = Modifier::UNDERLINED;
    }
    // Leave a plain cell that should NOT be marked.
    buf[(7, 0)].set_symbol("X");

    mark_url_hyperlink(&mut buf, area, url);

    // Each cyan+underlined cell should now carry the OSC 8 wrapper.
    let found = collect_osc8_chars(&buf, area, url);
    assert_eq!(found, "example");

    // The plain "X" cell should be untouched.
    assert_eq!(buf[(7, 0)].symbol(), "X");
}

#[test]
fn mark_url_hyperlink_sanitizes_control_chars() {
    let area = Rect::new(0, 0, 10, 1);
    let mut buf = Buffer::empty(area);

    // One cyan+underlined cell to mark.
    let cell = &mut buf[(0, 0)];
    cell.set_symbol("a");
    cell.fg = Color::Cyan;
    cell.modifier = Modifier::UNDERLINED;

    // URL contains ESC and BEL that could break the OSC 8 sequence.
    let malicious_url = "https://evil.com/\x1B]8;;\x07injected";
    mark_url_hyperlink(&mut buf, area, malicious_url);

    let sym = buf[(0, 0)].symbol().to_string();
    // The sanitized URL retains `]` (printable) but strips ESC and BEL.
    let sanitized = "https://evil.com/]8;;injected";
    assert!(
        sym.contains(sanitized),
        "symbol should contain sanitized URL, got: {sym:?}"
    );
    // The injected close-sequence must not survive: \x1B and \x07 are gone.
    assert!(
        !sym.contains("\x1B]8;;\x07injected"),
        "symbol must not contain raw control chars from URL"
    );
}
