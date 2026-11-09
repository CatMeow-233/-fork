use codex_app_server_protocol::HookEventName;
use codex_app_server_protocol::HookHandlerMetadata;
use codex_app_server_protocol::HookMetadata;
use codex_app_server_protocol::HookSource;
use codex_app_server_protocol::HookTrustStatus;
use codex_app_server_protocol::HooksListEntry;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use strum::IntoEnumIterator;

use self::render::detail_line;
use self::render::detail_wrapped_lines;
use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::picker_style::selection_style;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::render_menu_surface;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::hooks_rpc::HookTrustUpdate;
use crate::hooks_rpc::hook_needs_review;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::renderable::Renderable;
use crate::status::format_directory_display;

const EVENT_COLUMN_WIDTH: usize = 22;
const COUNT_COLUMN_WIDTH: usize = 12;
const MAX_COMMAND_DETAIL_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HooksBrowserPage {
    Events,
    Handlers(HookEventName),
}

pub(crate) struct HooksBrowserView {
    entry: HooksListEntry,
    page: HooksBrowserPage,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    keymap: ListKeymap,
}

impl HooksBrowserView {
    #[cfg(test)]
    pub(crate) fn new(
        hooks: Vec<HookMetadata>,
        warnings: Vec<String>,
        errors: Vec<codex_app_server_protocol::HookErrorInfo>,
        app_event_tx: AppEventSender,
    ) -> Self {
        Self::from_entry(
            HooksListEntry {
                cwd: std::path::PathBuf::new(),
                hooks,
                warnings,
                errors,
            },
            app_event_tx,
            crate::keymap::RuntimeKeymap::defaults().list,
        )
    }

    pub(crate) fn from_entry(
        mut entry: HooksListEntry,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        entry.hooks.sort_by_key(|hook| hook.display_order);
        let mut view = Self {
            entry,
            page: HooksBrowserPage::Events,
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            keymap,
        };
        if view.page_len() > 0 {
            view.state.selected_idx = Some(
                view.event_rows()
                    .iter()
                    .position(|row| row.needs_review > 0)
                    .unwrap_or(0),
            );
        }
        view
    }

    fn event_rows(&self) -> Vec<EventRow> {
        codex_protocol::protocol::HookEventName::iter()
            .map(|event_name| {
                let event_name: HookEventName = event_name.into();
                let installed = self
                    .entry
                    .hooks
                    .iter()
                    .filter(|hook| hook.event_name == event_name)
                    .count();
                let active = self
                    .entry
                    .hooks
                    .iter()
                    .filter(|hook| hook.event_name == event_name && hook_is_active(hook))
                    .count();
                let needs_review = self
                    .entry
                    .hooks
                    .iter()
                    .filter(|hook| hook.event_name == event_name && hook_needs_review(hook))
                    .count();
                EventRow {
                    event_name,
                    installed,
                    active,
                    needs_review,
                }
            })
            .collect()
    }

    fn handlers_for_event(&self, event_name: HookEventName) -> impl Iterator<Item = &HookMetadata> {
        self.entry
            .hooks
            .iter()
            .filter(move |hook| hook.event_name == event_name)
    }

    fn selected_event(&self) -> Option<HookEventName> {
        self.state
            .selected_idx
            .and_then(|idx| codex_protocol::protocol::HookEventName::iter().nth(idx))
            .map(Into::into)
    }

    fn selected_hook_index(&self, event_name: HookEventName) -> Option<usize> {
        let selected_visible_idx = self.state.selected_idx?;
        self.entry
            .hooks
            .iter()
            .enumerate()
            .filter(|(_, hook)| hook.event_name == event_name)
            .nth(selected_visible_idx)
            .map(|(idx, _)| idx)
    }

    fn selected_hook(&self, event_name: HookEventName) -> Option<&HookMetadata> {
        self.selected_hook_index(event_name)
            .and_then(|idx| self.entry.hooks.get(idx))
    }

    fn page_len(&self) -> usize {
        match self.page {
            HooksBrowserPage::Events => codex_protocol::protocol::HookEventName::iter().count(),
            HooksBrowserPage::Handlers(event_name) => self.handlers_for_event(event_name).count(),
        }
    }

    fn open_selected_event(&mut self) {
        let Some(event_name) = self.selected_event() else {
            return;
        };
        self.page = HooksBrowserPage::Handlers(event_name);
        self.state = ScrollState::new();
        if self.page_len() > 0 {
            self.state.selected_idx = Some(0);
        }
    }

    fn toggle_selected_hook(&mut self, event_name: HookEventName) {
        let Some(idx) = self.selected_hook_index(event_name) else {
            return;
        };
        let Some(hook) = self.entry.hooks.get_mut(idx) else {
            return;
        };
        if hook.is_managed {
            return;
        }
        if hook_needs_review(hook) {
            return;
        }

        hook.enabled = !hook.enabled;
        self.app_event_tx.send(AppEvent::SetHookEnabled {
            key: hook.key.clone(),
            enabled: hook.enabled,
        });
    }

    fn trust_selected_hook(&mut self, event_name: HookEventName) {
        let Some(idx) = self.selected_hook_index(event_name) else {
            return;
        };
        let Some(hook) = self.entry.hooks.get_mut(idx) else {
            return;
        };
        if !hook_needs_review(hook) {
            return;
        }

        hook.trust_status = HookTrustStatus::Trusted;
        self.app_event_tx.send(AppEvent::TrustHook {
            key: hook.key.clone(),
            current_hash: hook.current_hash.clone(),
        });
    }

    fn trust_all_hooks(&mut self) {
        let mut updates = Vec::new();
        for hook in &mut self.entry.hooks {
            if !hook_needs_review(hook) {
                continue;
            }

            hook.trust_status = HookTrustStatus::Trusted;
            updates.push(HookTrustUpdate {
                key: hook.key.clone(),
                current_hash: hook.current_hash.clone(),
            });
        }
        if !updates.is_empty() {
            self.app_event_tx.send(AppEvent::TrustHooks { updates });
        }
    }

    fn close(&mut self) {
        self.complete = true;
    }

    fn return_to_events(&mut self) {
        let selected_event_name = match self.page {
            HooksBrowserPage::Events => None,
            HooksBrowserPage::Handlers(event_name) => Some(event_name),
        };
        self.page = HooksBrowserPage::Events;
        self.state = ScrollState::new();
        self.state.selected_idx = selected_event_name
            .and_then(|event_name| {
                codex_protocol::protocol::HookEventName::iter()
                    .position(|candidate| HookEventName::from(candidate) == event_name)
            })
            .or_else(|| (self.page_len() > 0).then_some(0));
    }

    fn event_header_lines() -> Vec<Line<'static>> {
        vec![
            "Hooks".bold().into(),
            "Lifecycle hooks from config and enabled plugins."
                .dim()
                .into(),
        ]
    }

    fn review_needed_total_count(&self) -> usize {
        self.entry
            .hooks
            .iter()
            .filter(|hook| hook_needs_review(hook))
            .count()
    }

    #[allow(clippy::disallowed_methods)]
    fn handler_header_lines(
        event_name: HookEventName,
        review_needed_count: usize,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![format!("{} hooks", event_label(event_name)).bold().into()];
        match review_needed_message(review_needed_count) {
            None => lines.push(
                "Turn hooks on or off. Your changes are saved automatically."
                    .dim()
                    .into(),
            ),
            Some(message) => lines.push(message.yellow().into()),
        }
        lines
    }

    fn review_needed_count(&self, event_name: HookEventName) -> usize {
        self.handlers_for_event(event_name)
            .filter(|hook| hook_needs_review(hook))
            .count()
    }

    #[allow(clippy::disallowed_methods)]
    fn event_table_lines(&self) -> Vec<Line<'static>> {
        let rows = self.event_rows();
        let show_review = rows.iter().any(|row| row.needs_review > 0);
        let mut lines = Vec::new();
        let mut header = vec![
            format!("{:<EVENT_COLUMN_WIDTH$}", "Event").into(),
            format!("{:<COUNT_COLUMN_WIDTH$}", "Installed").into(),
            format!("{:<COUNT_COLUMN_WIDTH$}", "Active").into(),
        ];
        if show_review {
            header.push(format!("{:<COUNT_COLUMN_WIDTH$}", "Review").into());
        }
        header.push("Description".into());
        lines.push(Line::from(header));
        for (idx, row) in rows.into_iter().enumerate() {
            let selected = self.state.selected_idx == Some(idx);
            let needs_review = row.needs_review > 0;
            let mut row_line = vec![
                Span::from(format!(
                    "{:<EVENT_COLUMN_WIDTH$}",
                    event_label(row.event_name)
                )),
                Span::from(format!("{:<COUNT_COLUMN_WIDTH$}", row.installed)),
                Span::from(format!("{:<COUNT_COLUMN_WIDTH$}", row.active)),
            ];
            if show_review {
                let review_count = Span::from(format!("{:<COUNT_COLUMN_WIDTH$}", row.needs_review));
                row_line.push(if needs_review {
                    review_count.yellow()
                } else {
                    review_count
                });
            }
            row_line.push(Span::from(event_description(row.event_name)));

            if selected {
                let style = selection_style();
                for span in &mut row_line {
                    *span = span.clone().set_style(style);
                }
            } else {
                row_line[1] = row_line[1].clone().dim();
                row_line[2] = row_line[2].clone().dim();
                if show_review && !needs_review {
                    row_line[3] = row_line[3].clone().dim();
                }
                let description_idx = row_line.len() - 1;
                row_line[description_idx] = row_line[description_idx].clone().dim();
            }
            lines.push(if selected {
                Line::from(row_line).style(selection_style())
            } else {
                Line::from(row_line)
            });
        }
        lines
    }

    fn event_issue_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if self.entry.warnings.is_empty() && self.entry.errors.is_empty() {
            return lines;
        }

        lines.push("Issues".bold().into());
        lines.extend(
            self.entry
                .warnings
                .iter()
                .map(|warning| format!("⚠ {warning}").into()),
        );
        lines.extend(self.entry.errors.iter().map(|error| {
            format!("■ {}: {}", error.path.display(), error.message)
                .red()
                .into()
        }));
        lines
    }

    #[allow(clippy::disallowed_methods)]
    fn event_page_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Self::event_header_lines();
        lines.push(Line::default());

        if let Some(message) = review_needed_message(self.review_needed_total_count()) {
            lines.push(format!("⚠ {message}").yellow().into());
            lines.push(Line::default());
        }

        let issue_lines = self.event_issue_lines();
        if !issue_lines.is_empty() {
            lines.extend(issue_lines);
            lines.push(Line::default());
        }

        lines.extend(self.event_table_lines());
        lines
    }

    #[allow(clippy::disallowed_methods)]
    fn handler_row_lines(&self, event_name: HookEventName, width: usize) -> Vec<Line<'static>> {
        let index_width = self
            .handlers_for_event(event_name)
            .count()
            .to_string()
            .len()
            .max(2);
        self.handlers_for_event(event_name)
            .enumerate()
            .map(|(idx, hook)| {
                let marker = if hook_needs_review(hook) {
                    '!'
                } else if hook_is_active(hook) {
                    'x'
                } else {
                    ' '
                };
                let title = hook
                    .status_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or("Unnamed hook");
                let trust_suffix = match hook.trust_status {
                    HookTrustStatus::Modified => " · modified",
                    HookTrustStatus::Untrusted => " · new",
                    HookTrustStatus::Managed | HookTrustStatus::Trusted => "",
                };
                let prefix = if self.state.selected_idx == Some(idx) {
                    "› "
                } else {
                    "  "
                };
                let mut line = Line::from(vec![
                    format!("{prefix}[{marker}] ").into(),
                    format!("{:>index_width$}", idx + 1).dim(),
                    format!("  {title}{trust_suffix}").into(),
                ]);
                line = truncate_line_with_ellipsis_if_overflow(line, width);
                let needs_review = hook_needs_review(hook);
                if self.state.selected_idx == Some(idx) {
                    line = line.patch_style(selection_style());
                } else if needs_review {
                    line = line.yellow();
                } else if hook.is_managed {
                    line = line.dim();
                }
                line
            })
            .collect()
    }

    fn detail_lines(
        &self,
        event_name: HookEventName,
        width: usize,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        let Some(hook) = self.selected_hook(event_name) else {
            return crate::terminal_hyperlinks::plain_hyperlink_lines(vec![
                "No hooks installed for this event.".dim().into(),
            ]);
        };

        let mut lines = vec![detail_line("Event", event_label(event_name))];
        if let Some(matcher) = hook.matcher.as_deref() {
            lines.extend(detail_wrapped_lines(
                "Matcher", matcher, width, /*max_lines*/ None,
            ));
        }
        lines.extend(detail_wrapped_lines(
            "Source",
            &detail_source_value(hook),
            width,
            /*max_lines*/ None,
        ));
        match &hook.handler {
            HookHandlerMetadata::Command { command, r#async } => {
                lines.extend(detail_wrapped_lines(
                    "Command",
                    command,
                    width,
                    Some(MAX_COMMAND_DETAIL_LINES),
                ));
                lines.push(detail_line("Mode", if *r#async { "Async" } else { "Sync" }));
            }
            HookHandlerMetadata::McpTool { server, tool } => {
                lines.extend(detail_wrapped_lines(
                    "MCP Server",
                    server,
                    width,
                    /*max_lines*/ None,
                ));
                lines.extend(detail_wrapped_lines(
                    "MCP Tool", tool, width, /*max_lines*/ None,
                ));
            }
            HookHandlerMetadata::Prompt {} => {
                lines.push(detail_line("Handler", "Prompt"));
            }
            HookHandlerMetadata::Agent {} => {
                lines.push(detail_line("Handler", "Agent"));
            }
        }
        lines.push(detail_line("Timeout", &format!("{}s", hook.timeout_sec)));
        if let Some(limit) = hook.additional_context_limit {
            let value = if limit == 0 {
                "unlimited".to_string()
            } else {
                format!("limit: {limit} approximate tokens")
            };
            lines.push(detail_line("Context", &value));
        }
        lines.push(detail_line("Trust", hook_trust_label(hook.trust_status)));
        lines
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let hint_area = Rect {
            x: area.x + 2,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: area.height,
        };
        let accept = self.keymap.primary_hint(ListAction::Accept);
        let cancel = self
            .keymap
            .primary_hint(ListAction::Cancel)
            .unwrap_or_else(|| key_hint::plain(KeyCode::Esc).into());
        let mut spans = Vec::new();
        match self.page {
            HooksBrowserPage::Events => {
                if self.review_needed_total_count() > 0 {
                    spans.extend(key_hint::plain(KeyCode::Char('t')).spans());
                    spans.push(" trust all · ".dim());
                    if let Some(accept) = accept {
                        spans.extend(accept.spans());
                        spans.push(" review · ".dim());
                    }
                } else if let Some(accept) = accept {
                    spans.extend(accept.spans());
                    spans.push(" details · ".dim());
                }
                spans.extend(cancel.spans());
                spans.push(" close".dim());
            }
            HooksBrowserPage::Handlers(event_name) => {
                if let Some(hook) = self.selected_hook(event_name) {
                    if hook.is_managed {
                        spans.push("Managed (always on) · ".dim());
                    } else if hook_needs_review(hook) {
                        spans.extend(key_hint::plain(KeyCode::Char('t')).spans());
                        spans.push(" trust · ".dim());
                    } else {
                        let mut toggle = key_hint::plain(KeyCode::Char(' ')).display_label();
                        if let Some(accept) = accept {
                            toggle.push('/');
                            toggle.push_str(&accept.display_label());
                        }
                        spans.extend(key_hint::key_label_spans(&toggle));
                        spans.push(" toggle · ".dim());
                    }
                }
                spans.extend(cancel.spans());
                spans.push(" back".dim());
            }
        }
        let footer = Line::from(spans);
        footer.dim().render(hint_area, buf);
    }
}

impl BottomPaneView for HooksBrowserView {
    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        crate::keymap::KeymapContextSet::new(crate::keymap::KeymapContext::List)
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if let Some(action) = self.keymap.vertical_action_for(key_event) {
            let len = self.page_len();
            self.state
                .apply_vertical_action(action, len, MAX_POPUP_ROWS.min(len.max(1)));
            return;
        }

        match key_event {
            _ if self.keymap.accept.is_pressed(key_event)
                && self.page == HooksBrowserPage::Events =>
            {
                self.open_selected_event()
            }
            _ if self.keymap.accept.is_pressed(key_event) => {
                if let HooksBrowserPage::Handlers(event_name) = self.page {
                    self.toggle_selected_hook(event_name);
                }
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let HooksBrowserPage::Handlers(event_name) = self.page {
                    self.toggle_selected_hook(event_name);
                }
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: KeyModifiers::NONE,
                ..
            } => match self.page {
                HooksBrowserPage::Events => self.trust_all_hooks(),
                HooksBrowserPage::Handlers(event_name) => self.trust_selected_hook(event_name),
            },
            _ if self.keymap.cancel.is_pressed(key_event) => match self.page {
                HooksBrowserPage::Events => self.close(),
                HooksBrowserPage::Handlers(_) => self.return_to_events(),
            },
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.close();
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}

#[path = "hooks_browser_render.rs"]
mod render;

fn hook_is_active(hook: &HookMetadata) -> bool {
    hook.enabled
        && matches!(
            hook.trust_status,
            HookTrustStatus::Managed | HookTrustStatus::Trusted
        )
}

fn review_needed_message(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 hook needs review before it can run.".to_string()),
        count => Some(format!("{count} hooks need review before they can run.")),
    }
}

struct EventRow {
    event_name: HookEventName,
    installed: usize,
    active: usize,
    needs_review: usize,
}

fn hook_trust_label(status: HookTrustStatus) -> &'static str {
    match status {
        HookTrustStatus::Managed => "Managed",
        HookTrustStatus::Trusted => "Trusted",
        HookTrustStatus::Untrusted => "New hook - review required",
        HookTrustStatus::Modified => "Modified since last trusted - review required",
    }
}

fn event_label(event_name: HookEventName) -> &'static str {
    match event_name {
        HookEventName::PreToolUse => "PreToolUse",
        HookEventName::PermissionRequest => "PermissionRequest",
        HookEventName::PostToolUse => "PostToolUse",
        HookEventName::PreCompact => "PreCompact",
        HookEventName::PostCompact => "PostCompact",
        HookEventName::PreModelRequest => "PreModelRequest",
        HookEventName::PostModelRequest => "PostModelRequest",
        HookEventName::SessionStart => "SessionStart",
        HookEventName::SessionEnd => "SessionEnd",
        HookEventName::UserPromptSubmit => "UserPromptSubmit",
        HookEventName::SubagentStart => "SubagentStart",
        HookEventName::SubagentStop => "SubagentStop",
        HookEventName::Stop => "Stop",
        HookEventName::Interrupt => "Interrupt",
    }
}

fn event_description(event_name: HookEventName) -> &'static str {
    match event_name {
        HookEventName::PreToolUse => "Before a tool executes",
        HookEventName::PermissionRequest => "When permission is requested",
        HookEventName::PostToolUse => "After a tool executes",
        HookEventName::PreCompact => "Before context compaction",
        HookEventName::PostCompact => "After context compaction",
        HookEventName::PreModelRequest => "Before a model request",
        HookEventName::PostModelRequest => "After a model request, including failures",
        HookEventName::SessionStart => "When a new session starts",
        HookEventName::SessionEnd => "Right before a session ends",
        HookEventName::UserPromptSubmit => "When the user submits a prompt",
        HookEventName::SubagentStart => "When a subagent is created",
        HookEventName::SubagentStop => "Right before a subagent ends its turn",
        HookEventName::Stop => "Right before Codex ends its turn",
        HookEventName::Interrupt => "Right before an interrupted turn is aborted",
    }
}

fn hook_source_summary(hook: &HookMetadata) -> String {
    if let Some(id) = &hook.environment_id {
        return format!("Environment - {id}");
    }
    match hook.source {
        HookSource::Plugin => hook
            .plugin_id
            .as_deref()
            .map(|plugin_id| format!("Plugin - {plugin_id}"))
            .unwrap_or_else(|| "Plugin".to_string()),
        _ => config_source_label(hook.source).to_string(),
    }
}

fn detail_source_value(hook: &HookMetadata) -> String {
    if hook.environment_id.is_some() {
        return hook_source_summary(hook);
    }
    match hook.source {
        HookSource::Plugin => hook_source_summary(hook),
        HookSource::System
        | HookSource::Mdm
        | HookSource::CloudRequirements
        | HookSource::CloudManagedConfig
        | HookSource::LegacyManagedConfigFile
        | HookSource::LegacyManagedConfigMdm => config_source_label(hook.source).to_string(),
        _ => format!(
            "{} - {}",
            config_source_label(hook.source),
            format_directory_display(&hook.source_path, /*max_width*/ None)
        ),
    }
}

fn config_source_label(source: HookSource) -> &'static str {
    match source {
        HookSource::System => "Admin config",
        HookSource::User => "User config",
        HookSource::Project => "Project config",
        HookSource::Mdm => "Admin config",
        HookSource::SessionFlags => "Session flags",
        HookSource::Plugin => unreachable!("plugin hooks are handled by summary_source"),
        HookSource::CloudRequirements => "Admin config",
        HookSource::CloudManagedConfig => "Cloud-managed config",
        HookSource::LegacyManagedConfigFile => "Admin config",
        HookSource::LegacyManagedConfigMdm => "Admin config",
        HookSource::Unknown => "Unknown source",
    }
}

#[cfg(test)]
#[path = "hooks_browser_view_tests.rs"]
mod tests;
