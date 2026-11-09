use pretty_assertions::assert_eq;

use super::*;

fn detailed_no_color_unicode_options() -> HumanOutputOptions {
    HumanOutputOptions {
        show_details: true,
        show_all: false,
        ascii: false,
        color_enabled: false,
    }
}

fn summary_no_color_unicode_options() -> HumanOutputOptions {
    HumanOutputOptions {
        show_details: false,
        show_all: false,
        ascii: false,
        color_enabled: false,
    }
}

fn detailed_all_no_color_unicode_options() -> HumanOutputOptions {
    HumanOutputOptions {
        show_details: true,
        show_all: true,
        ascii: false,
        color_enabled: false,
    }
}

fn detailed_color_unicode_options() -> HumanOutputOptions {
    HumanOutputOptions {
        show_details: true,
        show_all: false,
        ascii: false,
        color_enabled: true,
    }
}

fn sample_report() -> DoctorReport {
    let checks = vec![
        DoctorCheck::new(
            "system.environment",
            "system",
            CheckStatus::Ok,
            "OS language en-US",
        )
        .detail("os: macOS 15.0")
        .detail("os language: en-US")
        .detail("VISUAL: code --wait")
        .detail("EDITOR: vim")
        .detail("PAGER: less -R")
        .detail("GIT_PAGER: delta")
        .detail("GH_PAGER: less")
        .detail("LESS: -FRX"),
        DoctorCheck::new(
            "runtime.provenance",
            "runtime",
            CheckStatus::Ok,
            "running local build on darwin-arm64",
        ),
        DoctorCheck::new(
            "installation",
            "install",
            CheckStatus::Ok,
            "installation looks consistent",
        ),
        DoctorCheck::new(
            "runtime.search",
            "search",
            CheckStatus::Ok,
            "search is OK (bundled)",
        ),
        DoctorCheck::new(
            "git.environment",
            "git",
            CheckStatus::Ok,
            "git version 2.54.0",
        )
        .detail("selected git: /usr/bin/git")
        .detail("git version: git version 2.54.0")
        .detail("repo detected: true"),
        DoctorCheck::new(
            "terminal.env",
            "terminal",
            CheckStatus::Warning,
            "narrow terminal",
        ),
        DoctorCheck::new(
            "terminal.title",
            "title",
            CheckStatus::Ok,
            "terminal title default",
        )
        .detail("terminal title source: default")
        .detail("terminal title items: activity, project-name")
        .detail("terminal title project value: codex"),
        DoctorCheck::new(
            "state.paths",
            "state",
            CheckStatus::Ok,
            "state paths inspectable",
        ),
        DoctorCheck::new(
            "auth.credentials",
            "auth",
            CheckStatus::Fail,
            "token expired",
        )
        .detail("OPENAI_API_KEY: present")
        .remediation("Run `codex login`."),
        DoctorCheck::new(
            "updates.status",
            "updates",
            CheckStatus::Ok,
            "update configuration is locally consistent",
        ),
        DoctorCheck::new(
            "network.env",
            "network",
            CheckStatus::Ok,
            "network environment readable",
        ),
        DoctorCheck::new(
            "network.websocket_reachability",
            "websocket",
            CheckStatus::Ok,
            "Responses WebSocket handshake succeeded",
        ),
        DoctorCheck::new(
            "app_server.status",
            "app-server",
            CheckStatus::Ok,
            "background server is not running",
        ),
        DoctorCheck::new(
            "network.provider_reachability",
            "reachability",
            CheckStatus::Ok,
            "active provider endpoints are reachable over HTTP",
        ),
    ];
    DoctorReport {
        schema_version: 1,
        generated_at: "0s since unix epoch".to_string(),
        overall_status: CheckStatus::Fail,
        codex_version: "0.0.0".to_string(),
        checks,
    }
}

#[test]
fn render_human_report_includes_details_by_default_without_color() {
    let rendered = render_human_report(&sample_report(), detailed_no_color_unicode_options());
    let expected = format!(
        "\
Codex Doctor v0.0.0

Notes
   ⚠ terminal     narrow terminal
   ✗ auth         token expired - Run `codex login`.
─────────────────────────────────────────────────────────────

Environment
  ✓ system       en-US
      os                       macOS 15.0
      OS language              en-US
      VISUAL                   code --wait
      EDITOR                   vim
      PAGER                    less -R
      GIT_PAGER                delta
      GH_PAGER                 less
      LESS                     -FRX
  ✓ runtime      running local build on darwin-arm64
  ✓ install      consistent
      managed by               npm: no · bun: no · pnpm: no · Vite+: no · package root —
  ✓ search       search is OK (bundled)
  ✓ git          git version 2.54.0
      selected git             /usr/bin/git
      version                  git version 2.54.0
      repo detected            true
  ⚠ terminal     narrow terminal
  ✓ title        default · project codex
      title source             default
      title items              activity, project-name
      project value            codex
  ✓ state        state paths inspectable

Configuration
  ✗ auth         token expired — Run `codex login`.
      OPENAI_API_KEY           present

Updates
  ✓ updates      update configuration is locally consistent

Connectivity
  ✓ network      network environment readable
  ✓ websocket    Responses WebSocket handshake succeeded
  ✓ reachability active provider endpoints are reachable over HTTP

Background Server
  ✓ app-server   background server is not running

{}
12 ok · 2 notes · 1 warn · 1 fail failed

--summary compact output           --all expand truncated lists
--json redacted report
",
        "─".repeat(SEPARATOR_WIDTH)
    );
    assert_eq!(rendered, expected);
}

#[test]
fn render_human_report_snapshot_covers_environment_rows() {
    let mut report = sample_report();
    report.checks.push(
        DoctorCheck::new(
            "network.reachability",
            "network",
            CheckStatus::Fail,
            "provider is unreachable",
        )
        .remediation(super::super::network::SYSTEM_PROXY_REMEDIATION),
    );
    report.checks.push(
        DoctorCheck::new("config.load", "config", CheckStatus::Ok, "config loaded")
            .detail("configured TUI mode: fullscreen"),
    );
    report.checks.push(DoctorCheck::new(
        "system.disk",
        "disk",
        CheckStatus::Ok,
        "sufficient free disk space (42.0 GiB)",
    ));
    report.checks.push(
            DoctorCheck::new(
                "git.worktree.dev_drive",
                "git",
                CheckStatus::Warning,
                "this worktree is not on a Windows Dev Drive",
            )
            .remediation(
                "create a trusted Windows Dev Drive: https://learn.microsoft.com/en-us/windows/dev-drive/",
            ),
        );
    let mut security = super::super::security::endpoint_check(
        super::super::security::EndpointInspection::Complete(vec!["Microsoft Defender"]),
    );
    let targets = security
        .details
        .iter_mut()
        .find(|detail| detail.starts_with("exclusion targets: "))
        .expect("endpoint security check should include exclusion targets");
    *targets = "exclusion targets: verified Codex app and required helpers".into();
    report.checks.push(security);
    report.checks.extend([
        DoctorCheck::new(
            "desktop.app.version",
            "desktop",
            CheckStatus::Ok,
            "the desktop application is installed",
        )
        .detail("version: 1.2.3")
        .detail("running: true")
        .detail("log directory: $HOME/Library/Logs/com.openai.codex"),
        DoctorCheck::new(
            "desktop.app_server.handshake",
            "desktop",
            CheckStatus::Ok,
            "the desktop app-server initialized successfully",
        ),
    ]);
    let update = report
        .checks
        .iter_mut()
        .find(|check| check.category == "updates")
        .unwrap();
    for (status, expected) in [
        ("available", "build 123 available"),
        ("ready to install", "build 123 available (ready to install)"),
    ] {
        update.details = vec![
            format!("desktop update status: {status}"),
            "desktop latest build: 123".to_string(),
        ];
        assert_eq!(desktop_update_note(update).unwrap().summary, expected);
    }
    insta::assert_snapshot!(
        "doctor_human_report_environment_rows",
        render_human_report(&report, detailed_no_color_unicode_options())
    );
}

#[test]
fn render_human_report_supports_summary_output_without_color() {
    let rendered = render_human_report(&sample_report(), summary_no_color_unicode_options());
    let expected = format!(
        "\
Codex Doctor v0.0.0

Notes
   ⚠ terminal     narrow terminal
   ✗ auth         token expired - Run `codex login`.
─────────────────────────────────────────────────────────────

Environment
  ✓ system       en-US
  ✓ runtime      running local build on darwin-arm64
  ✓ install      consistent
  ✓ search       search is OK (bundled)
  ✓ git          git version 2.54.0
  ⚠ terminal     narrow terminal
  ✓ title        default · project codex
  ✓ state        state paths inspectable

Configuration
  ✗ auth         token expired — Run `codex login`.

Updates
  ✓ updates      update configuration is locally consistent

Connectivity
  ✓ network      network environment readable
  ✓ websocket    Responses WebSocket handshake succeeded
  ✓ reachability active provider endpoints are reachable over HTTP

Background Server
  ✓ app-server   background server is not running

{}
12 ok · 2 notes · 1 warn · 1 fail failed

Run codex doctor without --summary for detailed diagnostics.
--all expand truncated lists       --json redacted report
",
        "─".repeat(SEPARATOR_WIDTH)
    );
    assert_eq!(rendered, expected);
}

#[test]
fn render_human_report_includes_threads_row_in_environment() {
    let mut report = sample_report();
    report.checks.push(DoctorCheck::new(
        "state.rollout_db_parity",
        "threads",
        CheckStatus::Warning,
        "rollout files and state DB thread inventory differ",
    ));

    let rendered = render_human_report(&report, summary_no_color_unicode_options());

    let threads_line = rendered
        .lines()
        .find(|line| line.contains("threads"))
        .expect("threads row should be rendered");
    assert!(
        threads_line.contains("rollout files and state DB thread inventory differ"),
        "{threads_line}"
    );
}

#[test]
fn render_human_report_includes_memories_db_in_state_health_summary() {
    let report = DoctorReport {
        schema_version: 1,
        generated_at: "0s since unix epoch".to_string(),
        overall_status: CheckStatus::Ok,
        codex_version: "0.0.0".to_string(),
        checks: vec![
            DoctorCheck::new(
                "state.paths",
                "state",
                CheckStatus::Ok,
                "state paths inspectable",
            )
            .detail("state DB: /tmp/state.sqlite")
            .detail("state DB integrity: ok")
            .detail("log DB: /tmp/logs.sqlite")
            .detail("log DB integrity: ok")
            .detail("goals DB: /tmp/goals.sqlite")
            .detail("goals DB integrity: ok")
            .detail("memories DB: /tmp/memories.sqlite")
            .detail("memories DB integrity: ok"),
        ],
    };

    let rendered = render_human_report(&report, detailed_no_color_unicode_options());

    assert!(rendered.contains("✓ state        databases healthy"));
    assert!(rendered.contains("memories DB              /tmp/memories.sqlite · integrity ok"));
}

#[test]
fn render_human_report_supports_ascii_output() {
    let rendered = render_human_report(
        &sample_report(),
        HumanOutputOptions {
            show_details: false,
            show_all: false,
            ascii: true,
            color_enabled: false,
        },
    );
    let expected = format!(
        "\
Codex Doctor v0.0.0

Notes
   [!!] terminal     narrow terminal
   [XX] auth         token expired - Run `codex login`.
-------------------------------------------------------------

Environment
  [ok] system       en-US
  [ok] runtime      running local build on darwin-arm64
  [ok] install      consistent
  [ok] search       search is OK (bundled)
  [ok] git          git version 2.54.0
  [!!] terminal     narrow terminal
  [ok] title        default | project codex
  [ok] state        state paths inspectable

Configuration
  [XX] auth         token expired - Run `codex login`.

Updates
  [ok] updates      update configuration is locally consistent

Connectivity
  [ok] network      network environment readable
  [ok] websocket    Responses WebSocket handshake succeeded
  [ok] reachability active provider endpoints are reachable over HTTP

Background Server
  [ok] app-server   background server is not running

{}
12 ok | 2 notes | 1 warn | 1 fail failed

Run codex doctor without --summary for detailed diagnostics.
--all expand truncated lists       --json redacted report
",
        "-".repeat(SEPARATOR_WIDTH)
    );
    assert_eq!(rendered, expected);
}

#[test]
fn render_human_report_includes_redacted_details() {
    let rendered = render_human_report(
        &sample_report(),
        HumanOutputOptions {
            show_details: true,
            show_all: false,
            ascii: false,
            color_enabled: false,
        },
    );
    assert!(rendered.contains("      OPENAI_API_KEY           present"));
}

#[test]
fn render_human_report_explains_terminal_warning_issue() {
    let report = DoctorReport {
        schema_version: 1,
        generated_at: "0s since unix epoch".to_string(),
        overall_status: CheckStatus::Warning,
        codex_version: "0.0.0".to_string(),
        checks: vec![
            DoctorCheck::new(
                "terminal.env",
                "terminal",
                CheckStatus::Warning,
                "width 79 cols - output may wrap (recommended >=80)",
            )
            .detail("terminal: Ghostty")
            .detail("terminal version: 1.3.1")
            .detail("terminal size: 79x26")
            .issue(
                super::super::DoctorIssue::new(
                    CheckStatus::Warning,
                    "width 79 cols - output may wrap (recommended >=80)",
                )
                .expected(">= 80 columns")
                .remedy("resize the window to at least 80 columns")
                .field("terminal size"),
            ),
        ],
    };

    let rendered = render_human_report(&report, detailed_no_color_unicode_options());

    assert!(rendered.contains("⚠ terminal     width 79 cols - output may wrap (recommended >=80)"));
    assert!(rendered.contains("▸ terminal size            79x26 (expected >= 80 columns)"));
    assert!(rendered.contains("→ resize the window to at least 80 columns"));
    assert!(!rendered.contains("⚠ terminal     Ghostty 1.3.1"));
}

#[test]
fn render_human_report_promotes_notes_without_changing_statuses() {
    let report = DoctorReport {
        schema_version: 1,
        generated_at: "0s since unix epoch".to_string(),
        overall_status: CheckStatus::Warning,
        codex_version: "0.0.0".to_string(),
        checks: vec![
            DoctorCheck::new(
                "updates.status",
                "updates",
                CheckStatus::Ok,
                "update configuration is locally consistent",
            )
            .detail("latest version status: newer version is available")
            .detail("latest version: 0.130.0")
            .detail("dismissed version: 0.128.0"),
            DoctorCheck::new(
                "state.paths",
                "state",
                CheckStatus::Ok,
                "state paths inspectable",
            )
            .detail(
                "active rollout files: 1515 files, 2702146365 total bytes, 1783594 average bytes",
            ),
            DoctorCheck::new(
                "sandbox.helpers",
                "sandbox",
                CheckStatus::Ok,
                "sandbox configuration is readable",
            )
            .detail("filesystem sandbox: danger-full-access")
            .detail("network sandbox: restricted")
            .detail("approval policy: Never"),
            DoctorCheck::new(
                "mcp.config",
                "mcp",
                CheckStatus::Warning,
                "MCP configuration has optional issues",
            ),
            DoctorCheck::new(
                "network.websocket_reachability",
                "websocket",
                CheckStatus::Ok,
                "Responses WebSocket handshake succeeded",
            )
            .detail("auth mode: chatgpt"),
            DoctorCheck::new(
                "network.provider_reachability",
                "reachability",
                CheckStatus::Ok,
                "active provider endpoints are reachable over HTTP",
            )
            .detail("reachability mode: API key auth"),
            DoctorCheck::new(
                "app_server.status",
                "app-server",
                CheckStatus::Ok,
                "background server is not running",
            )
            .detail("status: not running")
            .detail("mode: ephemeral"),
        ],
    };

    let rendered = render_human_report(&report, summary_no_color_unicode_options());

    assert!(rendered.contains("Notes\n   ↑ updates"));
    assert!(rendered.contains("0.130.0 available (current 0.0.0, dismissed 0.128.0)"));
    assert!(rendered.contains("⚠ rollouts"));
    assert!(rendered.contains("⚠ sandbox"));
    assert!(rendered.contains("⚠ mcp"));
    assert!(rendered.contains(
            "⚠ auth         mixed auth signals: ChatGPT login plus API key env var; HTTP reachability uses API-key mode"
        ));
    assert!(rendered.contains("○ app-server   not running (ephemeral mode)"));
    assert!(rendered.contains("5 ok · 1 idle · 5 notes · 1 warn · 0 fail degraded"));
}

#[test]
fn render_human_report_expands_feature_flags_with_all() {
    let report = DoctorReport {
        schema_version: 1,
        generated_at: "0s since unix epoch".to_string(),
        overall_status: CheckStatus::Ok,
        codex_version: "0.0.0".to_string(),
        checks: vec![
            DoctorCheck::new("config.load", "config", CheckStatus::Ok, "config loaded")
                .detail("model: gpt-5.5")
                .detail("model provider: openai")
                .detail("feature flags enabled: 3")
                .detail("enabled feature flags: shell_tool, memories, goals")
                .detail("feature flag overrides: memories=true"),
        ],
    };

    let compact = render_human_report(&report, detailed_no_color_unicode_options());
    let expanded = render_human_report(&report, detailed_all_no_color_unicode_options());

    assert!(!compact.contains("enabled flags"));
    assert!(
        compact
            .contains("feature flags            3 enabled · 1 overridden (full list with --all)")
    );
    assert!(expanded.contains("enabled flags            shell_tool, memories, goals"));
}

#[test]
fn detail_value_colors_inline_statuses_and_low_signal_values() {
    let rendered = detail_value(
        "npm: no · commit unknown · integrity ok · ~/code/codex/target/debug/codex · <redacted>",
        DisplayStatus::Ok,
        detailed_color_unicode_options(),
    );

    assert!(rendered.contains("npm: \u{1b}[38;5;240mno"));
    assert!(rendered.contains("\u{1b}[38;5;240munknown"));
    assert!(rendered.contains("\u{1b}[38;5;10mok"));
    assert!(rendered.contains("\u{1b}[38;5;117m~/code/codex/target/debug/codex"));
    assert!(rendered.contains("\u{1b}[38;5;244m"));
}

#[test]
fn update_note_emphasizes_available_version_and_dims_context() {
    let rendered = style_update_note_summary(
        "0.130.0 available (current 0.0.0, dismissed 0.128.0)",
        detailed_color_unicode_options(),
    );

    assert!(rendered.contains("\u{1b}[38;5;220m0.130.0 available"));
    assert!(rendered.contains("\u{1b}[2m(current 0.0.0, dismissed 0.128.0)"));
}

#[test]
fn redact_detail_distinguishes_database_paths_from_field_labels() {
    let details = [
        r"C:\doctor-secret-sqlite\logs_2.sqlite database failed integrity check",
        "C:/doctor-secret-sqlite/logs_2.sqlite database failed integrity check",
        "/tmp/doctor-secret-sqlite/logs_2.sqlite database failed integrity check",
        r"log database: C:\doctor-secret-sqlite\logs_2.sqlite",
    ];

    assert_eq!(
        details.map(redact_detail),
        [
            "<redacted>",
            "<redacted>",
            "<redacted>",
            "log database: <redacted>",
        ]
        .map(str::to_string)
    );
}

#[test]
fn redact_detail_sanitizes_urls() {
    let redacted = redact_detail(
        "reachability failed: https://user:pass@example.com/mcp?x=abc#frag (connect failed)",
    );

    assert_eq!(
        redacted,
        "reachability failed: https://example.com/mcp (connect failed)"
    );
}

#[test]
fn redact_detail_sanitizes_secret_url_path_segments() {
    let redacted = redact_detail("reachability failed: https://example.com/mcp/abc123xyz");

    assert_eq!(
        redacted,
        "reachability failed: https://example.com/mcp/<redacted>"
    );
}

#[test]
fn redact_detail_preserves_env_var_names() {
    assert_eq!(
        redact_detail("auth env vars present: OPENAI_API_KEY, CODEX_API_KEY"),
        "auth env vars present: OPENAI_API_KEY, CODEX_API_KEY"
    );
}

#[test]
fn redact_detail_preserves_missing_env_diagnostics() {
    for prefix in ["env var", "bearer token env var", "header env var"] {
        for (variable, expected) in [
            ("DOCS_TOKEN", "DOCS_TOKEN"),
            ("docs_token", "docs_token"),
            ("", "<redacted>"),
            ("TOKEN: SYNTHETIC_CREDENTIAL", "<redacted>"),
            ("sk-proj-SYNTHETIC_CREDENTIAL", "<redacted>"),
            ("ghp_SYNTHETIC_CREDENTIAL", "<redacted>"),
            ("github_pat_SYNTHETIC_CREDENTIAL", "<redacted>"),
            ("AKIAABCDEFGHIJKLMNOP", "<redacted>"),
            ("ASIAABCDEFGHIJKLMNOP", "<redacted>"),
            ("eyJhbGciOiJIUzI1NiJ9.synthetic.signature", "<redacted>"),
        ] {
            assert_eq!(
                redact_detail(&format!("demo-server: {prefix} {variable} is not set")),
                format!("demo-server: {prefix} {expected} is not set")
            );
        }
    }
    assert_eq!(
        redact_detail("demo: command not found (helper --token SYNTHETIC_CREDENTIAL is not set)"),
        "demo: <redacted>"
    );
    assert_eq!(
        redact_detail("ghp_SYNTHETIC_CREDENTIAL: env var DOCS_TOKEN is not set"),
        "<redacted>: env var DOCS_TOKEN is not set"
    );
    assert_eq!(
        redact_detail("https://user:pass@example.com: env var DOCS_TOKEN is not set"),
        "https://example.com: env var DOCS_TOKEN is not set"
    );
}

#[test]
fn missing_env_diagnostics_in_json_and_text() {
    let check = DoctorCheck::new("mcp.config", "mcp", CheckStatus::Warning, "missing inputs")
        .detail("demo: bearer token env var DOCS_TOKEN is not set")
        .detail("DEMO: header env var DOCS_SECRET is not set")
        .detail("stdio: env var DOCS_TOKEN is not set")
        .detail(
            "npm:@modelcontextprotocol/server-sequential.thinking: env var DOCS_TOKEN is not set",
        )
        .detail("typo: bearer token env var sk-proj-SYNTHETIC_CREDENTIAL is not set");
    let json = serde_json::to_value(super::super::redacted_json_check(&check)).unwrap();
    assert_eq!(
        json["details"],
        serde_json::json!({
            "demo": "bearer token env var DOCS_TOKEN is not set",
            "DEMO": "header env var DOCS_SECRET is not set",
            "stdio": "env var DOCS_TOKEN is not set",
            "npm:@modelcontextprotocol/server-sequential.thinking": "env var DOCS_TOKEN is not set",
            "typo": "bearer token env var <redacted> is not set",
        })
    );
    let report = DoctorReport {
        overall_status: CheckStatus::Warning,
        checks: vec![check],
        ..sample_report()
    };
    insta::assert_snapshot!(
        "missing_env_diagnostics",
        render_human_report(&report, detailed_all_no_color_unicode_options())
    );
}

#[test]
fn redact_detail_preserves_secret_presence_booleans() {
    assert_eq!(
        redact_detail("stored ChatGPT tokens: true"),
        "stored ChatGPT tokens: true"
    );
    assert_eq!(
        redact_detail("stored ChatGPT tokens: false"),
        "stored ChatGPT tokens: false"
    );
}

#[test]
fn render_human_report_can_emit_color() {
    let rendered = render_human_report(
        &sample_report(),
        HumanOutputOptions {
            show_details: false,
            show_all: false,
            ascii: false,
            color_enabled: true,
        },
    );
    assert!(rendered.contains("\u{1b}["));
}

#[test]
fn copyable_items_use_check_status() {
    let description =
        "/tmp/logs_2.sqlite, ~/goals_1.sqlite integrity check; try --summary or `codex doctor`";
    let details = r"see ./logs, ../goals. C:\logs, D:/goals. c:\logs, \\server\share\logs, \\?\C:\logs, \\?\UNC\server\share\logs, \\.\pipe\codex, \??\C:\logs, \DosDevices\C:\logs, \rooted. http://localhost:8080: https://example.com; wss://example.com) and `/tmp/my data/logs_2.sqlite`";
    let mut rendered = String::new();

    for status in [CheckStatus::Ok, CheckStatus::Warning, CheckStatus::Fail] {
        let check = DoctorCheck::new("state.paths", "state", status, description)
            .detail(format!("locations: {details}"));
        write_check_row(&mut rendered, &check, detailed_color_unicode_options());
        assert_eq!(
            style_description(
                description,
                display_status(&check),
                detailed_no_color_unicode_options()
            ),
            description
        );
        assert_eq!(
            detail_value(
                details,
                display_status(&check),
                detailed_no_color_unicode_options()
            ),
            details
        );
    }

    let snapshot_name = if cfg!(windows) {
        "copyable_items_color_windows"
    } else {
        "copyable_items_color"
    };
    insta::assert_snapshot!(snapshot_name, rendered.replace('\u{1b}', "<ESC>"));
}
