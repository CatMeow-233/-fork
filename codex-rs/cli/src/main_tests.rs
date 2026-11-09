use super::*;
use assert_matches::assert_matches;
use codex_protocol::ThreadId;
use codex_tui::TokenUsage;
use pretty_assertions::assert_eq;

#[test]
fn interactive_tui_future_stays_bounded() {
    let future = run_interactive_tui(
        TuiCli::parse_from(["codex"]),
        /*remote*/ None,
        /*remote_auth_token_env*/ None,
        Arg0DispatchPaths::default(),
    );
    let size = std::mem::size_of_val(&future);

    assert!(size < 64 * 1024, "interactive TUI future is {size} bytes");
}

#[cfg(windows)]
#[test]
fn windows_update_command_resolution_ignores_relative_path_entries() {
    let cwd = std::env::current_dir().expect("current directory");
    let decoy_dir = tempfile::tempdir_in(&cwd).expect("relative decoy directory");
    let trusted_dir = tempfile::tempdir().expect("trusted PATH directory");
    let relative_decoy_dir = decoy_dir
        .path()
        .strip_prefix(&cwd)
        .expect("decoy directory should be relative to cwd");

    for command in ["npm.cmd", "pnpm.cmd", "bun.exe"] {
        std::fs::write(decoy_dir.path().join(command), "decoy").expect("write cwd-relative decoy");
        std::fs::write(trusted_dir.path().join(command), "trusted")
            .expect("write trusted PATH command");
        let path_env = std::env::join_paths([relative_decoy_dir, trusted_dir.path()])
            .expect("join synthetic PATH");

        let resolved = resolve_windows_update_command_from_path(command, &path_env)
            .expect("resolve update command");

        assert_eq!(resolved, trusted_dir.path().join(command));
    }

    let cwd_decoy = tempfile::Builder::new()
        .suffix(".cmd")
        .tempfile_in(&cwd)
        .expect("cwd-local decoy");
    let command = cwd_decoy
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("decoy filename");
    let relative_only_path_env = std::env::join_paths(["."]).expect("join relative-only PATH");
    let err = resolve_windows_update_command_from_path(command, &relative_only_path_env)
        .expect_err("relative-only PATH should not resolve a cwd command");

    assert_eq!(
        err.to_string(),
        format!(
            "Could not find an absolute update command `{command}` on PATH. Please update manually: https://developers.openai.com/codex/cli/"
        )
    );
}

#[tokio::test]
async fn updater_http_client_factory_honors_respect_system_proxy() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .cli_overrides(vec![(
            "features.respect_system_proxy".to_string(),
            toml::Value::Boolean(true),
        )])
        .build()
        .await
        .expect("config should load");

    assert_eq!(
        updater_http_client_factory(Ok(config))
            .expect("updater configuration")
            .outbound_proxy_policy(),
        codex_http_client::OutboundProxyPolicy::RespectSystemProxy
    );
}

#[test]
fn updater_http_client_factory_propagates_config_errors() {
    assert_eq!(
        updater_http_client_factory(Err(anyhow::anyhow!("invalid config")))
            .expect_err("invalid configuration must prevent updates")
            .to_string(),
        "invalid config"
    );
}

#[tokio::test]
async fn updater_http_client_factory_enforces_application_requirements() {
    let home = tempfile::tempdir().expect("temporary Codex home");
    let requirements = home.path().join("requirements.toml");
    std::fs::write(
        &requirements,
        r#"
[application.network]
enabled = true
[application.network.domains]
"bedrock-mantle.us-gov-west-1.api.aws" = "allow"
"chatgpt.com" = "deny"
"releases.openai.com" = "deny"
"#,
    )
    .expect("write managed requirements");
    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.system_requirements_path = Some(requirements);
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(overrides)
        .build()
        .await
        .expect("load managed configuration");
    let factory = updater_http_client_factory(Ok(config)).expect("updater configuration");
    for host in ["chatgpt.com", "releases.openai.com", "unlisted.example"] {
        assert_eq!(
            factory
                .network_policy()
                .acquire(&format!("https://{host}/").parse().expect("URL"))
                .err(),
            Some(codex_http_client::NetworkPolicyDenied::Destination)
        );
    }
    assert!(
        factory
            .network_policy()
            .acquire(
                &"https://bedrock-mantle.us-gov-west-1.api.aws/"
                    .parse()
                    .expect("URL")
            )
            .is_ok()
    );
}

fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
    let cli = MultitoolCli::try_parse_from(args).expect("parse");
    let MultitoolCli {
        mut interactive,
        config_overrides: mut root_overrides,
        subcommand,
        feature_toggles: _,
        remote: _,
    } = cli;
    interactive
        .shared
        .take_auto_review_config_overrides(&mut root_overrides);

    let Subcommand::Resume(ResumeCommand {
        session_id,
        last,
        all,
        include_non_interactive,
        remote: _,
        config_overrides: resume_cli,
    }) = subcommand.expect("resume present")
    else {
        unreachable!()
    };
    let SessionTuiCli(resume_cli) = resume_cli;

    finalize_resume_interactive(
        interactive,
        root_overrides,
        session_id,
        last,
        all,
        include_non_interactive,
        resume_cli,
    )
}

fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
    let cli = MultitoolCli::try_parse_from(args).expect("parse");
    let MultitoolCli {
        mut interactive,
        config_overrides: mut root_overrides,
        subcommand,
        feature_toggles: _,
        remote: _,
    } = cli;
    interactive
        .shared
        .take_auto_review_config_overrides(&mut root_overrides);

    let Subcommand::Fork(ForkCommand {
        session_id,
        last,
        all,
        remote: _,
        config_overrides: fork_cli,
    }) = subcommand.expect("fork present")
    else {
        unreachable!()
    };
    let SessionTuiCli(fork_cli) = fork_cli;

    finalize_fork_interactive(interactive, root_overrides, session_id, last, all, fork_cli)
}

fn finalize_exec_from_args(args: &[&str]) -> ExecCli {
    let mut cli = MultitoolCli::try_parse_from(args).expect("parse");
    cli.interactive
        .shared
        .take_auto_review_config_overrides(&mut cli.config_overrides);
    let Some(Subcommand::Exec(mut exec)) = cli.subcommand else {
        panic!("expected exec subcommand");
    };
    exec.shared
        .inherit_exec_root_options(&cli.interactive.shared);
    prepend_config_flags(&mut exec.config_overrides, cli.config_overrides);
    exec.shared
        .take_auto_review_config_overrides(&mut exec.config_overrides);
    exec
}

fn finalize_archive_from_args(args: &[&str]) -> (String, TuiCli, InteractiveRemoteOptions) {
    let cli = MultitoolCli::try_parse_from(args).expect("parse");
    let MultitoolCli {
        interactive,
        config_overrides: root_overrides,
        subcommand,
        feature_toggles: _,
        remote: _,
    } = cli;

    let Subcommand::Archive(SessionArchiveCommand {
        target,
        remote,
        config_overrides: archive_cli,
    }) = subcommand.expect("archive present")
    else {
        unreachable!()
    };

    (
        target,
        finalize_session_archive_interactive(interactive, root_overrides, archive_cli),
        remote,
    )
}

fn profile_v2_for_args(args: &[&str]) -> anyhow::Result<Option<String>> {
    let cli = MultitoolCli::try_parse_from(args).expect("parse");
    let Some(subcommand) = cli.subcommand.as_ref() else {
        return Ok(cli
            .interactive
            .config_profile_v2
            .as_ref()
            .map(std::string::ToString::to_string));
    };
    Ok(profile_v2_for_subcommand(&cli.interactive, subcommand)?.map(ToString::to_string))
}

#[test]
fn profile_loader_overrides_use_explicit_codex_home() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let profile: ProfileV2Name = "work".parse()?;

    let overrides = loader_overrides_for_profile_at_codex_home(Some(&profile), codex_home.path());

    assert_eq!(
        overrides.user_config_path,
        Some(resolve_profile_v2_config_path(codex_home.path(), &profile))
    );
    assert_eq!(overrides.user_config_profile, Some(profile));
    Ok(())
}

#[test]
fn profile_v2_is_rejected_for_config_management_subcommands() {
    assert!(profile_v2_for_args(&["codex", "--profile", "work", "features", "list"]).is_err());
}

#[test]
fn profile_v2_is_allowed_for_runtime_subcommands() {
    assert_eq!(
        profile_v2_for_args(&["codex", "--profile", "work", "resume"])
            .expect("resume supports profile-v2")
            .as_deref(),
        Some("work")
    );
    assert_eq!(
        profile_v2_for_args(&["codex", "--profile", "work", "debug", "prompt-input"])
            .expect("debug prompt-input supports profile-v2")
            .as_deref(),
        Some("work")
    );
    assert_eq!(
        profile_v2_for_args(&["codex", "--profile", "work", "mcp", "list"])
            .expect("mcp supports profile-v2")
            .as_deref(),
        Some("work")
    );
    assert_eq!(
        profile_v2_for_args(&["codex", "--profile", "work", "sandbox"])
            .expect("sandbox supports config profile")
            .as_deref(),
        Some("work")
    );
}

#[test]
fn import_remains_an_interactive_prompt() {
    let cli = MultitoolCli::try_parse_from(["codex", "import"]).expect("parse");

    assert!(cli.subcommand.is_none());
    assert_eq!(cli.interactive.prompt.as_deref(), Some("import"));
}

#[test]
fn profile_v2_rejects_non_plain_names_at_parse_time() {
    assert!(MultitoolCli::try_parse_from(["codex", "--profile", "nested/work", "resume"]).is_err());
}

#[test]
fn worktree_flag_supports_interactive_exec_and_explicit_fork_positions() {
    let arguments = [
        vec!["codex", "--worktree"],
        vec!["codex", "--worktree", "hello"],
        vec!["codex", "--worktree", "exec", "hello"],
        vec!["codex", "exec", "--worktree", "hello"],
        vec![
            "codex",
            "fork",
            "--worktree",
            "019f1234-5678-7000-8000-000000000001",
        ],
        vec![
            "codex",
            "exec",
            "fork",
            "--worktree",
            "019f1234-5678-7000-8000-000000000001",
        ],
    ];

    for arguments in arguments {
        let cli = MultitoolCli::try_parse_from(&arguments).expect("parse worktree command");
        assert!(
            reject_unsupported_worktree_for_subcommand(
                cli.interactive.shared.worktree,
                &cli.subcommand,
            )
            .is_ok(),
            "supported worktree command should be accepted: {arguments:?}",
        );
    }
}

#[test]
fn worktree_flag_rejects_unsupported_session_and_management_commands() {
    let arguments = [
        vec!["codex", "--worktree", "login"],
        vec!["codex", "fork", "--worktree"],
        vec!["codex", "fork", "--worktree", "--last"],
        vec!["codex", "exec", "resume", "--worktree", "session"],
        vec!["codex", "exec", "review", "--worktree"],
        vec!["codex", "resume", "--worktree", "session"],
        vec!["codex", "archive", "session", "--worktree"],
        vec![
            "codex",
            "queue",
            "--thread",
            "session",
            "--message",
            "hi",
            "--worktree",
        ],
    ];

    let mut errors = Vec::new();
    for arguments in arguments {
        let cli = MultitoolCli::try_parse_from(&arguments).expect("parse shared worktree flag");
        let error = reject_unsupported_worktree_for_subcommand(
            cli.interactive.shared.worktree,
            &cli.subcommand,
        )
        .expect_err("unsupported worktree command");
        errors.push(format!("{}: {error}", arguments.join(" ")));
    }
    insta::assert_snapshot!("unsupported_worktree_commands", errors.join("\n"));
}

#[test]
fn exec_resume_last_accepts_prompt_positional() {
    let cli = MultitoolCli::try_parse_from(["codex", "exec", "--json", "resume", "--last", "2+2"])
        .expect("parse should succeed");

    let Some(Subcommand::Exec(exec)) = cli.subcommand else {
        panic!("expected exec subcommand");
    };
    let Some(codex_exec::Command::Resume(args)) = exec.command else {
        panic!("expected exec resume");
    };

    assert!(args.last);
    assert_eq!(args.session_id, None);
    assert_eq!(args.prompt.as_deref(), Some("2+2"));
}

#[test]
fn exec_resume_accepts_output_flags_after_subcommand() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "exec",
        "resume",
        "session-123",
        "-o",
        "/tmp/resume-output.md",
        "--output-schema",
        "/tmp/schema.json",
        "re-review",
    ])
    .expect("parse should succeed");

    let Some(Subcommand::Exec(exec)) = cli.subcommand else {
        panic!("expected exec subcommand");
    };
    let Some(codex_exec::Command::Resume(args)) = exec.command else {
        panic!("expected exec resume");
    };

    assert_eq!(
        exec.last_message_file,
        Some(std::path::PathBuf::from("/tmp/resume-output.md"))
    );
    assert_eq!(
        exec.output_schema,
        Some(std::path::PathBuf::from("/tmp/schema.json"))
    );
    assert_eq!(args.session_id.as_deref(), Some("session-123"));
    assert_eq!(args.prompt.as_deref(), Some("re-review"));
}

#[test]
fn dangerous_bypass_conflicts_with_approval_policy() {
    let err = MultitoolCli::try_parse_from([
        "codex",
        "--dangerously-bypass-approvals-and-sandbox",
        "--ask-for-approval",
        "on-request",
    ])
    .expect_err("conflicting permission flags should be rejected");

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn approve_for_me_configures_interactive_mode() {
    for flag in ["--approve-for-me", "--not-so-yolo"] {
        let mut cli = MultitoolCli::try_parse_from(["codex", flag]).expect("parse flag");

        assert!(cli.interactive.auto_review);
        cli.interactive
            .shared
            .take_auto_review_config_overrides(&mut cli.interactive.config_overrides);
        assert_eq!(
            cli.interactive.config_overrides.raw_overrides,
            vec![
                r#"approvals_reviewer="auto_review""#.to_string(),
                r#"approval_policy="on-request""#.to_string(),
                r#"sandbox_mode="workspace-write""#.to_string(),
            ]
        );
        assert!(!cli.interactive.auto_review);
    }
}

#[test]
fn not_so_yolo_alias_is_hidden_from_help() {
    for args in [&["codex", "--help"][..], &["codex", "exec", "--help"][..]] {
        let help = help_from_args(args);

        assert!(!help.contains("--not-so-yolo"), "{help}");
    }
}

#[test]
fn approve_for_me_defaults_propagate_from_root_to_exec() {
    let exec = finalize_exec_from_args(&["codex", "--approve-for-me", "exec", "summarize"]);

    assert_eq!(
        exec.config_overrides.raw_overrides,
        vec![
            r#"approvals_reviewer="auto_review""#.to_string(),
            r#"approval_policy="on-request""#.to_string(),
            r#"sandbox_mode="workspace-write""#.to_string(),
        ]
    );
    assert!(exec.sandbox_mode.is_none());
}

#[test]
fn later_exec_sandbox_partially_overrides_approve_for_me() {
    let exec = finalize_exec_from_args(&[
        "codex",
        "--approve-for-me",
        "exec",
        "--sandbox",
        "read-only",
    ]);

    assert_matches!(
        exec.sandbox_mode,
        Some(codex_utils_cli::SandboxModeCliArg::ReadOnly)
    );
    assert_eq!(
        exec.config_overrides.raw_overrides,
        vec![
            r#"approvals_reviewer="auto_review""#.to_string(),
            r#"approval_policy="on-request""#.to_string(),
            r#"sandbox_mode="workspace-write""#.to_string(),
        ]
    );
}

#[test]
fn later_approve_for_me_overrides_root_exec_sandbox() {
    let exec = finalize_exec_from_args(&[
        "codex",
        "--sandbox",
        "read-only",
        "exec",
        "--approve-for-me",
    ]);

    assert!(exec.sandbox_mode.is_none());
    assert_eq!(
        exec.config_overrides.raw_overrides,
        vec![
            r#"approvals_reviewer="auto_review""#.to_string(),
            r#"approval_policy="on-request""#.to_string(),
            r#"sandbox_mode="workspace-write""#.to_string(),
        ]
    );
}

#[test]
fn later_resume_approval_policy_partially_overrides_approve_for_me() {
    let interactive = finalize_resume_from_args(&[
        "codex",
        "--approve-for-me",
        "resume",
        "--ask-for-approval",
        "never",
    ]);

    assert_matches!(
        interactive.approval_policy,
        Some(codex_utils_cli::ApprovalModeCliArg::Never)
    );
    assert_eq!(
        interactive.config_overrides.raw_overrides,
        vec![
            r#"approvals_reviewer="auto_review""#.to_string(),
            r#"approval_policy="on-request""#.to_string(),
            r#"sandbox_mode="workspace-write""#.to_string(),
        ]
    );
}

#[test]
fn later_approve_for_me_overrides_root_tui_approval_policy() {
    let interactive = finalize_resume_from_args(&[
        "codex",
        "--ask-for-approval",
        "never",
        "resume",
        "--approve-for-me",
    ]);

    assert!(interactive.approval_policy.is_none());
    assert_eq!(
        interactive.config_overrides.raw_overrides,
        vec![
            r#"approvals_reviewer="auto_review""#.to_string(),
            r#"approval_policy="on-request""#.to_string(),
            r#"sandbox_mode="workspace-write""#.to_string(),
        ]
    );
}

#[test]
fn approve_for_me_conflicts_with_explicit_interactive_permissions() {
    for conflicting_args in [
        vec!["--sandbox", "read-only"],
        vec!["--ask-for-approval", "on-request"],
        vec!["--dangerously-bypass-approvals-and-sandbox"],
    ] {
        let mut args = vec!["codex", "--approve-for-me"];
        args.extend(conflicting_args);

        let error =
            MultitoolCli::try_parse_from(args).expect_err("permission flags should conflict");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}

fn app_server_from_args(args: &[&str]) -> AppServerCommand {
    let cli = MultitoolCli::try_parse_from(args).expect("parse");
    let Subcommand::AppServer(app_server) = cli.subcommand.expect("app-server present") else {
        unreachable!()
    };
    app_server
}

fn default_app_server_socket_path() -> AbsolutePathBuf {
    let codex_home = find_codex_home().expect("codex home");
    codex_app_server::app_server_control_socket_path(&codex_home)
        .expect("default app-server socket path")
}

#[test]
fn debug_prompt_input_parses_prompt_and_images() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "debug",
        "prompt-input",
        "hello",
        "--image",
        "/tmp/a.png,/tmp/b.png",
    ])
    .expect("parse");

    let Some(Subcommand::Debug(DebugCommand {
        subcommand: DebugSubcommand::PromptInput(cmd),
    })) = cli.subcommand
    else {
        panic!("expected debug prompt-input subcommand");
    };

    assert_eq!(cmd.prompt.as_deref(), Some("hello"));
    assert_eq!(
        cmd.images,
        vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")]
    );
}

#[test]
fn debug_models_parses_bundled_flag() {
    let cli =
        MultitoolCli::try_parse_from(["codex", "debug", "models", "--bundled"]).expect("parse");

    let Some(Subcommand::Debug(DebugCommand {
        subcommand: DebugSubcommand::Models(cmd),
    })) = cli.subcommand
    else {
        panic!("expected debug models subcommand");
    };

    assert!(cmd.bundled);
}

#[test]
fn responses_subcommand_is_not_registered() {
    let command = MultitoolCli::command();
    assert!(
        command
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "responses")
    );
}

fn help_from_args(args: &[&str]) -> String {
    let err = MultitoolCli::try_parse_from(args).expect_err("help should short-circuit");
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    err.to_string()
}

#[test]
fn queue_help_lists_supported_options() {
    let help = help_from_args(&["codex", "queue", "--help"]);
    let (general, local) = help
        .split_once("Local server options (without --remote):")
        .unwrap();
    assert!(general.contains("--message <TEXT>"));
    assert!(!general.contains("--model"));
    assert!(!general.contains("-c model="));
    let local_flags = local
        .lines()
        .filter(|line| line.starts_with("  -") || line.starts_with("      --"))
        .map(str::trim)
        .collect::<Vec<_>>();
    insta::assert_snapshot!(local_flags.join("\n"), @"-C, --cd <DIR>");
}

#[test]
fn tcp_tunnel_is_hidden_and_accepts_its_control_input_contract() {
    let root_help = help_from_args(&["codex", "--help"]);
    assert!(!root_help.contains("tcp-tunnel"), "{root_help}");
    let help = help_from_args(&["codex", "tcp-tunnel", "--help"]);
    for option in [
        "--target",
        "--auth-token-stdin",
        "--auth-token-updates-stdin",
        "--connect-headers-stdin",
    ] {
        assert!(help.contains(option), "{help}");
    }
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "tcp-tunnel",
        "--proxy-url",
        "https://proxy.example.org",
        "--proxy-origins-file",
        "/tmp/approved-origins",
        "--target",
        "[::1]:22",
        "--auth-token-stdin",
        "--auth-token-updates-stdin",
        "--connect-headers-stdin",
    ])
    .expect("generic tunnel should parse");
    assert!(matches!(cli.subcommand, Some(Subcommand::TcpTunnel(_))));
}

#[test]
fn plugin_marketplace_help_uses_plugin_namespace() {
    let help = help_from_args(&["codex", "plugin", "marketplace", "--help"]);
    assert!(
        help.contains("Usage: codex plugin marketplace [OPTIONS] <COMMAND>"),
        "{help}"
    );

    for (subcommand, usage) in [
        ("add", "Usage: codex plugin marketplace add"),
        ("list", "Usage: codex plugin marketplace list"),
        ("upgrade", "Usage: codex plugin marketplace upgrade"),
        ("remove", "Usage: codex plugin marketplace remove"),
    ] {
        let help = help_from_args(&["codex", "plugin", "marketplace", subcommand, "--help"]);
        assert!(help.contains(usage), "{help}");
    }
}

#[test]
fn plugin_marketplace_add_parses_under_plugin() {
    let cli = MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "add", "owner/repo"])
        .expect("parse");

    assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
}

#[test]
fn plugin_marketplace_upgrade_parses_under_plugin() {
    let cli = MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "upgrade", "debug"])
        .expect("parse");

    assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
}

#[test]
fn plugin_add_parses_under_plugin() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "plugin",
        "add",
        "sample",
        "--marketplace",
        "debug",
    ])
    .expect("parse");

    assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
}

#[test]
fn plugin_list_parses_under_plugin() {
    let cli = MultitoolCli::try_parse_from(["codex", "plugin", "list", "--marketplace", "debug"])
        .expect("parse");

    assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
}

#[test]
fn plugin_remove_parses_under_plugin() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "plugin",
        "remove",
        "sample",
        "--marketplace",
        "debug",
    ])
    .expect("parse");

    assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
}

#[test]
fn update_parses_as_update_subcommand() {
    let cli = MultitoolCli::try_parse_from(["codex", "update"]).expect("parse");
    assert!(matches!(cli.subcommand, Some(Subcommand::Update)));
}

#[test]
fn archive_merges_scoped_tui_flags() {
    let (target, interactive, remote) = finalize_archive_from_args(
        [
            "codex",
            "-C",
            "/root",
            "archive",
            "--remote",
            "unix://archive.sock",
            "--strict-config",
            "--dangerously-bypass-hook-trust",
            "-m",
            "gpt-5.1-test",
            "-p",
            "work",
            "-C",
            "/archive",
            "my-thread",
        ]
        .as_ref(),
    );

    assert_eq!(target, "my-thread");
    assert_eq!(remote.remote.as_deref(), Some("unix://archive.sock"));
    assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
    assert_eq!(interactive.config_profile_v2.as_deref(), Some("work"));
    assert_eq!(
        interactive.cwd.as_deref(),
        Some(std::path::Path::new("/archive"))
    );
    assert!(interactive.strict_config);
    assert!(interactive.bypass_hook_trust);
}

#[test]
fn delete_force_requires_uuid() {
    assert!(delete_action("123e4567-e89b-12d3-a456-426614174000", /*force*/ true).is_ok());

    let err = delete_action("my-thread", /*force*/ true).expect_err("name should require prompt");
    assert_eq!(
        err.to_string(),
        "--force requires a session UUID; names must be confirmed interactively"
    );
}

#[test]
fn sandbox_uninstall_respects_command_separator() {
    let cli = MultitoolCli::try_parse_from(["codex", "sandbox", "uninstall"]).expect("parse");
    assert!(matches!(
        cli.subcommand,
        Some(Subcommand::Sandbox(SandboxCommand {
            subcommand: Some(SandboxSubcommand::Uninstall(_)),
            ..
        }))
    ));

    for args in [
        vec!["codex", "sandbox", "--", "uninstall", "--help"],
        vec![
            "codex",
            "sandbox",
            "--profile",
            "uninstall",
            "--",
            "uninstall",
            "--help",
        ],
    ] {
        let cli = MultitoolCli::try_parse_from(args).expect("parse forwarded command");
        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };
        assert!(command.subcommand.is_none());
        assert_eq!(command.host.command, vec!["uninstall", "--help"]);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
fn sandbox_parses_permission_profile() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "sandbox",
        "--permission-profile",
        ":workspace",
        "--",
        "echo",
    ])
    .expect("parse");

    let Some(Subcommand::Sandbox(SandboxCommand { host: command, .. })) = cli.subcommand else {
        panic!("expected sandbox command");
    };

    assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
    assert_eq!(command.command, vec!["echo"]);
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
fn sandbox_parses_legacy_permissions_profile_alias() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "sandbox",
        "--permissions-profile",
        ":workspace",
        "--",
        "echo",
    ])
    .expect("parse");

    let Some(Subcommand::Sandbox(SandboxCommand { host: command, .. })) = cli.subcommand else {
        panic!("expected sandbox command");
    };

    assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
    assert_eq!(command.command, vec!["echo"]);
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
fn sandbox_help_only_shows_singular_permission_profile() {
    let help = help_from_args(&["codex", "sandbox", "--help"]);
    assert!(help.contains("--permission-profile"), "{help}");
    assert!(!help.contains("--permissions-profile"), "{help}");
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
fn sandbox_parses_permissions_profile_short_alias() {
    let cli = MultitoolCli::try_parse_from(["codex", "sandbox", "-P", ":workspace", "--", "echo"])
        .expect("parse");

    let Some(Subcommand::Sandbox(SandboxCommand { host: command, .. })) = cli.subcommand else {
        panic!("expected sandbox command");
    };

    assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
    assert_eq!(command.command, vec!["echo"]);
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
fn sandbox_parses_config_profile() {
    let cli = MultitoolCli::try_parse_from(["codex", "sandbox", "--profile", "work", "--", "echo"])
        .expect("parse");

    let Some(Subcommand::Sandbox(SandboxCommand { host: command, .. })) = cli.subcommand else {
        panic!("expected sandbox command");
    };

    assert_eq!(command.config_profile.as_deref(), Some("work"));
    assert_eq!(command.command, vec!["echo"]);
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[test]
fn sandbox_rejects_explicit_profile_controls_without_profile() {
    let err = MultitoolCli::try_parse_from(["codex", "sandbox", "-C", "/tmp"])
        .expect_err("parse should fail");

    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
}

#[test]
fn plugin_marketplace_remove_parses_under_plugin() {
    let cli = MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "remove", "debug"])
        .expect("parse");

    assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
}

#[test]
fn marketplace_no_longer_parses_at_top_level() {
    let add_result = MultitoolCli::try_parse_from(["codex", "marketplace", "add", "owner/repo"]);
    assert!(add_result.is_err());

    let upgrade_result = MultitoolCli::try_parse_from(["codex", "marketplace", "upgrade", "debug"]);
    assert!(upgrade_result.is_err());

    let remove_result = MultitoolCli::try_parse_from(["codex", "marketplace", "remove", "debug"]);
    assert!(remove_result.is_err());
}

fn sample_exit_info(conversation_id: Option<&str>, thread_name: Option<&str>) -> AppExitInfo {
    let token_usage = TokenUsage {
        output_tokens: 2,
        total_tokens: 2,
        ..Default::default()
    };
    let thread_id = conversation_id
        .map(ThreadId::from_string)
        .map(Result::unwrap);
    AppExitInfo {
        token_usage,
        thread_id,
        resume_hint: thread_id.map(|thread_id| codex_tui::ResumableThread {
            thread_id,
            thread_name: thread_name.map(str::to_string),
        }),
        disconnect_info: None,
        update_action: None,
        exit_reason: ExitReason::UserRequested,
    }
}

#[test]
fn format_exit_messages_skips_zero_usage() {
    let exit_info = AppExitInfo {
        token_usage: TokenUsage::default(),
        thread_id: None,
        resume_hint: None,
        disconnect_info: None,
        update_action: None,
        exit_reason: ExitReason::UserRequested,
    };
    let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
    assert!(lines.is_empty());
}

#[tokio::test]
async fn format_exit_messages_preserves_auth_env_through_tui_runner() {
    let exit_info = run_tui_with_recovery(
        || async {
            let mut exit_info = sample_exit_info(
                Some("123e4567-e89b-12d3-a456-426614174000"),
                /*thread_name*/ None,
            );
            exit_info.disconnect_info = Some(codex_tui::DisconnectInfo {
                command: vec![
                    "codex".to_string(),
                    "--remote".to_string(),
                    "wss://example.com:443/".to_string(),
                ],
                stop_hint: "press ctrl + x".to_string(),
            });
            Ok(exit_info)
        },
        Some("CODEX_REMOTE_TOKEN"),
    )
    .await
    .unwrap();
    assert_eq!(
        exit_info.format_exit_messages(/*color_enabled*/ false),
        vec![
            "Disconnected from this task. Any running work continues.",
            "To reconnect, run:",
            "  codex --remote wss://example.com:443/ --remote-auth-token-env CODEX_REMOTE_TOKEN resume 123e4567-e89b-12d3-a456-426614174000",
            "Stop the current turn: run codex --remote wss://example.com:443/ --remote-auth-token-env CODEX_REMOTE_TOKEN agents, select this task, and press ctrl + x.",
            "Token usage so far: total=2 input=0 output=2",
        ]
    );
}

#[test]
fn format_exit_messages_includes_session_id_without_resume_hint() {
    let mut exit_info = sample_exit_info(
        Some("123e4567-e89b-12d3-a456-426614174000"),
        /*thread_name*/ None,
    );
    exit_info.token_usage = TokenUsage::default();
    exit_info.resume_hint = None;
    let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
    insta::assert_snapshot!(lines.join("\n"), @"Session ID: 123e4567-e89b-12d3-a456-426614174000");
}

#[test]
fn format_exit_messages_includes_session_id_for_fatal_exit_without_resume_hint() {
    let exit_info = AppExitInfo {
        token_usage: TokenUsage::default(),
        thread_id: Some(ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap()),
        resume_hint: None,
        disconnect_info: None,
        update_action: None,
        exit_reason: ExitReason::Fatal("boom".to_string()),
    };
    let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
    assert_eq!(
        lines,
        vec!["Session ID: 123e4567-e89b-12d3-a456-426614174000".to_string()]
    );
}

#[test]
fn format_exit_messages_includes_resume_hint_for_fatal_exit() {
    let mut exit_info = sample_exit_info(
        Some("123e4567-e89b-12d3-a456-426614174000"),
        /*thread_name*/ None,
    );
    exit_info.exit_reason = ExitReason::Fatal("boom".to_string());
    let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
    assert_eq!(
        lines,
        vec![
            "Token usage: total=2 input=0 output=2".to_string(),
            "To continue this session, run:".to_string(),
            "  codex resume 123e4567-e89b-12d3-a456-426614174000".to_string(),
        ]
    );
}

#[test]
fn format_exit_messages_includes_resume_hint_without_color() {
    insta::allow_duplicates! {
        for thread_name in [None, Some("")] {
            let exit_info =
                sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), thread_name);
            let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
            insta::assert_snapshot!(lines.join("\n"), @"
                Token usage: total=2 input=0 output=2
                To continue this session, run:
                  codex resume 123e4567-e89b-12d3-a456-426614174000
                ");
        }
    }
}

#[test]
fn format_exit_messages_applies_color_when_enabled() {
    let exit_info = sample_exit_info(
        Some("123e4567-e89b-12d3-a456-426614174000"),
        /*thread_name*/ None,
    );
    let lines = exit_info.format_exit_messages(/*color_enabled*/ true);
    assert_eq!(
        lines,
        vec![
            "Token usage: total=2 input=0 output=2",
            "To continue this session, run:",
            "  \u{1b}[36mcodex resume 123e4567-e89b-12d3-a456-426614174000\u{1b}[39m",
        ]
    );
}

#[test]
fn format_exit_messages_names_picker_item_when_thread_has_name() {
    let exit_info = sample_exit_info(
        Some("123e4567-e89b-12d3-a456-426614174000"),
        Some("my-thread"),
    );
    let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
    insta::assert_snapshot!(lines.join("\n"), @"
        Token usage: total=2 input=0 output=2
        To continue this session, run:
          codex resume 123e4567-e89b-12d3-a456-426614174000
        Or run codex resume and select my-thread.
        ");
}

#[test]
fn format_exit_messages_colors_commands_and_thread_name() {
    let exit_info = sample_exit_info(
        Some("123e4567-e89b-12d3-a456-426614174000"),
        Some("my-thread"),
    );
    let lines = exit_info.format_exit_messages(/*color_enabled*/ true);
    assert_eq!(
        lines,
        vec![
            "Token usage: total=2 input=0 output=2",
            "To continue this session, run:",
            "  \u{1b}[36mcodex resume 123e4567-e89b-12d3-a456-426614174000\u{1b}[39m",
            "Or run \u{1b}[36mcodex resume\u{1b}[39m and select \u{1b}[36mmy-thread\u{1b}[39m.",
        ]
    );
}

#[test]
fn resume_model_flag_applies_when_no_root_flags() {
    let interactive = finalize_resume_from_args(["codex", "resume", "-m", "gpt-5.1-test"].as_ref());

    assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
    assert!(interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
}

#[test]
fn resume_and_fork_preserve_no_daemon() {
    for (command, finalize) in [
        ("resume", finalize_resume_from_args as fn(&[&str]) -> TuiCli),
        ("fork", finalize_fork_from_args as fn(&[&str]) -> TuiCli),
    ] {
        for args in [
            ["codex", "--no-daemon", command, "--last"],
            ["codex", command, "--last", "--no-daemon"],
        ] {
            assert!(finalize(&args).no_daemon);
        }
    }
}

#[test]
fn resume_and_fork_preserve_no_alt_screen() {
    for (command, finalize) in [
        ("resume", finalize_resume_from_args as fn(&[&str]) -> TuiCli),
        ("fork", finalize_fork_from_args as fn(&[&str]) -> TuiCli),
    ] {
        assert!(finalize(&["codex", command, "--no-alt-screen"]).no_alt_screen);
        assert!(finalize(&["codex", "--no-alt-screen", command]).no_alt_screen);
        assert!(!finalize(&["codex", command]).no_alt_screen);
    }
}

#[test]
fn resume_picker_logic_none_and_not_last() {
    let interactive = finalize_resume_from_args(["codex", "resume"].as_ref());
    assert!(interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
    assert!(!interactive.resume_show_all);
}

#[test]
fn resume_picker_logic_last() {
    let interactive = finalize_resume_from_args(["codex", "resume", "--last"].as_ref());
    assert!(!interactive.resume_picker);
    assert!(interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
    assert!(!interactive.resume_show_all);
}

#[test]
fn resume_last_accepts_prompt_positional() {
    let interactive =
        finalize_resume_from_args(["codex", "resume", "--last", "/compact focus on auth"].as_ref());

    assert!(!interactive.resume_picker);
    assert!(interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
    assert_eq!(
        interactive.prompt.as_deref(),
        Some("/compact focus on auth")
    );
}

#[test]
fn resume_last_rejects_explicit_session_and_prompt() {
    let err = MultitoolCli::try_parse_from(["codex", "resume", "--last", "1234", "continue here"])
        .expect_err("--last with an explicit session and prompt should be rejected");

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn resume_picker_logic_with_session_id() {
    let interactive = finalize_resume_from_args(["codex", "resume", "1234"].as_ref());
    assert!(!interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
    assert!(!interactive.resume_show_all);
}

#[test]
fn resume_with_session_id_accepts_prompt_positional() {
    let interactive =
        finalize_resume_from_args(["codex", "resume", "1234", "continue here"].as_ref());

    assert!(!interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
    assert_eq!(interactive.prompt.as_deref(), Some("continue here"));
}

#[test]
fn resume_all_flag_sets_show_all() {
    let interactive = finalize_resume_from_args(["codex", "resume", "--all"].as_ref());
    assert!(interactive.resume_picker);
    assert!(interactive.resume_show_all);
}

#[test]
fn resume_include_non_interactive_flag_sets_source_filter_override() {
    let interactive =
        finalize_resume_from_args(["codex", "resume", "--include-non-interactive"].as_ref());

    assert!(interactive.resume_picker);
    assert!(interactive.resume_include_non_interactive);
}

#[test]
fn resume_merges_option_flags() {
    let interactive = finalize_resume_from_args(
        [
            "codex",
            "resume",
            "sid",
            "--oss",
            "--search",
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
            "-m",
            "gpt-5.1-test",
            "-p",
            "my-config",
            "-C",
            "/tmp",
            "--strict-config",
            "-i",
            "/tmp/a.png,/tmp/b.png",
        ]
        .as_ref(),
    );

    assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
    assert!(interactive.oss);
    assert_eq!(interactive.config_profile_v2.as_deref(), Some("my-config"));
    assert_matches!(
        interactive.sandbox_mode,
        Some(codex_utils_cli::SandboxModeCliArg::WorkspaceWrite)
    );
    assert_matches!(
        interactive.approval_policy,
        Some(codex_utils_cli::ApprovalModeCliArg::OnRequest)
    );
    assert_eq!(
        interactive.cwd.as_deref(),
        Some(std::path::Path::new("/tmp"))
    );
    assert!(interactive.web_search);
    assert!(interactive.strict_config);
    let has_a = interactive
        .images
        .iter()
        .any(|p| p == std::path::Path::new("/tmp/a.png"));
    let has_b = interactive
        .images
        .iter()
        .any(|p| p == std::path::Path::new("/tmp/b.png"));
    assert!(has_a && has_b);
    assert!(!interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id.as_deref(), Some("sid"));
}

#[test]
fn resume_merges_dangerously_bypass_flag() {
    let interactive = finalize_resume_from_args(
        [
            "codex",
            "resume",
            "--dangerously-bypass-approvals-and-sandbox",
        ]
        .as_ref(),
    );
    assert!(interactive.dangerously_bypass_approvals_and_sandbox);
    assert!(interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
}

#[test]
fn resume_merges_bypass_hook_trust_flag() {
    let interactive =
        finalize_resume_from_args(["codex", "resume", "--dangerously-bypass-hook-trust"].as_ref());

    assert!(interactive.bypass_hook_trust);
    assert!(interactive.resume_picker);
    assert!(!interactive.resume_last);
    assert_eq!(interactive.resume_session_id, None);
}

#[test]
fn fork_picker_logic_none_and_not_last() {
    let interactive = finalize_fork_from_args(["codex", "fork"].as_ref());
    assert!(interactive.fork_picker);
    assert!(!interactive.fork_last);
    assert_eq!(interactive.fork_session_id, None);
    assert!(!interactive.fork_show_all);
}

#[test]
fn fork_picker_logic_last() {
    let interactive = finalize_fork_from_args(["codex", "fork", "--last"].as_ref());
    assert!(!interactive.fork_picker);
    assert!(interactive.fork_last);
    assert_eq!(interactive.fork_session_id, None);
    assert!(!interactive.fork_show_all);
}

#[test]
fn fork_last_accepts_prompt_positional() {
    let interactive =
        finalize_fork_from_args(["codex", "fork", "--last", "/compact focus on auth"].as_ref());

    assert!(!interactive.fork_picker);
    assert!(interactive.fork_last);
    assert_eq!(interactive.fork_session_id, None);
    assert_eq!(
        interactive.prompt.as_deref(),
        Some("/compact focus on auth")
    );
}

#[test]
fn fork_last_rejects_explicit_session_and_prompt() {
    let err = MultitoolCli::try_parse_from(["codex", "fork", "--last", "1234", "continue here"])
        .expect_err("--last with an explicit session and prompt should be rejected");

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn fork_picker_logic_with_session_id() {
    let interactive = finalize_fork_from_args(["codex", "fork", "1234"].as_ref());
    assert!(!interactive.fork_picker);
    assert!(!interactive.fork_last);
    assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
    assert!(!interactive.fork_show_all);
}

#[test]
fn fork_with_session_id_accepts_prompt_positional() {
    let interactive = finalize_fork_from_args(["codex", "fork", "1234", "continue here"].as_ref());

    assert!(!interactive.fork_picker);
    assert!(!interactive.fork_last);
    assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
    assert_eq!(interactive.prompt.as_deref(), Some("continue here"));
}

#[test]
fn fork_all_flag_sets_show_all() {
    let interactive = finalize_fork_from_args(["codex", "fork", "--all"].as_ref());
    assert!(interactive.fork_picker);
    assert!(interactive.fork_show_all);
}

#[test]
fn app_server_analytics_default_disabled_without_flag() {
    let app_server = app_server_from_args(["codex", "app-server"].as_ref());
    assert!(!app_server.analytics_default_enabled);
    assert!(!app_server.remote_control);
    assert_eq!(
        app_server.listen,
        codex_app_server::AppServerTransport::Stdio
    );
}

#[test]
fn app_server_remote_control_startup_flag_enables_remote_control() {
    let enabled = app_server_from_args(["codex", "app-server", "--remote-control"].as_ref());
    assert!(enabled.remote_control);
}

#[test]
fn app_server_analytics_default_enabled_with_flag() {
    let app_server =
        app_server_from_args(["codex", "app-server", "--analytics-default-enabled"].as_ref());
    assert!(app_server.analytics_default_enabled);
}

#[test]
fn strict_config_parses_for_supported_commands() {
    let cli = MultitoolCli::try_parse_from(["codex", "--strict-config"]).expect("parse");
    assert!(cli.interactive.strict_config);

    let cli = MultitoolCli::try_parse_from([
        "codex",
        "review",
        "--strict-config",
        "--trust",
        "--uncommitted",
    ])
    .expect("parse");
    assert_matches!(
        cli.subcommand,
        Some(Subcommand::Review(ReviewCommand {
            trust: true,
            strict_config: true,
            ..
        }))
    );

    let cli =
        MultitoolCli::try_parse_from(["codex", "exec-server", "--strict-config"]).expect("parse");
    assert_matches!(
        cli.subcommand,
        Some(Subcommand::ExecServer(ExecServerCommand {
            strict_config: true,
            ..
        }))
    );
}

#[test]
fn exec_server_forward_parses_shared_remote_options() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "exec-server",
        "forward",
        "--connect",
        "ws://127.0.0.1:8765",
        "--remote",
        "https://example.openai.com",
        "--environment-id",
        "env-1",
        "--name",
        "forwarded",
        "--strict-config",
        "--use-agent-identity-auth",
    ])
    .expect("parse forward");
    assert!(cli.remote.remote.is_none());
    assert!(!cli.interactive.strict_config);
    assert_matches!(
        cli.subcommand,
        Some(Subcommand::ExecServer(ExecServerCommand {
            strict_config: true,
            ..
        }))
    );
}

#[test]
fn exec_server_forward_requires_registration_and_destination() {
    for args in [
        vec!["forward", "--connect", "ws://127.0.0.1:8765"],
        vec![
            "forward",
            "--remote",
            "https://example.openai.com",
            "--environment-id",
            "env-1",
        ],
        vec![
            "forward",
            "--connect",
            "ws://127.0.0.1:8765",
            "--remote",
            "https://example.openai.com",
        ],
    ] {
        assert!(
            MultitoolCli::try_parse_from(["codex", "exec-server"].into_iter().chain(args)).is_err()
        );
    }
}

#[test]
fn root_strict_config_is_supported_for_exec_server() {
    let cli =
        MultitoolCli::try_parse_from(["codex", "--strict-config", "exec-server"]).expect("parse");

    reject_root_strict_config_for_subcommand(cli.interactive.strict_config, &cli.subcommand)
        .expect("exec-server should support root --strict-config");
}

#[test]
fn root_strict_config_is_rejected_for_unsupported_subcommands() {
    let cli =
        MultitoolCli::try_parse_from(["codex", "--strict-config", "mcp", "list"]).expect("parse");
    let err =
        reject_root_strict_config_for_subcommand(cli.interactive.strict_config, &cli.subcommand)
            .expect_err("mcp should not support root --strict-config");

    assert_eq!(
        err.to_string(),
        "`--strict-config` is not supported for `codex mcp`"
    );

    let cli = MultitoolCli::try_parse_from(["codex", "--strict-config", "remote-control"])
        .expect("parse");
    let err =
        reject_root_strict_config_for_subcommand(cli.interactive.strict_config, &cli.subcommand)
            .expect_err("remote-control should not support root --strict-config");

    assert_eq!(
        err.to_string(),
        "`--strict-config` is not supported for `codex remote-control`"
    );
}

#[test]
fn app_server_subcommands_reject_strict_config() {
    let app_server =
        app_server_from_args(["codex", "app-server", "--strict-config", "proxy"].as_ref());
    let err = reject_strict_config_for_app_server_subcommand(
        app_server.strict_config,
        app_server.subcommand.as_ref(),
    )
    .expect_err("app-server proxy should not support --strict-config");

    assert_eq!(
        err.to_string(),
        "`--strict-config` is not supported for `codex app-server proxy`"
    );
}

#[test]
fn reject_remote_flag_for_remote_control() {
    let cli = MultitoolCli::try_parse_from(["codex", "--remote", "unix://", "remote-control"])
        .expect("parse");
    let Some(Subcommand::RemoteControl(remote_control)) = &cli.subcommand else {
        panic!("expected remote-control subcommand");
    };
    assert_eq!(remote_control.subcommand_name(), "remote-control");

    let err = reject_remote_mode_for_subcommand(
        cli.remote.remote.as_deref(),
        cli.remote.remote_auth_token_env.as_deref(),
        "remote-control",
    )
    .expect_err("remote-control should reject root --remote");

    assert!(err.to_string().contains("remote-control"));
}

#[test]
fn remote_control_pair_parses() {
    let cli = MultitoolCli::try_parse_from(["codex", "remote-control", "pair"]).expect("parse");
    let Some(Subcommand::RemoteControl(remote_control)) = &cli.subcommand else {
        panic!("expected remote-control subcommand");
    };
    assert_eq!(remote_control.subcommand_name(), "remote-control pair");
}

#[test]
fn remote_flag_parses_for_interactive_root() {
    let cli =
        MultitoolCli::try_parse_from(["codex", "--remote", "unix://codex.sock"]).expect("parse");
    assert_eq!(cli.remote.remote.as_deref(), Some("unix://codex.sock"));
}

#[test]
fn remote_auth_token_env_flag_parses_for_interactive_root() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "--remote-auth-token-env",
        "CODEX_REMOTE_AUTH_TOKEN",
        "--remote",
        "ws://127.0.0.1:4500",
    ])
    .expect("parse");
    assert_eq!(
        cli.remote.remote_auth_token_env.as_deref(),
        Some("CODEX_REMOTE_AUTH_TOKEN")
    );
}

#[test]
fn remote_flag_parses_for_resume_subcommand() {
    let cli = MultitoolCli::try_parse_from(["codex", "resume", "--remote", "unix://codex.sock"])
        .expect("parse");
    let Subcommand::Resume(ResumeCommand { remote, .. }) = cli.subcommand.expect("resume present")
    else {
        panic!("expected resume subcommand");
    };
    assert_eq!(remote.remote.as_deref(), Some("unix://codex.sock"));
}

#[test]
fn agents_subcommand_accepts_remote_session_options() {
    let cli = MultitoolCli::try_parse_from([
        "codex",
        "agents",
        "--remote",
        "ws://127.0.0.1:4500",
        "--remote-auth-token-env",
        "CODEX_REMOTE_AUTH_TOKEN",
        "--cd",
        "/workspace",
        "--no-alt-screen",
    ])
    .expect("parse");
    let Some(Subcommand::Agents(options)) = cli.subcommand else {
        panic!("expected agents subcommand");
    };

    assert_eq!(
        options.remote.remote.as_deref(),
        Some("ws://127.0.0.1:4500")
    );
    assert_eq!(
        options.remote.remote_auth_token_env.as_deref(),
        Some("CODEX_REMOTE_AUTH_TOKEN")
    );
    assert_eq!(
        options.cwd.as_deref(),
        Some(std::path::Path::new("/workspace"))
    );
    assert!(options.no_alt_screen);
}

#[test]
fn reject_remote_mode_for_non_interactive_subcommands() {
    let err = reject_remote_mode_for_subcommand(
        Some("127.0.0.1:4500"),
        /*remote_auth_token_env*/ None,
        "exec",
    )
    .expect_err("non-interactive subcommands should reject --remote");
    assert!(
        err.to_string()
            .contains("only supported for interactive TUI commands")
    );
}

#[test]
fn reject_remote_auth_token_env_for_non_interactive_subcommands() {
    let err = reject_remote_mode_for_subcommand(
        /*remote*/ None,
        Some("CODEX_REMOTE_AUTH_TOKEN"),
        "exec",
    )
    .expect_err("non-interactive subcommands should reject --remote-auth-token-env");
    assert!(
        err.to_string()
            .contains("only supported for interactive TUI commands")
    );
}

#[test]
fn reject_remote_auth_token_env_for_app_server_generate_internal_json_schema() {
    let subcommand =
        AppServerSubcommand::GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand {
            out_dir: PathBuf::from("/tmp/out"),
        });
    let err = reject_remote_mode_for_app_server_subcommand(
        /*remote*/ None,
        Some("CODEX_REMOTE_AUTH_TOKEN"),
        Some(&subcommand),
    )
    .expect_err("non-interactive app-server subcommands should reject --remote-auth-token-env");
    assert!(err.to_string().contains("generate-internal-json-schema"));
}

#[test]
fn read_remote_auth_token_from_env_var_reports_missing_values() {
    let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
        Err(std::env::VarError::NotPresent)
    })
    .expect_err("missing env vars should be rejected");
    assert!(err.to_string().contains("is not set"));
}

#[test]
fn read_remote_auth_token_from_env_var_trims_values() {
    let auth_token = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
        Ok("  bearer-token  ".to_string())
    })
    .expect("env var should parse");
    assert_eq!(auth_token, "bearer-token");
}

#[test]
fn read_remote_auth_token_from_env_var_rejects_empty_values() {
    let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
        Ok(" \n\t ".to_string())
    })
    .expect_err("empty env vars should be rejected");
    assert!(err.to_string().contains("is empty"));
}

#[test]
fn app_server_grpc_code_mode_host_url_parses_independently_of_listen_transport() {
    let app_server = app_server_from_args(
        [
            "codex",
            "app-server",
            "--code-mode-host",
            "https://example.test",
            "--listen",
            "ws://127.0.0.1:4500",
        ]
        .as_ref(),
    );

    assert_eq!(
        app_server.code_mode_host.code_mode_host,
        Some(url::Url::parse("https://example.test").expect("test endpoint should parse"))
    );
}

#[test]
fn app_server_rejects_invalid_code_mode_host_urls() {
    for endpoint in [
        "ftp://127.0.0.1:8765",
        "ws://",
        "ws://127.0.0.1:8765",
        "wss://example.test/code-mode",
        "ws://alice:secret@example.test/code-mode",
        "wss://alice:secret@example.test/code-mode",
        "wss://example.test/code-mode#fragment",
        "http://",
        "https://example.test/#fragment",
        "https://example.test/code-mode",
        "http://alice:secret@example.test",
        "https://alice:secret@example.test",
        "http://example.test/?token=secret",
    ] {
        let error =
            MultitoolCli::try_parse_from(["codex", "app-server", "--code-mode-host", endpoint])
                .expect_err("invalid code-mode host endpoint should fail argument parsing");

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        let rendered_error = error.to_string();
        assert!(!rendered_error.contains("alice"));
        assert!(!rendered_error.contains("secret"));
    }
}

#[test]
fn app_server_listen_websocket_url_parses() {
    let app_server =
        app_server_from_args(["codex", "app-server", "--listen", "ws://127.0.0.1:4500"].as_ref());
    assert_eq!(
        app_server.listen,
        codex_app_server::AppServerTransport::WebSocket {
            bind_address: "127.0.0.1:4500".parse().expect("valid socket address"),
        }
    );
}

#[test]
fn app_server_listen_stdio_url_parses() {
    let app_server = app_server_from_args(["codex", "app-server", "--listen", "stdio://"].as_ref());
    assert_eq!(
        app_server.listen,
        codex_app_server::AppServerTransport::Stdio
    );
}

#[test]
fn app_server_stdio_flag_parses() {
    let app_server = app_server_from_args(["codex", "app-server", "--stdio"].as_ref());
    assert!(app_server.stdio);
}

#[test]
fn app_server_stdio_flag_conflicts_with_listen() {
    let err =
        MultitoolCli::try_parse_from(["codex", "app-server", "--stdio", "--listen", "stdio://"])
            .expect_err("--stdio and --listen should be rejected together");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn app_server_listen_unix_socket_url_parses() {
    let app_server = app_server_from_args(["codex", "app-server", "--listen", "unix://"].as_ref());
    assert_eq!(
        app_server.listen,
        codex_app_server::AppServerTransport::UnixSocket {
            socket_path: default_app_server_socket_path()
        }
    );
}

#[test]
fn app_server_listen_unix_socket_path_parses() {
    let app_server = app_server_from_args(
        ["codex", "app-server", "--listen", "unix:///tmp/codex.sock"].as_ref(),
    );
    assert_eq!(
        app_server.listen,
        codex_app_server::AppServerTransport::UnixSocket {
            socket_path: AbsolutePathBuf::from_absolute_path("/tmp/codex.sock")
                .expect("absolute path should parse")
        }
    );
}

#[test]
fn app_server_listen_off_parses() {
    let app_server = app_server_from_args(["codex", "app-server", "--listen", "off"].as_ref());
    assert_eq!(app_server.listen, codex_app_server::AppServerTransport::Off);
}

#[test]
fn app_server_listen_invalid_url_fails_to_parse() {
    let parse_result =
        MultitoolCli::try_parse_from(["codex", "app-server", "--listen", "http://foo"]);
    assert!(parse_result.is_err());
}

#[test]
fn app_server_proxy_subcommand_parses() {
    let app_server = app_server_from_args(["codex", "app-server", "proxy"].as_ref());
    assert!(matches!(
        app_server.subcommand,
        Some(AppServerSubcommand::Proxy(AppServerProxyCommand {
            socket_path: None
        }))
    ));
}

#[test]
fn app_server_daemon_subcommands_parse() {
    assert!(matches!(
        app_server_from_args(
            [
                "codex",
                "app-server",
                "daemon",
                "bootstrap",
                "--remote-control"
            ]
            .as_ref()
        )
        .subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::Bootstrap(AppServerBootstrapCommand {
                remote_control: true
            })
        }))
    ));
    assert!(matches!(
        app_server_from_args(["codex", "app-server", "daemon", "start"].as_ref()).subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::Start
        }))
    ));
    assert!(matches!(
        app_server_from_args(["codex", "app-server", "daemon", "restart"].as_ref()).subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::Restart
        }))
    ));
    assert!(matches!(
        app_server_from_args(["codex", "app-server", "daemon", "enable-remote-control"].as_ref())
            .subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::EnableRemoteControl
        }))
    ));
    assert!(matches!(
        app_server_from_args(["codex", "app-server", "daemon", "disable-remote-control"].as_ref())
            .subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::DisableRemoteControl
        }))
    ));
    assert!(matches!(
        app_server_from_args(["codex", "app-server", "daemon", "stop"].as_ref()).subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::Stop
        }))
    ));
    assert!(matches!(
        app_server_from_args(["codex", "app-server", "daemon", "version"].as_ref()).subcommand,
        Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::Version
        }))
    ));
}

#[test]
fn app_server_proxy_sock_path_parses() {
    let app_server =
        app_server_from_args(["codex", "app-server", "proxy", "--sock", "codex.sock"].as_ref());
    let Some(AppServerSubcommand::Proxy(proxy)) = app_server.subcommand else {
        panic!("expected proxy subcommand");
    };
    assert_eq!(
        proxy.socket_path,
        Some(
            AbsolutePathBuf::relative_to_current_dir("codex.sock")
                .expect("relative path should resolve")
        )
    );
}

#[test]
fn reject_remote_auth_token_env_for_app_server_proxy() {
    let subcommand = AppServerSubcommand::Proxy(AppServerProxyCommand { socket_path: None });
    let err = reject_remote_mode_for_app_server_subcommand(
        /*remote*/ None,
        Some("CODEX_REMOTE_AUTH_TOKEN"),
        Some(&subcommand),
    )
    .expect_err("app-server proxy should reject --remote-auth-token-env");
    assert!(err.to_string().contains("app-server proxy"));
}

#[test]
fn reject_remote_auth_token_env_for_app_server_version() {
    let subcommand = AppServerSubcommand::Daemon(AppServerDaemonCommand {
        subcommand: AppServerDaemonSubcommand::Version,
    });
    let err = reject_remote_mode_for_app_server_subcommand(
        /*remote*/ None,
        Some("CODEX_REMOTE_AUTH_TOKEN"),
        Some(&subcommand),
    )
    .expect_err("app-server daemon version should reject --remote-auth-token-env");
    assert!(err.to_string().contains("app-server daemon version"));
}

#[test]
fn app_server_capability_token_flags_parse() {
    let app_server = app_server_from_args(
        [
            "codex",
            "app-server",
            "--ws-auth",
            "capability-token",
            "--ws-token-file",
            "/tmp/codex-token",
        ]
        .as_ref(),
    );
    assert_eq!(
        app_server.auth.ws_auth,
        Some(codex_websocket_auth::WebsocketAuthCliMode::CapabilityToken)
    );
    assert_eq!(
        app_server.auth.ws_token_file,
        Some(PathBuf::from("/tmp/codex-token"))
    );
}

#[test]
fn app_server_signed_bearer_flags_parse() {
    let app_server = app_server_from_args(
        [
            "codex",
            "app-server",
            "--ws-auth",
            "signed-bearer-token",
            "--ws-shared-secret-file",
            "/tmp/codex-secret",
            "--ws-issuer",
            "issuer",
            "--ws-audience",
            "audience",
            "--ws-max-clock-skew-seconds",
            "9",
        ]
        .as_ref(),
    );
    assert_eq!(
        app_server.auth.ws_auth,
        Some(codex_websocket_auth::WebsocketAuthCliMode::SignedBearerToken)
    );
    assert_eq!(
        app_server.auth.ws_shared_secret_file,
        Some(PathBuf::from("/tmp/codex-secret"))
    );
    assert_eq!(app_server.auth.ws_issuer.as_deref(), Some("issuer"));
    assert_eq!(app_server.auth.ws_audience.as_deref(), Some("audience"));
    assert_eq!(app_server.auth.ws_max_clock_skew_seconds, Some(9));
}

#[test]
fn app_server_rejects_removed_insecure_non_loopback_flag() {
    let parse_result = MultitoolCli::try_parse_from([
        "codex",
        "app-server",
        "--allow-unauthenticated-non-loopback-ws",
    ]);
    assert!(parse_result.is_err());
}

#[test]
fn features_enable_parses_feature_name() {
    let cli = MultitoolCli::try_parse_from(["codex", "features", "enable", "unified_exec"])
        .expect("parse should succeed");
    let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
        panic!("expected features subcommand");
    };
    let FeaturesSubcommand::Enable(FeatureSetArgs { feature }) = sub else {
        panic!("expected features enable");
    };
    assert_eq!(feature, "unified_exec");
}

#[test]
fn features_disable_parses_feature_name() {
    let cli = MultitoolCli::try_parse_from(["codex", "features", "disable", "shell_tool"])
        .expect("parse should succeed");
    let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
        panic!("expected features subcommand");
    };
    let FeaturesSubcommand::Disable(FeatureSetArgs { feature }) = sub else {
        panic!("expected features disable");
    };
    assert_eq!(feature, "shell_tool");
}

#[test]
fn feature_toggles_known_features_generate_overrides() {
    let toggles = FeatureToggles {
        enable: vec!["web_search_request".to_string()],
        disable: vec!["unified_exec".to_string()],
    };
    let overrides = toggles.to_overrides().expect("valid features");
    assert_eq!(
        overrides,
        vec![
            "features.web_search_request=true".to_string(),
            "features.unified_exec=false".to_string(),
        ]
    );
}

#[test]
fn feature_toggles_accept_legacy_linux_sandbox_flag() {
    let toggles = FeatureToggles {
        enable: vec!["use_linux_sandbox_bwrap".to_string()],
        disable: Vec::new(),
    };
    let overrides = toggles.to_overrides().expect("valid features");
    assert_eq!(
        overrides,
        vec!["features.use_linux_sandbox_bwrap=true".to_string(),]
    );
}

#[test]
fn feature_toggles_accept_removed_image_detail_original_flag() {
    let toggles = FeatureToggles {
        enable: vec!["image_detail_original".to_string()],
        disable: Vec::new(),
    };
    let overrides = toggles.to_overrides().expect("valid features");
    assert_eq!(
        overrides,
        vec!["features.image_detail_original=true".to_string(),]
    );
}

#[test]
fn feature_toggles_accept_removed_enable_fanout_flag() {
    let toggles = FeatureToggles {
        enable: vec!["enable_fanout".to_string()],
        disable: Vec::new(),
    };
    let overrides = toggles.to_overrides().expect("valid features");
    assert_eq!(overrides, vec!["features.enable_fanout=true".to_string(),]);
}

#[test]
fn feature_toggles_accept_removed_item_ids_flag() {
    let toggles = FeatureToggles {
        enable: vec!["item_ids".to_string()],
        disable: Vec::new(),
    };
    let overrides = toggles.to_overrides().expect("valid features");
    assert_eq!(overrides, vec!["features.item_ids=true".to_string()]);
}

#[test]
fn feature_toggles_unknown_feature_errors() {
    let toggles = FeatureToggles {
        enable: vec!["does_not_exist".to_string()],
        disable: Vec::new(),
    };
    let err = toggles
        .to_overrides()
        .expect_err("feature should be rejected");
    assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
}

#[test]
fn strict_config_with_unknown_enable_errors() {
    let err = strict_config_feature_toggle_error(["--enable", "does_not_exist"].as_ref());
    assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
}

#[test]
fn strict_config_with_unknown_disable_errors() {
    let err = strict_config_feature_toggle_error(["--disable", "does_not_exist"].as_ref());
    assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
}

#[test]
fn strict_config_with_compound_enable_errors() {
    let err = strict_config_feature_toggle_error(
        ["--enable", "multi_agent_v2.subagent_usage_hint_text"].as_ref(),
    );
    assert_eq!(
        err.to_string(),
        "Unknown feature flag: multi_agent_v2.subagent_usage_hint_text"
    );
}

fn strict_config_feature_toggle_error(args: &[&str]) -> anyhow::Error {
    let cli_args = std::iter::once("codex")
        .chain(std::iter::once("--strict-config"))
        .chain(args.iter().copied());
    let cli = MultitoolCli::try_parse_from(cli_args).expect("parse should succeed");
    assert!(cli.interactive.strict_config);
    cli.feature_toggles
        .to_overrides()
        .expect_err("feature should be rejected")
}
