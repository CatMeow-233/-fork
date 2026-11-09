use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::app_server_session::AppServerSession;
use crate::app_server_session::unread_revision;
use crate::backend_id::BackendId;
use crate::clipboard_paste::normalize_pasted_search_query;
use crate::clock_format::ClockFormat;
use crate::color::blend;
use crate::color::is_light;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::is_plain_text_key_event;
use crate::keymap::ListAction;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::markdown_render::render_streaming_markdown_lines_with_width_and_cwd as render_assistant;
use crate::pager_overlay::Overlay;
use crate::status::format_directory_display;
use crate::style::footer_hint_label_style;
use crate::style::readable_color_on;
use crate::style::secondary_text_style;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::text_formatting::truncate_text;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::TranscriptCells;
use crate::thread_transcript::load_session_transcript;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use chrono::DateTime;
use chrono::Utc;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadListCwdFilter;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use codex_config::types::SessionPickerViewMode;
use codex_protocol::ThreadId;
use codex_utils_path as path_utils;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Styled as _;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

mod app_server_loader;
mod archive;
mod catalog;
mod filtering;
mod key_dispatch;
mod launch;
mod layout;
mod page_loading;
mod paging;
mod render_chrome;
mod render_details;
mod render_rows;
mod sources;
mod transcript_views;
mod viewport;

#[cfg(test)]
#[path = "resume_picker_color_tests.rs"]
mod color_tests;

#[cfg(test)]
#[path = "resume_picker_pagination_error_tests.rs"]
mod pagination_error_tests;

use page_loading::PageCwdFilter;
use page_loading::PageLoadMode;
use page_loading::PaginationState;

#[cfg(test)]
use app_server_loader::parse_timestamp_str;
#[cfg(test)]
use app_server_loader::row_from_app_server_thread;
use app_server_loader::spawn_app_server_page_loader;
use app_server_loader::spawn_app_server_page_loaders;
#[cfg(test)]
use app_server_loader::thread_list_params;
use filtering::local_repo_filter_available;
use filtering::paths_match;
pub(crate) use filtering::repository_cwd_filter;
#[cfg(test)]
use launch::local_picker_cwd_filter;
#[cfg(test)]
use launch::picker_cwd_filter;
pub use launch::run_fork_picker_with_app_server;
pub(crate) use launch::run_resume_picker_from_existing_session_with_app_servers;
pub use launch::run_resume_picker_with_app_server;
#[cfg(test)]
use render_chrome::footer_hint_lines;
use render_chrome::list_viewport_width;
#[cfg(test)]
use render_chrome::picker_footer_percent;
#[cfg(test)]
use render_chrome::picker_footer_progress_label;
use render_chrome::picker_footer_scroll_percent;
use render_chrome::render_picker_footer;
use render_chrome::render_transcript_loading_overlay;
use render_chrome::search_line;
use render_chrome::toolbar_for_width;
#[cfg(test)]
use render_chrome::toolbar_line;
#[cfg(test)]
use render_chrome::transcript_loading_overlay_style;
pub(crate) use render_details::format_relative_time;
#[cfg(test)]
use render_details::format_relative_time_long;
#[cfg(test)]
use render_details::render_expanded_session_details;
use render_details::render_transcript_preview_lines;
#[cfg(test)]
use render_rows::dense_summary_line;
#[cfg(test)]
use render_rows::dense_zebra_style;
#[cfg(test)]
use render_rows::render_comfortable_session_lines;
#[cfg(test)]
use render_rows::render_dense_session_lines;
#[cfg(test)]
use render_rows::render_footer_lines;
use render_rows::render_list;
use render_rows::render_session_lines;
#[cfg(test)]
use render_rows::selected_session_style;
#[cfg(test)]
use render_rows::selection_marker;
#[cfg(test)]
use render_rows::session_summary_line;

#[path = "resume_picker_transcript_preview.rs"]
mod transcript_preview;

pub(crate) use transcript_preview::load_transcript_preview;

const PAGE_SIZE: usize = 25;
const LOAD_NEAR_THRESHOLD: usize = 5;
const SESSION_META_INDENT_WIDTH: usize = 2;
const SESSION_META_DATE_WIDTH: usize = 12;
const SESSION_META_FIELD_GAP_WIDTH: usize = 2;
const SESSION_META_MIN_CWD_WIDTH: usize = 30;
const SESSION_META_MAX_CWD_WIDTH: usize = 72;
const SESSION_META_BRANCH_ICON: &str = "";
const SESSION_META_CWD_ICON: &str = "⌁";
const FOOTER_COMPACT_BREAKPOINT: u16 = 120;
const FOOTER_HINT_LEFT_PADDING: usize = 1;
const FOOTER_HINT_GAP: usize = 3;
const PICKER_LIST_HORIZONTAL_INSET: u16 = 4;

#[derive(Debug, Clone)]
pub struct SessionTarget {
    /// Logical owner of the native thread ID, retained across connection replacement.
    pub(crate) backend_id: crate::backend_id::BackendId,
    pub path: Option<PathBuf>,
    pub thread_id: ThreadId,
    /// Working directory reported by `thread/list` or `thread/read` at selection time.
    pub cwd: Option<PathBuf>,
    /// History mode observed during selection, if the server provided one.
    pub history_mode: Option<ThreadHistoryMode>,
}

impl SessionTarget {
    pub(crate) fn ensure_backend(&self, backend_id: crate::backend_id::BackendId) -> Result<()> {
        color_eyre::eyre::ensure!(
            self.backend_id == backend_id,
            "This session belongs to a different backend. Open the session picker again."
        );
        Ok(())
    }

    pub fn display_label(&self) -> String {
        self.path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("thread {}", self.thread_id))
    }
}

#[derive(Debug, Clone)]
pub enum SessionSelection {
    StartFresh,
    AgentsOverview,
    Resume(SessionTarget),
    Fork(SessionTarget),
    Exit,
}

#[derive(Clone, Copy, Debug)]
pub enum SessionPickerAction {
    Resume,
    Fork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPickerLaunchContext {
    Startup,
    ExistingSession { current_thread_id: Option<ThreadId> },
}

impl SessionPickerAction {
    fn title(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "Resume a previous session",
            SessionPickerAction::Fork => "Fork a previous session",
        }
    }

    fn action_label(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "resume",
            SessionPickerAction::Fork => "fork",
        }
    }

    fn selection(self, target_session: SessionTarget) -> SessionSelection {
        match self {
            SessionPickerAction::Resume => SessionSelection::Resume(target_session),
            SessionPickerAction::Fork => SessionSelection::Fork(target_session),
        }
    }
}

#[derive(Clone)]
struct PageLoadRequest {
    cursor: Option<PageCursor>,
    request_token: usize,
    listing_token: usize,
    search_token: Option<usize>,
    mode: PageLoadMode,
    filter_mode: SessionFilterMode,
    cwd_filter: Option<PathBuf>,
    status: SessionStatus,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
}

enum PickerLoadRequest {
    Acknowledge {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
        revision: String,
    },
    Page(PageLoadRequest),
    Preview {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
    },
    Transcript {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
        cancellation: oneshot::Receiver<()>,
    },
    Archive {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
    },
    Unarchive {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
    },
}

#[derive(Clone)]
enum ProviderFilter {
    // Omit the filter and let the server use its default provider.
    Any,
    // Explicitly include threads from every provider.
    All,
    MatchDefault(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionFilterMode {
    Cwd,
    Repo,
    All,
}

impl SessionFilterMode {
    fn from_show_all(show_all: bool, filter_cwd: Option<&Path>) -> Self {
        if show_all || filter_cwd.is_none() {
            Self::All
        } else {
            Self::Cwd
        }
    }

    fn change(
        self,
        direction: ToolbarValueDirection,
        filter_cwd: Option<&Path>,
        repo_filter_available: bool,
    ) -> Self {
        match (self, direction) {
            (Self::Cwd, ToolbarValueDirection::Next) if repo_filter_available => Self::Repo,
            (Self::Cwd, ToolbarValueDirection::Next | ToolbarValueDirection::Previous)
            | (Self::Repo, ToolbarValueDirection::Next) => Self::All,
            (Self::Repo, ToolbarValueDirection::Previous) => Self::Cwd,
            (Self::All, ToolbarValueDirection::Previous) if repo_filter_available => Self::Repo,
            (Self::All, _) if filter_cwd.is_some() => Self::Cwd,
            (Self::All, _) => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStatus {
    Active,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarControl {
    Filter,
    Status,
    Sort,
}

enum ToolbarValueDirection {
    Previous,
    Next,
}

impl ToolbarControl {
    fn previous(self, action: SessionPickerAction) -> Self {
        match self {
            Self::Filter => Self::Sort,
            Self::Status => Self::Filter,
            Self::Sort if matches!(action, SessionPickerAction::Resume) => Self::Status,
            Self::Sort => Self::Filter,
        }
    }

    fn next(self, action: SessionPickerAction) -> Self {
        match self {
            Self::Filter if matches!(action, SessionPickerAction::Resume) => Self::Status,
            Self::Filter | Self::Status => Self::Sort,
            Self::Sort => Self::Filter,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionListDensity {
    Comfortable,
    Dense,
}

impl SessionListDensity {
    fn toggle(self) -> Self {
        match self {
            Self::Comfortable => Self::Dense,
            Self::Dense => Self::Comfortable,
        }
    }
}

impl From<SessionPickerViewMode> for SessionListDensity {
    fn from(mode: SessionPickerViewMode) -> Self {
        match mode {
            SessionPickerViewMode::Comfortable => Self::Comfortable,
            SessionPickerViewMode::Dense => Self::Dense,
        }
    }
}

impl From<SessionListDensity> for SessionPickerViewMode {
    fn from(density: SessionListDensity) -> Self {
        match density {
            SessionListDensity::Comfortable => Self::Comfortable,
            SessionListDensity::Dense => Self::Dense,
        }
    }
}

type PickerLoader = Arc<dyn Fn(PickerLoadRequest) + Send + Sync>;

struct PickerBackground {
    loader: PickerLoader,
    task: tokio::task::JoinHandle<()>,
}

enum BackgroundEvent {
    Acknowledged {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
        revision: Option<String>,
    },
    PartialPage {
        listing_token: usize,
        page: PickerPage,
    },
    SourceError {
        listing_token: usize,
        next_cursor: Option<PageCursor>,
    },
    Page {
        request_token: usize,
        search_token: Option<usize>,
        mode: PageLoadMode,
        page: std::io::Result<PickerPage>,
    },
    Preview {
        thread_id: ThreadId,
        preview: std::io::Result<Vec<TranscriptPreviewLine>>,
    },
    Transcript {
        backend_id: crate::backend_id::BackendId,
        thread_id: ThreadId,
        transcript: std::io::Result<LoadedTranscript>,
    },
    Archive {
        thread_id: ThreadId,
        result: std::io::Result<()>,
    },
    Unarchive {
        thread_id: ThreadId,
        result: std::io::Result<SessionTarget>,
    },
}

#[derive(Clone)]
enum PageCursor {
    AppServer(String),
    Catalog,
    SourcesPending,
}

pub(crate) struct ResumePickerAppServers {
    ordered: Vec<AppServerSession>,
    displayed_owner: crate::backend_id::BackendId,
}

impl ResumePickerAppServers {
    pub(crate) fn new(
        ordered: Vec<AppServerSession>,
        displayed_owner: crate::backend_id::BackendId,
    ) -> Self {
        assert!(
            ordered
                .iter()
                .any(|session| session.backend_id() == displayed_owner)
        );
        Self {
            ordered,
            displayed_owner,
        }
    }

    fn singleton(session: AppServerSession) -> Self {
        let displayed_owner = session.backend_id();
        Self::new(vec![session], displayed_owner)
    }
}

struct AppServerPickerBackend {
    uses_remote_filesystem: bool,
    session: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<Config>,
}

struct PickerPage {
    rows: Vec<Row>,
    read_states: HashMap<String, codex_app_server_protocol::ThreadReadState>,
    history_modes: HashMap<ThreadId, ThreadHistoryMode>,
    next_cursor: Option<PageCursor>,
    num_scanned_files: usize,
    reached_scan_cap: bool,
}

#[derive(Clone)]
struct SessionPickerViewPersistence {
    codex_home: PathBuf,
}

struct SessionPickerRunOptions {
    backend_id: crate::backend_id::BackendId,
    use_theme_colors: bool,
    copy_on_select: bool,
    copy_on_select_clear_selection: bool,
    mouse_scroll_speed: f64,
    show_all: bool,
    filter_cwd: Option<PathBuf>,
    local_filter_cwd: Option<PathBuf>,
    action: SessionPickerAction,
    launch_context: SessionPickerLaunchContext,
    provider_filter: ProviderFilter,
    initial_density: SessionListDensity,
    view_persistence: Option<SessionPickerViewPersistence>,
    keymap: RuntimeKeymap,
    initial_page_mode: PageLoadMode,
}

/// RAII guard that ensures we leave the alt-screen on scope exit.
struct AltScreenGuard<'a> {
    tui: &'a mut Tui,
}

impl<'a> AltScreenGuard<'a> {
    fn enter(tui: &'a mut Tui) -> Self {
        let _ = tui.enter_alt_screen();
        Self { tui }
    }
}

impl Drop for AltScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tui.leave_alt_screen();
    }
}

struct PickerState {
    backend_id: crate::backend_id::BackendId,
    clock_format: ClockFormat,
    use_theme_colors: bool,
    copy_on_select: bool,
    copy_on_select_clear_selection: bool,
    mouse_scroll_speed: f64,
    // Resolve local filesystem membership once per cwd for each page-loading cycle.
    local_cwd_matches: HashMap<PathBuf, bool>,
    requester: FrameRequester,
    relative_time_reference: Option<DateTime<Utc>>,
    pagination: PaginationState,
    all_rows: Vec<Row>,
    filtered_rows: Vec<Row>,
    seen_rows: HashSet<SeenRowKey>,
    selected: usize,
    scroll_top: usize,
    pending_page_down_target: Option<usize>,
    frozen_footer_percent: Option<u8>,
    query: String,
    search_state: SearchState,
    listing_token: usize,
    next_request_token: usize,
    next_search_token: usize,
    picker_loader: PickerLoader,
    view_rows: Option<usize>,
    view_width: Option<u16>,
    provider_filter: ProviderFilter,
    filter_mode: SessionFilterMode,
    status: SessionStatus,
    filter_cwd: Option<PathBuf>,
    local_filter_cwd: Option<PathBuf>,
    repo_filter_available: bool,
    toolbar_focus: ToolbarControl,
    density: SessionListDensity,
    launch_context: SessionPickerLaunchContext,
    view_persistence: Option<SessionPickerViewPersistence>,
    action: SessionPickerAction,
    sort_key: ThreadSortKey,
    inline_error: Option<String>,
    archive_state: archive::ArchiveState,
    expanded_thread_id: Option<ThreadId>,
    transcript_previews: HashMap<ThreadId, TranscriptPreviewState>,
    transcript_cells: HashMap<(BackendId, ThreadId), SessionTranscriptState>,
    pending_transcript_open: Option<(BackendId, ThreadId)>,
    unread: HashMap<(BackendId, ThreadId), String>,
    overlay_receipt: Option<PickerLoadRequest>,
    pending_transcript_cancellation: Option<oneshot::Sender<()>>,
    transcript_loading_frame_shown: bool,
    overlay: Option<Overlay>,
    keymap: RuntimeKeymap,
    initial_page_mode: PageLoadMode,
    chord_matcher: crate::keymap::KeyChordMatcher,
}

#[derive(Clone, Copy, Debug)]
enum SearchState {
    Idle,
    Active { token: usize },
}

#[derive(Clone)]
enum TranscriptPreviewState {
    Loading,
    Loaded(Vec<TranscriptPreviewLine>),
    Failed,
}

struct LoadedTranscript {
    cells: TranscriptCells,
    unread_revision: Option<String>,
}

enum SessionTranscriptState {
    Loading,
    Loaded(LoadedTranscript),
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptPreviewLine {
    speaker: TranscriptPreviewSpeaker,
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TranscriptPreviewSpeaker {
    User,
    Assistant,
}

enum LoadTrigger {
    Scroll,
    Search { token: usize },
}

impl SearchState {
    fn active_token(&self) -> Option<usize> {
        match self {
            SearchState::Idle => None,
            SearchState::Active { token } => Some(*token),
        }
    }

    fn is_active(&self) -> bool {
        self.active_token().is_some()
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
struct Row {
    backend_id: Option<crate::backend_id::BackendId>,
    history_mode: Option<ThreadHistoryMode>,
    path: Option<PathBuf>,
    preview: String,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SeenRowKey {
    Path(PathBuf),
    Thread(ThreadId),
}

impl Row {
    fn seen_key(&self) -> Option<SeenRowKey> {
        if let Some(path) = self.path.clone() {
            return Some(SeenRowKey::Path(path));
        }
        self.thread_id.map(SeenRowKey::Thread)
    }

    fn display_preview(&self) -> &str {
        self.thread_name.as_deref().unwrap_or(&self.preview)
    }

    fn matches_query(&self, query: &str) -> bool {
        if self.preview.to_lowercase().contains(query) {
            return true;
        }
        if let Some(thread_name) = self.thread_name.as_ref()
            && thread_name.to_lowercase().contains(query)
        {
            return true;
        }
        if self
            .thread_id
            .is_some_and(|thread_id| thread_id.to_string().to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .git_branch
            .as_ref()
            .is_some_and(|branch| branch.to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.to_string_lossy().to_lowercase().contains(query))
        {
            return true;
        }
        false
    }
}

impl PickerState {
    fn new(
        backend_id: crate::backend_id::BackendId,
        requester: FrameRequester,
        picker_loader: PickerLoader,
        provider_filter: ProviderFilter,
        show_all: bool,
        filter_cwd: Option<PathBuf>,
        action: SessionPickerAction,
    ) -> Self {
        Self {
            backend_id,
            clock_format: ClockFormat::system(),
            use_theme_colors: true,
            copy_on_select: false,
            copy_on_select_clear_selection: false,
            mouse_scroll_speed: 1.0,
            requester,
            relative_time_reference: None,
            pagination: PaginationState::new(),
            all_rows: Vec::new(),
            filtered_rows: Vec::new(),
            seen_rows: HashSet::new(),
            selected: 0,
            scroll_top: 0,
            pending_page_down_target: None,
            frozen_footer_percent: None,
            query: String::new(),
            search_state: SearchState::Idle,
            listing_token: 0,
            next_request_token: 0,
            next_search_token: 0,
            picker_loader,
            view_rows: None,
            view_width: None,
            provider_filter,
            filter_mode: SessionFilterMode::from_show_all(show_all, filter_cwd.as_deref()),
            status: SessionStatus::Active,
            local_filter_cwd: filter_cwd.clone(),
            local_cwd_matches: HashMap::new(),
            filter_cwd,
            repo_filter_available: false,
            toolbar_focus: ToolbarControl::Filter,
            density: SessionListDensity::Comfortable,
            launch_context: SessionPickerLaunchContext::Startup,
            view_persistence: None,
            action,
            sort_key: ThreadSortKey::UpdatedAt,
            inline_error: None,
            archive_state: archive::ArchiveState::default(),
            expanded_thread_id: None,
            transcript_previews: HashMap::new(),
            transcript_cells: HashMap::new(),
            pending_transcript_open: None,
            unread: HashMap::new(),
            overlay_receipt: None,
            pending_transcript_cancellation: None,
            transcript_loading_frame_shown: false,
            overlay: None,
            keymap: RuntimeKeymap::defaults(),
            initial_page_mode: PageLoadMode::StoreDefault,
            chord_matcher: crate::keymap::KeyChordMatcher::default(),
        }
    }

    fn request_frame(&self) {
        self.requester.schedule_frame();
    }

    fn set_repo_filter_available(&mut self, available: bool) {
        self.repo_filter_available = available;
        self.request_frame();
    }
}

struct PickerFooterHint {
    key: String,
    wide_label: String,
    compact_label: String,
    priority: u8,
}

#[derive(Clone, Copy)]
enum FooterHintLabelMode {
    Wide,
    Compact,
    KeyOnly,
}

struct DenseSummaryInput<'a> {
    unread: bool,
    thread_id: Option<ThreadId>,
    use_theme_colors: bool,
    marker: Span<'static>,
    date: &'a str,
    title: &'a str,
    is_selected: bool,
    is_zebra: bool,
    width: u16,
}

struct DenseColumns {
    date_width: usize,
    title_width: usize,
}

enum FooterPart {
    Date(String),
    Branch(Option<String>),
    Cwd(Option<String>),
}

impl FooterPart {
    fn text(&self) -> &str {
        match self {
            FooterPart::Date(text) => text,
            FooterPart::Branch(Some(text)) | FooterPart::Cwd(Some(text)) => text,
            FooterPart::Branch(None) => "no branch",
            FooterPart::Cwd(None) => "no cwd",
        }
    }

    fn prefix(&self) -> Option<&'static str> {
        match self {
            FooterPart::Date(_) => None,
            FooterPart::Branch(_) => Some(SESSION_META_BRANCH_ICON),
            FooterPart::Cwd(_) => Some(SESSION_META_CWD_ICON),
        }
    }
}

#[cfg(test)]
#[path = "resume_picker_tests.rs"]
mod tests;
