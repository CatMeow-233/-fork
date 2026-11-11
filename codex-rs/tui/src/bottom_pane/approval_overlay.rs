//! Approval modal rendering and decision routing for high-risk operations.
//!
//! This module converts agent approval requests (exec/apply-patch/MCP
//! elicitation) into a list-selection view with action-specific options and
//! shortcuts. It owns two important contracts:
//!
//! 1. Selection always emits an explicit decision event back to the app.
//! 2. MCP elicitation keeps `Esc` mapped to `Cancel`, even with custom
//!    keybindings, so dismissal never silently becomes "continue without info".
//!
//! This module does not evaluate whether an action is safe to run; it only
//! presents choices and routes user decisions.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::app_server_requests::ResolvedAppServerRequest;
#[cfg(test)]
use crate::app_command::AppCommand as Op;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::list_selection_view::ListSelectionView;
use crate::bottom_pane::list_selection_view::SelectionItem;
use crate::bottom_pane::list_selection_view::SelectionViewParams;
use crate::bottom_pane::popup_consts::accept_cancel_hint_line;
use crate::diff_model::FileChange;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell;
use crate::history_cell::ReviewDecision;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ApprovalKeymap;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use codex_app_server_protocol::AdditionalPermissionProfile;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionApprovalKind;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::FileSystemAccessMode;
use codex_app_server_protocol::FileSystemPath;
use codex_app_server_protocol::FileSystemSandboxEntry;
use codex_app_server_protocol::FileSystemSpecialPath;
use codex_app_server_protocol::McpServerElicitationAction;
use codex_app_server_protocol::NetworkApprovalContext;
use codex_app_server_protocol::NetworkApprovalProtocol;
use codex_app_server_protocol::NetworkPolicyRuleAction;
use codex_app_server_protocol::RequestId;
use codex_features::Features;
use codex_protocol::ThreadId;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::LegacyAppPathString;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

/// Request coming from the agent that needs user approval.
#[derive(Clone, Debug)]
pub(crate) enum ApprovalRequest {
    Exec(ExecApprovalRequest),
    Permissions(PermissionsApprovalRequest),
    ApplyPatch(ApplyPatchApprovalRequest),
    McpElicitation(McpElicitationApprovalRequest),
}

#[derive(Clone, Debug)]
pub(crate) struct ExecApprovalRequest {
    pub kind: CommandExecutionApprovalKind,
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub id: String,
    pub environment_id: Option<String>,
    pub command: Vec<String>,
    pub reason: Option<String>,
    pub available_decisions: Vec<CommandExecutionApprovalDecision>,
    pub network_approval_context: Option<NetworkApprovalContext>,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
}

#[derive(Clone, Debug)]
pub(crate) struct PermissionsApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub call_id: String,
    pub environment_id: Option<String>,
    pub reason: Option<String>,
    pub permissions: RequestPermissionProfile,
}

#[derive(Clone, Debug)]
pub(crate) struct ApplyPatchApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub id: String,
    pub reason: Option<String>,
    pub cwd: AbsolutePathBuf,
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Clone, Debug)]
pub(crate) struct McpElicitationApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub server_name: String,
    pub request_id: RequestId,
    pub message: String,
}

impl ApprovalRequest {
    pub(super) fn program_status_blocked_kind(
        &self,
    ) -> crate::terminal_program_status::ProgramStatusKind {
        use crate::terminal_program_status::ProgramStatusKind;
        match self {
            Self::Exec(_) | Self::Permissions(_) | Self::ApplyPatch(_) => {
                ProgramStatusKind::Permission
            }
            Self::McpElicitation(_) => ProgramStatusKind::Question,
        }
    }

    pub(super) fn thread_id(&self) -> ThreadId {
        match self {
            ApprovalRequest::Exec(request) => request.thread_id,
            ApprovalRequest::Permissions(request) => request.thread_id,
            ApprovalRequest::ApplyPatch(request) => request.thread_id,
            ApprovalRequest::McpElicitation(request) => request.thread_id,
        }
    }

    fn thread_label(&self) -> Option<&str> {
        match self {
            ApprovalRequest::Exec(request) => request.thread_label.as_deref(),
            ApprovalRequest::Permissions(request) => request.thread_label.as_deref(),
            ApprovalRequest::ApplyPatch(request) => request.thread_label.as_deref(),
            ApprovalRequest::McpElicitation(request) => request.thread_label.as_deref(),
        }
    }

    pub(super) fn matches_resolved_request(&self, request: &ResolvedAppServerRequest) -> bool {
        match (self, request) {
            (
                ApprovalRequest::Exec(request),
                ResolvedAppServerRequest::ExecApproval { thread_id, id },
            ) => request.thread_id.to_string() == *thread_id && request.id == *id,
            (
                ApprovalRequest::Permissions(request),
                ResolvedAppServerRequest::PermissionsApproval { thread_id, id },
            ) => request.thread_id.to_string() == *thread_id && request.call_id == *id,
            (
                ApprovalRequest::ApplyPatch(request),
                ResolvedAppServerRequest::FileChangeApproval { thread_id, id },
            ) => request.thread_id.to_string() == *thread_id && request.id == *id,
            (
                ApprovalRequest::McpElicitation(request),
                ResolvedAppServerRequest::McpElicitation {
                    server_name: resolved_server_name,
                    request_id: resolved_request_id,
                },
            ) => {
                request.server_name == *resolved_server_name
                    && request.request_id == *resolved_request_id
            }
            _ => false,
        }
    }
}

/// Modal overlay asking the user to approve or deny one or more requests.
pub(crate) struct ApprovalOverlay {
    current_request: Option<ApprovalRequest>,
    queue: Vec<ApprovalRequest>,
    app_event_tx: AppEventSender,
    list: ListSelectionView,
    options: Vec<ApprovalOption>,
    current_complete: bool,
    done: bool,
    features: Features,
    approval_keymap: ApprovalKeymap,
    list_keymap: ListKeymap,
}

impl ApprovalOverlay {
    pub fn new(
        request: ApprovalRequest,
        app_event_tx: AppEventSender,
        features: Features,
        approval_keymap: ApprovalKeymap,
        list_keymap: ListKeymap,
    ) -> Self {
        let mut view = Self {
            current_request: None,
            queue: Vec::new(),
            app_event_tx: app_event_tx.clone(),
            list: ListSelectionView::new(Default::default(), app_event_tx, list_keymap.clone()),
            options: Vec::new(),
            current_complete: false,
            done: false,
            features,
            approval_keymap,
            list_keymap,
        };
        view.set_current(request);
        view
    }

    pub fn enqueue_request(&mut self, req: ApprovalRequest) {
        self.queue.push(req);
    }

    fn dismiss_resolved_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        let queue_len = self.queue.len();
        self.queue
            .retain(|queued_request| !queued_request.matches_resolved_request(request));
        if self
            .current_request
            .as_ref()
            .is_some_and(|current_request| current_request.matches_resolved_request(request))
        {
            self.current_complete = true;
            self.advance_queue();
            return true;
        }

        self.queue.len() != queue_len
    }

    fn set_current(&mut self, request: ApprovalRequest) {
        self.current_complete = false;
        let header = build_header(&request);
        let (options, params) = Self::build_options(
            &request,
            header,
            &self.features,
            &self.approval_keymap,
            &self.list_keymap,
        );
        self.current_request = Some(request);
        self.options = options;
        self.list =
            ListSelectionView::new(params, self.app_event_tx.clone(), self.list_keymap.clone());
    }

    fn build_options(
        request: &ApprovalRequest,
        header: Box<dyn Renderable>,
        _features: &Features,
        approval_keymap: &ApprovalKeymap,
        list_keymap: &ListKeymap,
    ) -> (Vec<ApprovalOption>, SelectionViewParams) {
        let (options, title) = match request {
            ApprovalRequest::Exec(request) => {
                let title = if request.kind == CommandExecutionApprovalKind::WriteStdin {
                    request.command.get(2).map_or_else(
                        || "Would you like to send input to the existing terminal?".to_string(),
                        |process_id| {
                            format!("Would you like to send input to terminal {process_id}?")
                        },
                    )
                } else {
                    request.network_approval_context.as_ref().map_or_else(
                        || "Would you like to run the following command?".to_string(),
                        |network_approval_context| {
                            format!(
                                "Do you want to approve network access to \"{}\"?",
                                network_approval_context.host
                            )
                        },
                    )
                };
                (
                    exec_options(
                        &request.available_decisions,
                        request.network_approval_context.as_ref(),
                        request.additional_permissions.as_ref(),
                        approval_keymap,
                    ),
                    title,
                )
            }
            ApprovalRequest::Permissions(_) => (
                permissions_options(approval_keymap),
                "Would you like to grant these permissions?".to_string(),
            ),
            ApprovalRequest::ApplyPatch(_) => (
                patch_options(approval_keymap),
                "Would you like to make the following edits?".to_string(),
            ),
            ApprovalRequest::McpElicitation(request) => (
                elicitation_options(approval_keymap),
                format!("{} needs your approval.", request.server_name),
            ),
        };

        let header = Box::new(ColumnRenderable::with([
            Paragraph::new(title.bold())
                .wrap(Wrap { trim: false })
                .into(),
            Line::from("").into(),
            header,
        ]));

        let items = options
            .iter()
            .map(|opt| SelectionItem {
                name: opt.label.clone(),
                display_shortcut: approval_keymap.hint_for_bindings(&opt.shortcuts),
                dismiss_on_select: false,
                ..Default::default()
            })
            .collect();

        let params = SelectionViewParams {
            footer_hint: Some(approval_footer_hint(request, approval_keymap, list_keymap)),
            items,
            header,
            header_view_all_hint: approval_keymap
                .primary_hint("open_fullscreen", &approval_keymap.open_fullscreen),
            ..SelectionViewParams::picker()
        };

        (options, params)
    }

    fn apply_selection(&mut self, actual_idx: usize) {
        if self.current_complete {
            return;
        }
        let Some(option) = self.options.get(actual_idx) else {
            return;
        };
        if let Some(request) = self.current_request.as_ref() {
            match (request, &option.decision) {
                (ApprovalRequest::Exec(request), ApprovalDecision::Command(decision)) => {
                    self.handle_exec_decision(&request.id, &request.command, decision.clone());
                }
                (
                    ApprovalRequest::Permissions(request),
                    ApprovalDecision::Permissions(decision),
                ) => {
                    self.handle_permissions_decision(
                        &request.call_id,
                        &request.permissions,
                        *decision,
                    );
                }
                (ApprovalRequest::ApplyPatch(request), ApprovalDecision::FileChange(decision)) => {
                    self.handle_patch_decision(&request.id, decision.clone());
                }
                (
                    ApprovalRequest::McpElicitation(request),
                    ApprovalDecision::McpElicitation(decision),
                ) => {
                    self.handle_elicitation_decision(
                        &request.server_name,
                        &request.request_id,
                        *decision,
                    );
                }
                _ => {}
            }
        }

        self.current_complete = true;
        self.advance_queue();
    }

    fn handle_exec_decision(
        &self,
        id: &str,
        command: &[String],
        decision: CommandExecutionApprovalDecision,
    ) {
        let Some(request) = self.current_request.as_ref() else {
            return;
        };
        if request.thread_label().is_none() {
            let network_approval_context = match request {
                ApprovalRequest::Exec(request) => request.network_approval_context.as_ref(),
                ApprovalRequest::Permissions(_)
                | ApprovalRequest::ApplyPatch(_)
                | ApprovalRequest::McpElicitation(_) => None,
            };
            let subject = if let Some(network_approval_context) = network_approval_context {
                history_cell::ApprovalDecisionSubject::NetworkAccess {
                    target: network_approval_target(network_approval_context, command),
                }
            } else if let Some(target) = network_approval_command_target(command) {
                history_cell::ApprovalDecisionSubject::NetworkAccess {
                    target: target.to_string(),
                }
            } else {
                history_cell::ApprovalDecisionSubject::Command(command.to_vec())
            };
            let cell = history_cell::new_approval_decision_cell(
                subject,
                command_decision_to_review_decision(&decision),
                history_cell::ApprovalDecisionActor::User,
            );
            self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
        }
        let thread_id = request.thread_id();
        self.app_event_tx
            .exec_approval(thread_id, id.to_string(), decision);
    }

    fn handle_permissions_decision(
        &self,
        call_id: &str,
        permissions: &RequestPermissionProfile,
        decision: PermissionsDecision,
    ) {
        let Some(request) = self.current_request.as_ref() else {
            return;
        };
        let granted_permissions = match decision {
            PermissionsDecision::GrantForTurn
            | PermissionsDecision::GrantForTurnWithStrictAutoReview
            | PermissionsDecision::GrantForSession => permissions.clone(),
            PermissionsDecision::Deny => Default::default(),
        };
        let scope = if matches!(decision, PermissionsDecision::GrantForSession) {
            PermissionGrantScope::Session
        } else {
            PermissionGrantScope::Turn
        };
        let strict_auto_review = matches!(
            decision,
            PermissionsDecision::GrantForTurnWithStrictAutoReview
        );
        if request.thread_label().is_none() {
            let message = if granted_permissions.is_empty() {
                "You did not grant additional permissions"
            } else if strict_auto_review {
                "You granted additional permissions with strict auto review"
            } else if matches!(scope, PermissionGrantScope::Session) {
                "You granted additional permissions for this session"
            } else {
                "You granted additional permissions"
            };
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                crate::history_cell::PlainHistoryCell::new(vec![message.into()]),
            )));
        }
        let thread_id = request.thread_id();
        self.app_event_tx.request_permissions_response(
            thread_id,
            call_id.to_string(),
            codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: granted_permissions,
                scope,
                strict_auto_review,
            },
        );
    }

    fn handle_patch_decision(&self, id: &str, decision: FileChangeApprovalDecision) {
        let Some(thread_id) = self
            .current_request
            .as_ref()
            .map(ApprovalRequest::thread_id)
        else {
            return;
        };
        self.app_event_tx
            .patch_approval(thread_id, id.to_string(), decision);
    }

    fn handle_elicitation_decision(
        &self,
        server_name: &str,
        request_id: &RequestId,
        decision: McpServerElicitationAction,
    ) {
        let Some(thread_id) = self
            .current_request
            .as_ref()
            .map(ApprovalRequest::thread_id)
        else {
            return;
        };
        self.app_event_tx.resolve_elicitation(
            thread_id,
            server_name.to_string(),
            request_id.clone(),
            decision,
            /*content*/ None,
            /*meta*/ None,
        );
    }

    fn advance_queue(&mut self) {
        if let Some(next) = self.queue.pop() {
            self.set_current(next);
        } else {
            self.done = true;
        }
    }

    fn cancel_current_request(&mut self) {
        if self.done {
            return;
        }
        if !self.current_complete
            && let Some(request) = self.current_request.as_ref()
        {
            match request {
                ApprovalRequest::Exec(request) => {
                    self.handle_exec_decision(
                        &request.id,
                        &request.command,
                        CommandExecutionApprovalDecision::Cancel,
                    );
                }
                ApprovalRequest::Permissions(request) => {
                    self.handle_permissions_decision(
                        &request.call_id,
                        &request.permissions,
                        PermissionsDecision::Deny,
                    );
                }
                ApprovalRequest::ApplyPatch(request) => {
                    self.handle_patch_decision(&request.id, FileChangeApprovalDecision::Cancel);
                }
                ApprovalRequest::McpElicitation(request) => {
                    self.handle_elicitation_decision(
                        &request.server_name,
                        &request.request_id,
                        McpServerElicitationAction::Cancel,
                    );
                }
            }
        }
        self.queue.clear();
        self.done = true;
    }

    /// Apply approval-specific shortcuts before delegating to list navigation.
    ///
    /// `open_fullscreen` is handled here because it is orthogonal to list item
    /// selection and should work regardless of current highlighted row.
    fn try_handle_shortcut(&mut self, key_event: &KeyEvent) -> bool {
        if key_event.kind == KeyEventKind::Press
            && self.approval_keymap.open_fullscreen.is_pressed(*key_event)
            && let Some(request) = self.current_request.as_ref()
        {
            self.app_event_tx
                .send(AppEvent::FullScreenApprovalRequest(request.clone()));
            return true;
        }

        if key_event.kind == KeyEventKind::Press
            && self.approval_keymap.open_thread.is_pressed(*key_event)
            && let Some(request) = self.current_request.as_ref()
            && request.thread_label().is_some()
        {
            self.app_event_tx
                .send(AppEvent::SelectAgentThread(request.thread_id()));
            return true;
        }

        if self.list_keymap.cancel.is_pressed(*key_event) {
            self.cancel_current_request();
            return true;
        }

        if let Some(idx) = self
            .options
            .iter()
            .position(|opt| opt.shortcuts.iter().any(|s| s.is_press(*key_event)))
        {
            self.apply_selection(idx);
            true
        } else {
            false
        }
    }
}

impl BottomPaneView for ApprovalOverlay {
    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        crate::keymap::KeymapContextSet::new(crate::keymap::KeymapContext::Approval)
            .with(crate::keymap::KeymapContext::List)
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.try_handle_shortcut(&key_event) {
            return;
        }
        self.list.handle_key_event(key_event);
        if let Some(idx) = self.list.take_last_selected_index() {
            self.apply_selection(idx);
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.cancel_current_request();
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.done
    }

    fn current_patch_approval(&self) -> Option<&ApplyPatchApprovalRequest> {
        let ApprovalRequest::ApplyPatch(patch) = self.current_request.as_ref()? else {
            return None;
        };
        Some(patch)
    }

    fn try_consume_approval_request(
        &mut self,
        request: ApprovalRequest,
    ) -> Option<ApprovalRequest> {
        self.enqueue_request(request);
        None
    }

    fn dismiss_app_server_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        self.dismiss_resolved_request(request)
    }

    fn terminal_title_requires_action(&self) -> bool {
        true
    }

    fn program_status_blocked_kind(
        &self,
    ) -> Option<crate::terminal_program_status::ProgramStatusKind> {
        self.current_request
            .as_ref()
            .map(ApprovalRequest::program_status_blocked_kind)
    }
}

impl Renderable for ApprovalOverlay {
    fn desired_height(&self, width: u16) -> u16 {
        self.list.desired_height(width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.list.render(area, buf);
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.list.cursor_pos(area)
    }
}

fn approval_footer_hint(
    request: &ApprovalRequest,
    approval_keymap: &ApprovalKeymap,
    list_keymap: &ListKeymap,
) -> Line<'static> {
    let mut spans = accept_cancel_hint_line(
        list_keymap.primary_hint(ListAction::Accept),
        "to confirm",
        list_keymap.primary_hint(ListAction::Cancel),
        "to cancel",
    )
    .spans;
    if request.thread_label().is_some()
        && let Some(open_thread) =
            approval_keymap.primary_hint("open_thread", &approval_keymap.open_thread)
    {
        if !spans.is_empty() {
            spans.push(" or ".into());
        } else {
            spans.push("Press ".into());
        }
        spans.extend([open_thread.into(), " to open thread".into()]);
    }
    Line::from(spans)
}

fn network_approval_target(
    network_approval_context: &NetworkApprovalContext,
    command: &[String],
) -> String {
    if let Some(target) = network_approval_command_target(command) {
        return target.to_string();
    }

    let scheme = match network_approval_context.protocol {
        NetworkApprovalProtocol::Http => "http",
        NetworkApprovalProtocol::Https => "https",
        NetworkApprovalProtocol::Socks5Tcp => "socks5-tcp",
        NetworkApprovalProtocol::Socks5Udp => "socks5-udp",
    };
    format!("{scheme}://{}", network_approval_context.host)
}

fn network_approval_command_target(command: &[String]) -> Option<&str> {
    match command {
        [program, target] if program == "network-access" && !target.is_empty() => {
            Some(target.as_str())
        }
        [command] => command
            .strip_prefix("network-access ")
            .filter(|target| !target.is_empty()),
        _ => None,
    }
}

/// Thread, environment and reason rows for the values present, each followed by a blank line.
fn request_context_lines(
    thread_label: Option<&str>,
    environment_id: Option<&str>,
    reason: Option<&str>,
) -> Vec<Line<'static>> {
    [
        (
            "Thread: ",
            thread_label.map(|value| value.to_string().bold()),
        ),
        (
            "Environment: ",
            environment_id.map(|value| value.to_string().bold()),
        ),
        ("Reason: ", reason.map(|value| value.to_string().italic())),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.map(|value| Line::from(vec![label.into(), value])))
    .flat_map(|line| [line, Line::from("")])
    .collect()
}

/// The labelled thread row, if any, followed by a blank line.
pub(super) fn thread_label_lines(thread_label: Option<&str>) -> Vec<Line<'static>> {
    request_context_lines(
        thread_label,
        /*environment_id*/ None,
        /*reason*/ None,
    )
}

fn build_header(request: &ApprovalRequest) -> Box<dyn Renderable> {
    match request {
        ApprovalRequest::Exec(request) => {
            let mut header = request_context_lines(
                request.thread_label.as_deref(),
                request.environment_id.as_deref(),
                request.reason.as_deref(),
            );
            if let Some(additional_permissions) = &request.additional_permissions
                && let Some(rule_line) = format_additional_permissions_rule(additional_permissions)
            {
                header.push(Line::from(vec![
                    "Permission rule: ".into(),
                    rule_line.fg(crate::style::accent_color()),
                ]));
                header.push(Line::from(""));
            }
            if request.kind == CommandExecutionApprovalKind::WriteStdin {
                let input = request.command.last().map_or("", String::as_str);
                header.push(vec!["Input: ".into(), format!("{input:?}").into()].into());
            } else {
                let full_cmd = strip_bash_lc_and_escape(&request.command);
                let mut full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                if let Some(first) = full_cmd_lines.first_mut() {
                    first.spans.insert(0, Span::from("$ "));
                }
                if request.network_approval_context.is_none() {
                    header.extend(full_cmd_lines);
                }
            }
            Box::new(crate::terminal_hyperlinks::HyperlinkText::new(header))
        }
        ApprovalRequest::Permissions(request) => {
            let mut header = request_context_lines(
                request.thread_label.as_deref(),
                request.environment_id.as_deref(),
                request.reason.as_deref(),
            );
            if let Some(rule_line) = format_requested_permissions_rule(&request.permissions) {
                header.push(Line::from(vec![
                    "Permission rule: ".into(),
                    rule_line.fg(crate::style::accent_color()),
                ]));
            }
            Box::new(crate::terminal_hyperlinks::HyperlinkText::new(header))
        }
        ApprovalRequest::ApplyPatch(request) => super::apply_patch_header::build_header(request),
        ApprovalRequest::McpElicitation(request) => {
            let mut lines = thread_label_lines(request.thread_label.as_deref());
            lines.extend([
                Line::from(vec!["Server: ".into(), request.server_name.clone().bold()]),
                Line::from(""),
                Line::from(request.message.clone()),
            ]);
            Box::new(crate::terminal_hyperlinks::HyperlinkText::new(lines))
        }
    }
}

#[derive(Clone)]
enum ApprovalDecision {
    Command(CommandExecutionApprovalDecision),
    FileChange(FileChangeApprovalDecision),
    Permissions(PermissionsDecision),
    McpElicitation(McpServerElicitationAction),
}

#[derive(Clone, Copy)]
enum PermissionsDecision {
    GrantForTurn,
    GrantForTurnWithStrictAutoReview,
    GrantForSession,
    Deny,
}

#[derive(Clone)]
struct ApprovalOption {
    label: String,
    decision: ApprovalDecision,
    shortcuts: Vec<KeyBinding>,
}

fn command_decision_to_review_decision(
    decision: &CommandExecutionApprovalDecision,
) -> ReviewDecision {
    match decision {
        CommandExecutionApprovalDecision::Accept => ReviewDecision::Approved,
        CommandExecutionApprovalDecision::AcceptForSession => ReviewDecision::ApprovedForSession,
        CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
            execpolicy_amendment,
        } => ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment: execpolicy_amendment.clone().into_core(),
        },
        CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
            network_policy_amendment,
        } => ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment: network_policy_amendment.clone().into_core(),
        },
        CommandExecutionApprovalDecision::Decline => ReviewDecision::Denied,
        CommandExecutionApprovalDecision::Cancel => ReviewDecision::Abort,
    }
}

fn exec_options(
    available_decisions: &[CommandExecutionApprovalDecision],
    network_approval_context: Option<&NetworkApprovalContext>,
    additional_permissions: Option<&AdditionalPermissionProfile>,
    keymap: &ApprovalKeymap,
) -> Vec<ApprovalOption> {
    available_decisions
        .iter()
        .filter_map(|decision| match decision {
            CommandExecutionApprovalDecision::Accept => Some(ApprovalOption {
                label: if network_approval_context.is_some() {
                    "Yes, just this once".to_string()
                } else {
                    "Yes, proceed".to_string()
                },
                decision: ApprovalDecision::Command(CommandExecutionApprovalDecision::Accept),
                shortcuts: keymap.approve.clone(),
            }),
            CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                execpolicy_amendment,
            } => {
                let rendered_prefix = strip_bash_lc_and_escape(&execpolicy_amendment.command);
                if rendered_prefix.contains('\n') || rendered_prefix.contains('\r') {
                    return None;
                }

                Some(ApprovalOption {
                    label: format!(
                        "Yes, and don't ask again for commands that start with `{rendered_prefix}`"
                    ),
                    decision: ApprovalDecision::Command(
                        CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                            execpolicy_amendment: execpolicy_amendment.clone(),
                        },
                    ),
                    shortcuts: keymap.approve_for_prefix.clone(),
                })
            }
            CommandExecutionApprovalDecision::AcceptForSession => Some(ApprovalOption {
                label: if network_approval_context.is_some() {
                    "Yes, and allow this host for this conversation".to_string()
                } else if additional_permissions.is_some() {
                    "Yes, and allow these permissions for this session".to_string()
                } else {
                    "Yes, and don't ask again for this command in this session".to_string()
                },
                decision: ApprovalDecision::Command(
                    CommandExecutionApprovalDecision::AcceptForSession,
                ),
                shortcuts: keymap.approve_for_session.clone(),
            }),
            CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                network_policy_amendment,
            } => {
                let (label, shortcuts) = match network_policy_amendment.action {
                    NetworkPolicyRuleAction::Allow => (
                        "Yes, and allow this host in the future".to_string(),
                        keymap.approve_for_prefix.clone(),
                    ),
                    NetworkPolicyRuleAction::Deny => (
                        "No, and block this host in the future".to_string(),
                        keymap.deny.clone(),
                    ),
                };
                Some(ApprovalOption {
                    label,
                    decision: ApprovalDecision::Command(
                        CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                            network_policy_amendment: network_policy_amendment.clone(),
                        },
                    ),
                    shortcuts,
                })
            }
            CommandExecutionApprovalDecision::Decline => Some(ApprovalOption {
                label: "No, continue without running it".to_string(),
                decision: ApprovalDecision::Command(CommandExecutionApprovalDecision::Decline),
                shortcuts: keymap.deny.clone(),
            }),
            CommandExecutionApprovalDecision::Cancel => Some(ApprovalOption {
                label: "No, and tell Codex what to do differently".to_string(),
                decision: ApprovalDecision::Command(CommandExecutionApprovalDecision::Cancel),
                shortcuts: keymap.decline.clone(),
            }),
        })
        .collect()
}

pub(crate) fn format_additional_permissions_rule(
    additional_permissions: &AdditionalPermissionProfile,
) -> Option<String> {
    let mut parts = Vec::new();
    if additional_permissions
        .network
        .as_ref()
        .and_then(|network| network.enabled)
        .unwrap_or(false)
    {
        parts.push("network".to_string());
    }
    if let Some(file_system) = additional_permissions.file_system.as_ref() {
        for (access, verb) in [
            (FileSystemAccessMode::Read, "read"),
            (FileSystemAccessMode::Write, "write"),
            (FileSystemAccessMode::Deny, "deny read"),
        ] {
            let paths = format_file_system_entry_paths(
                file_system
                    .entries
                    .iter()
                    .flatten()
                    .filter(|entry| entry.access == access),
            );
            if !paths.is_empty() {
                parts.push(format!("{verb} {paths}"));
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

pub(crate) fn format_requested_permissions_rule(
    permissions: &RequestPermissionProfile,
) -> Option<String> {
    let permissions =
        crate::app_server_approval_conversions::granted_permission_profile_from_request(
            permissions.clone(),
        );
    format_additional_permissions_rule(&AdditionalPermissionProfile {
        network: permissions.network,
        file_system: permissions.file_system,
    })
}

fn format_file_system_entry_paths<'a>(
    entries: impl Iterator<Item = &'a FileSystemSandboxEntry>,
) -> String {
    entries
        .map(|entry| match &entry.path {
            FileSystemPath::Path { path } => format!("`{path}`"),
            FileSystemPath::GlobPattern { pattern } => format!("glob `{pattern}`"),
            FileSystemPath::Special { value } => format!("`{}`", special_path_label(value)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn special_path_label(value: &FileSystemSpecialPath) -> String {
    match value {
        FileSystemSpecialPath::Root => ":root".to_string(),
        FileSystemSpecialPath::Minimal => ":minimal".to_string(),
        FileSystemSpecialPath::ProjectRoots { subpath } => path_label(":workspace_roots", subpath),
        FileSystemSpecialPath::Tmpdir => ":tmpdir".to_string(),
        FileSystemSpecialPath::SlashTmp => "/tmp".to_string(),
        FileSystemSpecialPath::Unknown { path, subpath } => path_label(path, subpath),
    }
}

fn path_label(base: &str, subpath: &Option<LegacyAppPathString>) -> String {
    match subpath {
        Some(subpath) => format!("{base}/{subpath}"),
        None => base.to_string(),
    }
}

fn patch_options(keymap: &ApprovalKeymap) -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            label: "Yes, proceed".to_string(),
            decision: ApprovalDecision::FileChange(FileChangeApprovalDecision::Accept),
            shortcuts: keymap.approve.clone(),
        },
        ApprovalOption {
            label: "Yes, and don't ask again for these files".to_string(),
            decision: ApprovalDecision::FileChange(FileChangeApprovalDecision::AcceptForSession),
            shortcuts: keymap.approve_for_session.clone(),
        },
        ApprovalOption {
            label: "No, and tell Codex what to do differently".to_string(),
            decision: ApprovalDecision::FileChange(FileChangeApprovalDecision::Cancel),
            shortcuts: keymap.decline.clone(),
        },
    ]
}

fn permissions_options(keymap: &ApprovalKeymap) -> Vec<ApprovalOption> {
    let deny_shortcuts = keymap
        .deny
        .iter()
        .copied()
        .filter(|shortcut| shortcut.parts() != (KeyCode::Esc, KeyModifiers::NONE))
        .collect();

    vec![
        ApprovalOption {
            label: "Yes, grant these permissions for this turn".to_string(),
            decision: ApprovalDecision::Permissions(PermissionsDecision::GrantForTurn),
            shortcuts: keymap.approve.clone(),
        },
        ApprovalOption {
            label: "Yes, grant for this turn with strict auto review".to_string(),
            decision: ApprovalDecision::Permissions(
                PermissionsDecision::GrantForTurnWithStrictAutoReview,
            ),
            shortcuts: vec![key_hint::plain(KeyCode::Char('r'))],
        },
        ApprovalOption {
            label: "Yes, grant these permissions for this session".to_string(),
            decision: ApprovalDecision::Permissions(PermissionsDecision::GrantForSession),
            shortcuts: keymap.approve_for_session.clone(),
        },
        ApprovalOption {
            label: "No, continue without permissions".to_string(),
            decision: ApprovalDecision::Permissions(PermissionsDecision::Deny),
            shortcuts: deny_shortcuts,
        },
    ]
}

/// Build MCP elicitation options with stable cancellation semantics.
///
/// `Esc` is always treated as cancel for elicitation prompts, even if users
/// customize `decline`/`cancel` bindings. We keep this as a hard contract so
/// dismissal remains a safe abort path and never silently maps to "continue
/// without requested info." Any decline/cancel overlap is removed from the
/// decline option in elicitation mode to preserve this invariant.
fn elicitation_options(keymap: &ApprovalKeymap) -> Vec<ApprovalOption> {
    let mut cancel_shortcuts = vec![key_hint::plain(KeyCode::Esc)];
    for shortcut in &keymap.cancel {
        if !cancel_shortcuts.contains(shortcut) {
            cancel_shortcuts.push(*shortcut);
        }
    }

    let decline_shortcuts: Vec<KeyBinding> = keymap
        .decline
        .iter()
        .copied()
        .filter(|shortcut| !cancel_shortcuts.contains(shortcut))
        .collect();

    vec![
        ApprovalOption {
            label: "Yes, provide the requested info".to_string(),
            decision: ApprovalDecision::McpElicitation(McpServerElicitationAction::Accept),
            shortcuts: keymap.approve.clone(),
        },
        ApprovalOption {
            label: "No, but continue without it".to_string(),
            decision: ApprovalDecision::McpElicitation(McpServerElicitationAction::Decline),
            shortcuts: decline_shortcuts,
        },
        ApprovalOption {
            label: "Cancel this request".to_string(),
            decision: ApprovalDecision::McpElicitation(McpServerElicitationAction::Cancel),
            shortcuts: cancel_shortcuts,
        },
    ]
}

#[cfg(test)]
#[path = "approval_overlay_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "approval_overlay/clipping_tests.rs"]
mod clipping_tests;
