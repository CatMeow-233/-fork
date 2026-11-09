//! Terminal check: terminal identity, color, terminfo, locale, and size diagnostics.

use super::*;

impl TerminalCheckInputs {
    fn detect(no_color_flag: bool) -> Self {
        let names = terminal_env_names();
        let (env, present_env) = collect_env_snapshot(&names);
        let terminal_size = crossterm::terminal::size().map_err(|err| err.to_string());
        let info = terminal_info();
        let tmux_details = if matches!(info.multiplexer, Some(Multiplexer::Tmux { .. })) {
            vec!["tmux options: not inspected (PATH helpers are not executed)".to_string()]
        } else {
            Vec::new()
        };
        let windows_console_details = windows_console_details();
        Self {
            info,
            env,
            present_env,
            no_color_flag,
            stdin_is_terminal: std::io::stdin().is_terminal(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            stderr_is_terminal: std::io::stderr().is_terminal(),
            stream_supports_color: supports_color::on(Stream::Stdout).is_some(),
            terminal_size,
            tmux_details,
            windows_console_details,
        }
    }

    fn env_value(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(String::as_str)
    }

    fn env_present(&self, name: &str) -> bool {
        self.present_env.contains(name)
    }
}

pub(super) fn terminal_check(no_color_flag: bool) -> DoctorCheck {
    terminal_check_from_inputs(TerminalCheckInputs::detect(no_color_flag))
}

#[cfg(windows)]
fn windows_console_details() -> Vec<String> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    use windows_sys::Win32::System::Console::GetConsoleCP;
    use windows_sys::Win32::System::Console::GetConsoleMode;
    use windows_sys::Win32::System::Console::GetConsoleOutputCP;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
    use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;

    let mut details = Vec::new();
    details.push(format!("console input code page: {}", unsafe {
        GetConsoleCP()
    }));
    details.push(format!("console output code page: {}", unsafe {
        GetConsoleOutputCP()
    }));
    details.push(console_mode_detail("stdout console mode", unsafe {
        GetStdHandle(STD_OUTPUT_HANDLE)
    }));
    details.push(console_mode_detail("stderr console mode", unsafe {
        GetStdHandle(STD_ERROR_HANDLE)
    }));

    fn console_mode_detail(label: &str, handle: HANDLE) -> String {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return format!("{label}: unavailable");
        }
        let mut mode = 0_u32;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return format!("{label}: unavailable");
        }
        let vt_enabled = mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0;
        format!("{label}: 0x{mode:08x} (VT processing: {vt_enabled})")
    }

    details
}

#[cfg(not(windows))]
fn windows_console_details() -> Vec<String> {
    Vec::new()
}

pub(super) fn terminal_check_from_inputs(inputs: TerminalCheckInputs) -> DoctorCheck {
    let info = &inputs.info;
    let name = info.name;
    let mut details = vec![format!("terminal: {}", terminal_name(info))];
    if let Some(term_program) = info.term_program.as_deref() {
        details.push(format!("TERM_PROGRAM: {term_program}"));
    }
    if let Some(version) = info.version.as_deref() {
        details.push(format!("terminal version: {version}"));
    }
    if let Some(term) = info.term.as_deref() {
        details.push(format!("TERM: {term}"));
    }
    if let Some(multiplexer) = info.multiplexer.as_ref() {
        details.push(format!("multiplexer: {}", multiplexer_name(multiplexer)));
    }
    details.push(format!("stdin is terminal: {}", inputs.stdin_is_terminal));
    details.push(format!("stdout is terminal: {}", inputs.stdout_is_terminal));
    details.push(format!("stderr is terminal: {}", inputs.stderr_is_terminal));
    match &inputs.terminal_size {
        Ok((columns, rows)) => details.push(format!("terminal size: {columns}x{rows}")),
        Err(err) => details.push(format!("terminal size: unavailable ({err})")),
    }
    push_terminal_env_values(&mut details, &inputs, TERMINAL_DIMENSION_ENV_VARS);
    details.push(format!("color output: {}", color_output_summary(&inputs)));
    push_terminal_env_values(&mut details, &inputs, COLOR_ENV_VARS);
    let terminfo_warning = push_terminfo_details(&mut details, &inputs);
    let locale = effective_locale(&inputs);
    if let Some(locale) = locale.as_ref() {
        details.push(format!("effective locale: {locale}"));
    }
    push_presence_env_values(&mut details, &inputs, REMOTE_TERMINAL_ENV_VARS);
    details.extend(inputs.tmux_details.iter().cloned());
    details.extend(inputs.windows_console_details.iter().cloned());

    let locale_warning = locale.as_deref().is_some_and(is_non_utf8_locale);
    let mut issues = Vec::new();
    if matches!(name, TerminalName::Dumb) {
        let issue = if inputs.stdin_is_terminal || inputs.stdout_is_terminal {
            DoctorIssue::new(
                CheckStatus::Fail,
                "TERM=dumb - colors and cursor control are disabled",
            )
            .expected("TERM=xterm-256color or another real terminal type")
            .remedy("set TERM to a real value, for example xterm-256color")
        } else {
            DoctorIssue::new(
                CheckStatus::Warning,
                "TERM=dumb - colors and cursor control are disabled in this non-interactive run",
            )
        };
        issues.push(issue.measured("TERM=dumb").field("TERM"));
    }
    if locale_warning {
        let measured = locale.unwrap_or_else(|| "unknown".to_string());
        issues.push(
            DoctorIssue::new(
                CheckStatus::Warning,
                "locale is not UTF-8 - unicode glyphs may render incorrectly",
            )
            .measured(measured)
            .expected("UTF-8 locale, for example en_US.UTF-8")
            .remedy("export LANG=en_US.UTF-8 or another UTF-8 locale")
            .field("effective locale"),
        );
    }
    if terminfo_warning {
        issues.push(
            DoctorIssue::new(
                CheckStatus::Fail,
                "TERMINFO unreadable - terminal capabilities are unknown",
            )
            .expected("readable terminfo file or directory")
            .remedy("check that $TERMINFO points to a readable directory")
            .field("TERMINFO")
            .field("TERMINFO_DIRS entry"),
        );
    }
    issues.extend(terminal_size_issues(&inputs));
    issues.sort_by_key(|issue| std::cmp::Reverse(issue.severity));

    let status = issues
        .iter()
        .map(|issue| issue.severity)
        .max()
        .unwrap_or(CheckStatus::Ok);
    let summary = issues
        .first()
        .map(|issue| issue.cause.as_str())
        .unwrap_or("terminal metadata was detected");
    let mut check = DoctorCheck::new("terminal.env", "terminal", status, summary).details(details);
    for issue in issues {
        check = check.issue(issue);
    }
    check
}

fn terminal_name(info: &TerminalInfo) -> &'static str {
    match info.name {
        TerminalName::AppleTerminal => "Apple Terminal",
        TerminalName::Ghostty => "Ghostty",
        TerminalName::Rex => "Rex",
        TerminalName::Iterm2 => "iTerm2",
        TerminalName::WarpTerminal => "Warp",
        TerminalName::VsCode => "VS Code",
        TerminalName::WezTerm => "WezTerm",
        TerminalName::Kitty => "kitty",
        TerminalName::Alacritty => "Alacritty",
        TerminalName::Konsole => "Konsole",
        TerminalName::GnomeTerminal => "GNOME Terminal",
        TerminalName::Vte => "VTE",
        TerminalName::WindowsTerminal => "Windows Terminal",
        TerminalName::Dumb => "dumb",
        TerminalName::Unknown => "unknown",
    }
}

fn multiplexer_name(multiplexer: &Multiplexer) -> String {
    match multiplexer {
        Multiplexer::Tmux { version } => match version {
            Some(version) => format!("tmux {version}"),
            None => "tmux".to_string(),
        },
        Multiplexer::Zellij { version } => match version {
            Some(version) => format!("zellij {version}"),
            None => "zellij".to_string(),
        },
    }
}

fn terminal_env_names() -> BTreeSet<&'static str> {
    let mut names = BTreeSet::from(["TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"]);
    names.extend(COLOR_ENV_VARS.iter().copied());
    names.extend(TERMINAL_DIMENSION_ENV_VARS.iter().copied());
    names.extend(TERMINFO_ENV_VARS.iter().copied());
    names.extend(LOCALE_ENV_VARS.iter().copied());
    names.extend(REMOTE_TERMINAL_ENV_VARS.iter().copied());
    names
}

fn collect_env_snapshot(
    names: &BTreeSet<&'static str>,
) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let mut values = BTreeMap::new();
    let mut present = BTreeSet::new();
    for name in names {
        if let Some(raw) = env::var_os(name) {
            present.insert((*name).to_string());
            let value = raw.to_string_lossy().trim().to_string();
            if !value.is_empty() {
                values.insert((*name).to_string(), value);
            }
        }
    }
    (values, present)
}

fn push_terminal_env_values(
    details: &mut Vec<String>,
    inputs: &TerminalCheckInputs,
    names: &[&str],
) {
    for name in names {
        if let Some(value) = inputs.env_value(name) {
            details.push(format!("{name}: {value}"));
        } else if inputs.env_present(name) {
            details.push(format!("{name}: present"));
        }
    }
}

fn push_presence_env_values(
    details: &mut Vec<String>,
    inputs: &TerminalCheckInputs,
    names: &[&str],
) {
    for name in names {
        if inputs.env_present(name) {
            details.push(format!("{name}: present"));
        }
    }
}

pub(super) fn color_output_summary(inputs: &TerminalCheckInputs) -> String {
    if should_enable_color(
        inputs.no_color_flag,
        inputs.env_present("NO_COLOR"),
        inputs.env_value("TERM"),
        inputs.stdout_is_terminal,
        inputs.stream_supports_color,
    ) {
        return "enabled".to_string();
    }

    let reason = if inputs.no_color_flag {
        "--no-color"
    } else if inputs.env_present("NO_COLOR") {
        "NO_COLOR"
    } else if inputs.env_value("TERM") == Some("dumb") {
        "TERM=dumb"
    } else if !inputs.stdout_is_terminal {
        "stdout is not a terminal"
    } else if !inputs.stream_supports_color {
        "terminal color support not detected"
    } else {
        "disabled"
    };
    format!("disabled ({reason})")
}

fn push_terminfo_details(details: &mut Vec<String>, inputs: &TerminalCheckInputs) -> bool {
    let mut has_warning = false;
    if let Some(raw) = inputs.env_value("TERMINFO") {
        let path = PathBuf::from(raw);
        let (status, warning) = terminal_path_readiness(&path);
        details.push(format!("TERMINFO: {} ({status})", path.display()));
        has_warning |= warning;
    }
    if let Some(raw) = inputs.env_value("TERMINFO_DIRS") {
        for path in env::split_paths(raw).filter(|path| !path.as_os_str().is_empty()) {
            let (status, warning) = terminal_path_readiness(&path);
            details.push(format!(
                "TERMINFO_DIRS entry: {} ({status})",
                path.display()
            ));
            has_warning |= warning;
        }
    } else if inputs.env_present("TERMINFO_DIRS") {
        details.push("TERMINFO_DIRS: present".to_string());
    }
    has_warning
}

fn terminal_path_readiness(path: &Path) -> (String, bool) {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => match std::fs::read_dir(path) {
            Ok(_) => ("dir".to_string(), false),
            Err(err) => (format!("dir unreadable: {err}"), true),
        },
        Ok(metadata) if metadata.is_file() => match read_probe_file(path) {
            Ok(_) => ("file".to_string(), false),
            Err(err) => (format!("file unreadable: {err}"), true),
        },
        Ok(_) => ("not a file or directory".to_string(), true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ("missing".to_string(), true),
        Err(err) => (err.to_string(), true),
    }
}

fn effective_locale(inputs: &TerminalCheckInputs) -> Option<String> {
    LOCALE_ENV_VARS
        .iter()
        .find_map(|name| inputs.env_value(name).map(ToString::to_string))
}

fn is_non_utf8_locale(locale: &str) -> bool {
    let locale = locale.to_ascii_lowercase();
    !(locale.contains("utf-8") || locale.contains("utf8"))
}

fn terminal_size_issues(inputs: &TerminalCheckInputs) -> Vec<DoctorIssue> {
    let mut issues = Vec::new();
    if let Ok((columns, rows)) = inputs.terminal_size {
        if columns > 0 && columns < NARROW_TERMINAL_COLUMNS {
            issues.push(
                DoctorIssue::new(
                    CheckStatus::Warning,
                    format!("width {columns} cols - output may wrap (recommended >=80)"),
                )
                .measured(format!("{columns} x {rows}"))
                .expected(format!(">= {NARROW_TERMINAL_COLUMNS} columns"))
                .remedy("resize the window to at least 80 columns")
                .field("terminal size"),
            );
        }
        if rows > 0 && rows < NARROW_TERMINAL_ROWS {
            issues.push(
                DoctorIssue::new(
                    CheckStatus::Warning,
                    format!("height {rows} rows - content may scroll off (recommended >=24)"),
                )
                .measured(format!("{columns} x {rows}"))
                .expected(format!(">= {NARROW_TERMINAL_ROWS} rows"))
                .remedy("resize the window to at least 24 rows")
                .field("terminal size"),
            );
        }
    }

    if let Some(columns) = inputs
        .env_value("COLUMNS")
        .and_then(|columns| columns.parse::<u16>().ok())
        && columns > 0
        && columns < NARROW_TERMINAL_COLUMNS
    {
        issues.push(
            DoctorIssue::new(
                CheckStatus::Warning,
                format!("COLUMNS={columns} - output may wrap (recommended >=80)"),
            )
            .measured(format!("{columns} columns"))
            .expected(format!(">= {NARROW_TERMINAL_COLUMNS} columns"))
            .remedy("resize the window to at least 80 columns")
            .field("COLUMNS"),
        );
    }
    if let Some(rows) = inputs
        .env_value("LINES")
        .and_then(|rows| rows.parse::<u16>().ok())
        && rows > 0
        && rows < NARROW_TERMINAL_ROWS
    {
        issues.push(
            DoctorIssue::new(
                CheckStatus::Warning,
                format!("LINES={rows} - content may scroll off (recommended >=24)"),
            )
            .measured(format!("{rows} rows"))
            .expected(format!(">= {NARROW_TERMINAL_ROWS} rows"))
            .remedy("resize the window to at least 24 rows")
            .field("LINES"),
        );
    }

    issues
}
