//! Render selectable lists with shared picker controls and full-width focus rows.
//! Selection and scrolling use filtered row indices; actions map back to source items.
//! Controls and overflow indicators keep their own rows outside the result viewport.
//! Letter-labeled children are indented and do not consume numeric shortcuts.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use itertools::Itertools as _;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use super::selection_picker_layout::PickerLayoutSizes;
use super::selection_picker_layout::picker_areas;

use super::selection_popup_common::wrap_styled_line;
use crate::app_event_sender::AppEventSender;
use crate::clipboard_paste::normalize_pasted_search_query;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::ShortcutHint;
use crate::key_hint::is_plain_text_key_event;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::wrapping::word_wrap_lines;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::bottom_pane_view::ViewCompletion;
pub(crate) use super::picker_style::PickerSurface;
use super::picker_style::render_scroll_indicators;
use super::picker_style::selection_style;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::ColumnWidthConfig;
pub(crate) use super::selection_popup_common::ColumnWidthMode;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height_with_col_width_mode;
use super::selection_popup_common::render_rows_single_line_with_col_width_mode;
use super::selection_popup_common::render_rows_with_col_width_mode;
use super::selection_popup_common::should_show_selected_description_below;
pub(crate) use super::selection_row_layout::SelectionDescriptionLayout;
use super::selection_tabs::SelectionTab;
use super::selection_tabs::render_tab_bar;
use super::selection_tabs::tab_bar_height;
use std::cell::Cell;
use unicode_width::UnicodeWidthStr;

/// Minimum list width (in content columns) required before the side-by-side
/// layout is activated. Keeps the list usable even when sharing horizontal
/// space with the side content panel.
const MIN_LIST_WIDTH_FOR_SIDE: u16 = 40;

/// Horizontal gap (in columns) between the list area and the side content
/// panel when side-by-side layout is active.
const SIDE_CONTENT_GAP: u16 = 2;

/// Shared menu-surface horizontal inset (2 cells per side) used by selection popups.
const MENU_SURFACE_HORIZONTAL_INSET: u16 = 4;

/// Controls how the side content panel is sized relative to the popup width.
///
/// When the computed side width falls below `side_content_min_width` or the
/// remaining list area would be narrower than [`MIN_LIST_WIDTH_FOR_SIDE`], the
/// side-by-side layout is abandoned and the stacked fallback is used instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SideContentWidth {
    /// Fixed number of columns.  `Fixed(0)` disables side content entirely.
    Fixed(u16),
    /// Exact 50/50 split of the content area (minus the inter-column gap).
    Half,
}

impl Default for SideContentWidth {
    fn default() -> Self {
        Self::Fixed(0)
    }
}

/// Returns the popup content width after subtracting the shared menu-surface
/// horizontal inset (2 columns on each side).
pub(crate) fn popup_content_width(total_width: u16) -> u16 {
    total_width.saturating_sub(MENU_SURFACE_HORIZONTAL_INSET)
}

/// Returns side-by-side layout widths as `(list_width, side_width)` when the
/// layout can fit. Returns `None` when the side panel is disabled/too narrow or
/// when the remaining list width would become unusably small.
pub(crate) fn side_by_side_layout_widths(
    content_width: u16,
    side_content_width: SideContentWidth,
    side_content_min_width: u16,
) -> Option<(u16, u16)> {
    let side_width = match side_content_width {
        SideContentWidth::Fixed(0) => return None,
        SideContentWidth::Fixed(width) => width,
        SideContentWidth::Half => content_width.saturating_sub(SIDE_CONTENT_GAP) / 2,
    };
    if side_width < side_content_min_width {
        return None;
    }
    let list_width = content_width.saturating_sub(SIDE_CONTENT_GAP + side_width);
    (list_width >= MIN_LIST_WIDTH_FOR_SIDE).then_some((list_width, side_width))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SelectionRowDisplay {
    #[default]
    Wrapped,
    SingleLine,
}

/// One selectable item in the generic selection list.
pub(crate) type SelectionAction = Box<dyn Fn(&AppEventSender) + Send + Sync>;

/// A second way to accept the highlighted row, sharing its dismissal behavior.
pub(crate) struct SelectionSecondaryAction {
    pub key: KeyBinding,
    pub action: SelectionAction,
    pub footer_hint: Line<'static>,
}

enum SelectionActionKind {
    Primary,
    Secondary,
}
pub(crate) type SelectionToggleAction = dyn Fn(bool, &AppEventSender) + Send + Sync;

pub(crate) struct SelectionToggle {
    pub is_on: bool,
    pub action: Box<SelectionToggleAction>,
}

/// A displayed toggle value with the number of its save actions still in flight.
#[derive(Clone, Copy)]
pub(crate) struct SelectionToggleState {
    pub is_on: bool,
    pub pending_actions: usize,
}

/// Callback invoked whenever the highlighted item changes (arrow keys, search
/// filter, number-key jump).  Receives the *actual* index into the unfiltered
/// `items` list and the event sender.  Used by the theme picker for live preview.
pub(crate) type OnSelectionChangedCallback =
    Option<Box<dyn Fn(usize, &AppEventSender) + Send + Sync>>;

/// Callback invoked when the picker is dismissed without accepting (Esc or
/// Ctrl+C).  Used by the theme picker to restore the pre-open theme.
pub(crate) type OnCancelCallback = Option<Box<dyn Fn(&AppEventSender) + Send + Sync>>;

/// One row in a [`ListSelectionView`] selection list.
///
/// This is the source-of-truth model for row state before filtering and
/// formatting into render rows. A row is treated as disabled when either
/// `is_disabled` is true or `disabled_reason` is present; disabled rows cannot
/// be accepted and are skipped by keyboard navigation.
#[derive(Default)]
pub(crate) struct SelectionItem {
    pub name: String,
    /// Indent beneath the preceding parent, using this letter instead of a numeric shortcut.
    pub child_label: Option<char>,
    pub name_prefix_spans: Vec<Span<'static>>,
    pub toggle: Option<SelectionToggle>,
    /// Render `[x]` and treat the primary accept key as a toggle action.
    pub checkbox_toggle: bool,
    /// Toggle actions sent from this row that have not finished saving yet.
    pub pending_toggle_actions: usize,
    pub toggle_placeholder: Option<&'static str>,
    pub display_shortcut: Option<ShortcutHint>,
    /// Required row metadata retained when the secondary description hides.
    pub category_tag: Option<String>,
    pub description: Option<String>,
    pub selected_description: Option<String>,
    pub is_current: bool,
    pub is_disabled: bool,
    pub actions: Vec<SelectionAction>,
    pub secondary_action: Option<SelectionSecondaryAction>,
    /// Footer hint shown while this row is selected.
    pub footer_hint: Option<Line<'static>>,
    /// Footer hint shown instead while on-demand search is active.
    pub search_footer_hint: Option<Line<'static>>,
    pub dismiss_on_select: bool,
    /// Require an explicit accept key after a direct shortcut highlights this sensitive item.
    pub require_explicit_confirmation: bool,
    pub dismiss_parent_on_child_accept: bool,
    pub search_value: Option<String>,
    pub disabled_reason: Option<String>,
    /// Optional marker rendered in place of the number for a disabled row.
    pub disabled_gutter_marker: Option<&'static str>,
}

/// Construction-time configuration for [`ListSelectionView`].
///
/// This config is consumed once by [`ListSelectionView::new`]. After
/// construction, mutable interaction state (filtering, scrolling, and selected
/// row) lives on the view itself.
///
/// `col_width_mode` controls column width mode in selection lists:
/// `AutoVisible` (default) measures only rows visible in the viewport
/// `AutoAllRows` measures all rows to ensure stable column widths as the user scrolls
/// `row_display` controls whether rows can wrap or stay single-line with ellipsis truncation
/// `description_layout` optionally hides descriptions when their column would become too narrow.
pub(crate) struct SelectionViewParams {
    pub presentation: super::ViewPresentation,
    pub picker_surface: PickerSurface,
    /// Upper row budget; compact completion menus retain the default of eight.
    pub max_visible_rows: usize,
    /// Keep the result area at its configured row budget when filtering shrinks it.
    pub reserve_result_rows: bool,
    /// Whether this picker can continue handling input without the app server.
    pub allow_input_when_disconnected: bool,
    pub view_id: Option<&'static str>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub footer_note: Option<Line<'static>>,
    pub footer_hint: Option<Line<'static>>,
    pub tab_footer_hints: Vec<(String, Line<'static>)>,
    pub items: Vec<SelectionItem>,
    /// Optional flat item list used while a tabbed picker has a non-empty search query.
    pub search_items: Vec<SelectionItem>,
    pub tabs: Vec<SelectionTab>,
    pub initial_tab_id: Option<String>,
    pub is_searchable: bool,
    /// Optional non-text key that can toggle a row while printable keys edit the search query.
    pub search_toggle_key: Option<KeyBinding>,
    pub search_mode: SearchMode,
    /// Use ordinal digits in the gutter; when false, use only each row's explicit shortcut.
    pub auto_number_shortcuts: bool,
    pub search_placeholder: Option<String>,
    pub col_width_mode: ColumnWidthMode,
    pub row_display: SelectionRowDisplay,
    pub description_layout: SelectionDescriptionLayout,
    /// Rendered left-column width to use for auto-sized rows.
    pub name_column_width: Option<usize>,
    pub header: Box<dyn Renderable>,
    /// Optional shortcut to the complete header, shown when its text is clipped.
    pub header_view_all_hint: Option<ShortcutHint>,
    /// Blank rows after the header; defaults to one. Inline banners use zero.
    pub header_gap: u16,
    pub initial_selected_idx: Option<usize>,
    /// Restore a filter together with its actual selected item after a source refresh.
    pub initial_search_query: Option<String>,

    /// Rich content rendered beside (wide terminals) or below (narrow terminals)
    /// the list items, inside the bordered menu surface. Used by the theme picker
    /// to show a syntax-highlighted preview.
    pub side_content: Box<dyn Renderable>,

    /// Width mode for side content when side-by-side layout is active.
    pub side_content_width: SideContentWidth,

    /// Minimum side panel width required before side-by-side layout activates.
    pub side_content_min_width: u16,

    /// Optional fallback content rendered when side-by-side does not fit.
    /// When absent, `side_content` is reused.
    pub stacked_side_content: Option<Box<dyn Renderable>>,

    /// Keep side-content background colors after rendering in side-by-side mode.
    /// Disabled by default so existing popups preserve their reset-background look.
    pub preserve_side_content_bg: bool,

    /// Called when the highlighted item changes (navigation, filter, number-key).
    /// Receives the *actual* item index, not the filtered/visible index.
    pub on_selection_changed: OnSelectionChangedCallback,

    /// Whether cancellation keys can dismiss the picker.
    pub allow_cancel: bool,

    /// Called when the picker is dismissed via Esc/Ctrl+C without selecting.
    pub on_cancel: OnCancelCallback,
}

impl Default for SelectionViewParams {
    fn default() -> Self {
        Self {
            picker_surface: PickerSurface::default(),
            max_visible_rows: MAX_POPUP_ROWS,
            reserve_result_rows: false,
            allow_input_when_disconnected: false,
            presentation: super::ViewPresentation::Inline,
            view_id: None,
            title: None,
            subtitle: None,
            footer_note: None,
            footer_hint: None,
            tab_footer_hints: Vec::new(),
            items: Vec::new(),
            search_items: Vec::new(),
            tabs: Vec::new(),
            initial_tab_id: None,
            is_searchable: false,
            search_toggle_key: None,
            search_mode: SearchMode::default(),
            auto_number_shortcuts: true,
            search_placeholder: None,
            col_width_mode: ColumnWidthMode::AutoVisible,
            row_display: SelectionRowDisplay::Wrapped,
            description_layout: SelectionDescriptionLayout::Columns,
            name_column_width: None,
            header: Box::new(()),
            header_view_all_hint: None,
            header_gap: 1,
            initial_selected_idx: None,
            initial_search_query: None,
            side_content: Box::new(()),
            side_content_width: SideContentWidth::default(),
            side_content_min_width: 0,
            stacked_side_content: None,
            preserve_side_content_bg: false,
            on_selection_changed: None,
            allow_cancel: true,
            on_cancel: None,
        }
    }
}

/// Controls whether a searchable list starts in search mode or waits for a shortcut.
#[derive(Clone, Copy, Default)]
pub(crate) enum SearchMode {
    #[default]
    Immediate,
    OnDemand(KeyBinding),
}

/// Runtime state for rendering and interacting with a list-based selection popup.
///
/// This type is the single authority for filtered index mapping between
/// visible rows and source items and for preserving selection while filters
/// change.
pub(crate) struct ListSelectionView {
    presentation: super::ViewPresentation,
    picker_surface: PickerSurface,
    max_visible_rows: usize,
    reserve_result_rows: bool,
    allow_input_when_disconnected: bool,
    view_id: Option<&'static str>,
    footer_note: Option<Line<'static>>,
    footer_hint: Option<Line<'static>>,
    tab_footer_hints: Vec<(String, Line<'static>)>,
    items: Vec<SelectionItem>,
    search_items: Vec<SelectionItem>,
    tabs: Vec<SelectionTab>,
    active_tab_idx: Option<usize>,
    state: ScrollState,
    completion: Option<ViewCompletion>,
    pub(super) dismiss_after_child_accept: bool,
    pub(super) app_event_tx: AppEventSender,
    is_searchable: bool,
    search_toggle_key: Option<KeyBinding>,
    search_mode: SearchMode,
    search_active: bool,
    auto_number_shortcuts: bool,
    search_query: String,
    search_placeholder: Option<String>,
    col_width_mode: ColumnWidthMode,
    row_display: SelectionRowDisplay,
    description_layout: SelectionDescriptionLayout,
    name_column_width: Option<usize>,
    filtered_indices: Vec<usize>,
    last_selected_actual_idx: Option<usize>,
    rendered_item_count: Cell<usize>,
    search_cursor: Cell<Option<(u16, u16)>>,
    header: Box<dyn Renderable>,
    header_view_all_hint: Option<ShortcutHint>,
    header_gap: u16,
    initial_selected_idx: Option<usize>,
    side_content: Box<dyn Renderable>,
    side_content_width: SideContentWidth,
    side_content_min_width: u16,
    stacked_side_content: Option<Box<dyn Renderable>>,
    preserve_side_content_bg: bool,

    /// Called when the highlighted item changes (navigation, filter, number-key).
    on_selection_changed: OnSelectionChangedCallback,

    allow_cancel: bool,

    /// Called when the picker is dismissed via Esc/Ctrl+C without selecting.
    on_cancel: OnCancelCallback,
    keymap: ListKeymap,
}

const SELECTION_TOGGLE_ON_PREFIX: &str = "[*] ";
const SELECTION_CHECKBOX_ON_PREFIX: &str = "[x] ";
const SELECTION_TOGGLE_OFF_PREFIX: &str = "[ ] ";
pub(crate) const SELECTION_TOGGLE_UNAVAILABLE_PREFIX: &str = "[-] ";
pub(crate) const SELECTION_TOGGLE_BLOCKED_PREFIX: &str = "[!] ";

fn selection_toggle_prefix(item: &SelectionItem, toggle: &SelectionToggle) -> &'static str {
    if toggle.is_on {
        if item.checkbox_toggle {
            SELECTION_CHECKBOX_ON_PREFIX
        } else {
            SELECTION_TOGGLE_ON_PREFIX
        }
    } else {
        SELECTION_TOGGLE_OFF_PREFIX
    }
}

fn selection_item_toggle_prefix(item: &SelectionItem) -> Option<&'static str> {
    item.toggle_placeholder.or_else(|| {
        item.toggle
            .as_ref()
            .map(|toggle| selection_toggle_prefix(item, toggle))
    })
}

impl ListSelectionView {
    fn selected_item_has_toggle(&self) -> bool {
        self.selected_actual_idx()
            .and_then(|actual_idx| self.active_items().get(actual_idx))
            .is_some_and(|item| item.toggle.is_some() && Self::item_is_enabled(item))
    }

    fn selected_item_has_toggle_placeholder(&self) -> bool {
        self.selected_actual_idx()
            .and_then(|actual_idx| self.active_items().get(actual_idx))
            .is_some_and(|item| {
                item.toggle.is_none()
                    && item.toggle_placeholder.is_some()
                    && Self::item_is_enabled(item)
            })
    }

    fn toggle_selected(&mut self) {
        let Some(actual_idx) = self.selected_actual_idx() else {
            return;
        };
        let app_event_tx = self.app_event_tx.clone();
        let Some(item) = self.active_items_mut().get_mut(actual_idx) else {
            return;
        };
        if !Self::item_is_enabled(item) {
            return;
        }
        let Some(toggle) = item.toggle.as_mut() else {
            return;
        };

        toggle.is_on = !toggle.is_on;
        item.pending_toggle_actions = item.pending_toggle_actions.saturating_add(/*rhs*/ 1);
        (toggle.action)(toggle.is_on, &app_event_tx);
    }
}

impl ListSelectionView {
    /// Create a selection popup view with filtering, scrolling, and callbacks wired.
    ///
    /// The constructor normalizes header/title composition and immediately
    /// applies filtering so `ScrollState` starts in a valid visible range.
    /// When search is enabled, rows without `search_value` will disappear as
    /// soon as the query is non-empty, which can look like dropped data unless
    /// callers intentionally populate that field.
    pub fn new(
        params: SelectionViewParams,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        let mut header = params.header;
        if params.title.is_some() || params.subtitle.is_some() {
            let title = params.title.map(|title| Line::from(title.bold()));
            let subtitle = params.subtitle.map(|subtitle| Line::from(subtitle.dim()));
            header = Box::new(ColumnRenderable::with([
                header,
                Box::new(
                    Paragraph::new(title.into_iter().chain(subtitle).collect::<Vec<_>>())
                        .wrap(Wrap { trim: false }),
                ),
            ]));
        }
        let active_tab_idx = params.initial_tab_id.as_ref().and_then(|initial_tab_id| {
            params
                .tabs
                .iter()
                .position(|tab| tab.id.as_str() == initial_tab_id.as_str())
        });
        let active_tab_idx = if params.tabs.is_empty() {
            None
        } else {
            Some(active_tab_idx.unwrap_or(0))
        };
        let has_initial_selected_idx = params.initial_selected_idx.is_some();
        let search_active = matches!(params.search_mode, SearchMode::Immediate)
            || params.initial_search_query.is_some();
        let search_query = params.initial_search_query.unwrap_or_default();
        let mut s = Self {
            picker_surface: params.picker_surface,
            max_visible_rows: params.max_visible_rows.max(/*other*/ 1),
            reserve_result_rows: params.reserve_result_rows,
            allow_input_when_disconnected: params.allow_input_when_disconnected,
            presentation: params.presentation,
            view_id: params.view_id,
            footer_note: params.footer_note,
            footer_hint: params.footer_hint,
            tab_footer_hints: params.tab_footer_hints,
            items: params.items,
            search_items: params.search_items,
            tabs: params.tabs,
            active_tab_idx,
            state: ScrollState::new(),
            completion: None,
            dismiss_after_child_accept: false,
            app_event_tx,
            is_searchable: params.is_searchable,
            search_toggle_key: params.search_toggle_key,
            search_mode: params.search_mode,
            search_active,
            search_query,
            auto_number_shortcuts: params.auto_number_shortcuts,
            search_placeholder: if params.is_searchable {
                params.search_placeholder
            } else {
                None
            },
            col_width_mode: params.col_width_mode,
            row_display: params.row_display,
            description_layout: params.description_layout,
            name_column_width: params.name_column_width,
            filtered_indices: Vec::new(),
            last_selected_actual_idx: None,
            rendered_item_count: Cell::new(/*value*/ 0),
            search_cursor: Cell::new(/*value*/ None),
            header,
            header_view_all_hint: params.header_view_all_hint,
            header_gap: params.header_gap,
            initial_selected_idx: params.initial_selected_idx,
            side_content: params.side_content,
            side_content_width: params.side_content_width,
            side_content_min_width: params.side_content_min_width,
            stacked_side_content: params.stacked_side_content,
            preserve_side_content_bg: params.preserve_side_content_bg,
            on_selection_changed: params.on_selection_changed,
            allow_cancel: params.allow_cancel,
            on_cancel: params.on_cancel,
            keymap,
        };
        s.apply_filter();
        if s.tabs_enabled() && !has_initial_selected_idx && s.state.selected_idx.is_none() {
            s.select_first_enabled_row();
        }
        s
    }

    fn visible_len(&self) -> usize {
        self.filtered_indices.len()
    }

    fn tabs_enabled(&self) -> bool {
        self.active_tab_idx.is_some()
    }

    fn search_items_active(&self) -> bool {
        !self.search_query.is_empty() && !self.search_items.is_empty()
    }

    fn edit_search_query(&mut self, edit: impl FnOnce(&mut String)) {
        let search_items_were_active = self.search_items_active();
        edit(&mut self.search_query);
        if search_items_were_active != self.search_items_active() {
            self.state.reset();
        }
        self.apply_filter();
    }

    fn active_items(&self) -> &[SelectionItem] {
        if self.search_items_active() {
            return self.search_items.as_slice();
        }
        self.active_tab_idx
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| tab.items.as_slice())
            .unwrap_or(self.items.as_slice())
    }

    fn active_items_mut(&mut self) -> &mut [SelectionItem] {
        if self.search_items_active() {
            return self.search_items.as_mut_slice();
        }
        if let Some(idx) = self.active_tab_idx
            && let Some(tab) = self.tabs.get_mut(idx)
        {
            return tab.items.as_mut_slice();
        }
        self.items.as_mut_slice()
    }

    fn active_header(&self, width: u16) -> &dyn Renderable {
        self.active_tab_idx
            .and_then(|idx| self.tabs.get(idx))
            .filter(|tab| tab.header.desired_height(width) > 0)
            .map(|tab| tab.header.as_ref())
            .unwrap_or(self.header.as_ref())
    }

    fn header_height(&self, width: u16) -> u16 {
        let active_height = self.active_header(width).desired_height(width);
        if self.reserve_result_rows {
            self.tabs
                .iter()
                .map(|tab| tab.header.desired_height(width))
                .max()
                .unwrap_or(active_height)
                .max(active_height)
        } else {
            active_height
        }
    }

    fn active_footer_hint(&self) -> Option<&Line<'static>> {
        if let Some(item) = self
            .selected_actual_idx()
            .and_then(|idx| self.active_items().get(idx))
        {
            if self.on_demand_search_active()
                && let Some(hint) = item.search_footer_hint.as_ref()
            {
                return Some(hint);
            }
            if let Some(hint) = item.footer_hint.as_ref() {
                return Some(hint);
            }
            if let Some(action) = item.secondary_action.as_ref() {
                return Some(&action.footer_hint);
            }
        }
        self.active_tab_id()
            .and_then(|active_tab_id| {
                self.tab_footer_hints
                    .iter()
                    .find_map(|(tab_id, hint)| (tab_id.as_str() == active_tab_id).then_some(hint))
            })
            .or(self.footer_hint.as_ref())
    }

    fn active_tab_id(&self) -> Option<&str> {
        self.active_tab_idx
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| tab.id.as_str())
    }

    fn visible_rows(&self, len: usize) -> usize {
        let rendered = self.rendered_item_count.get();
        let limit = if rendered > 0 {
            self.max_visible_rows.min(rendered)
        } else {
            self.max_visible_rows
        };
        limit.min(len.max(/*other*/ 1))
    }

    pub(crate) fn selected_actual_idx(&self) -> Option<usize> {
        self.state
            .selected_idx
            .and_then(|visible_idx| self.filtered_indices.get(visible_idx).copied())
    }

    fn apply_filter(&mut self) {
        let previously_selected = self
            .selected_actual_idx()
            .filter(|actual_idx| self.enabled_actual_idx(*actual_idx).is_some())
            .or_else(|| {
                self.initial_selected_idx
                    .take()
                    .filter(|actual_idx| self.enabled_actual_idx(*actual_idx).is_some())
            })
            .or_else(|| {
                (!self.is_searchable)
                    .then(|| {
                        self.active_items()
                            .iter()
                            .position(|item| item.is_current && Self::item_is_enabled(item))
                    })
                    .flatten()
            });

        if self.is_searchable && !self.search_query.is_empty() {
            let query_lower = self.search_query.to_lowercase();
            self.filtered_indices = self
                .active_items()
                .iter()
                .positions(|item| {
                    item.search_value
                        .as_ref()
                        .is_some_and(|v| v.to_lowercase().contains(&query_lower))
                })
                .collect();
        } else {
            self.filtered_indices = (0..self.active_items().len()).collect();
        }

        let len = self.filtered_indices.len();
        let selected_visible_idx = self
            .state
            .selected_idx
            .and_then(|visible_idx| {
                self.filtered_indices
                    .get(visible_idx)
                    .and_then(|idx| self.filtered_indices.iter().position(|cur| cur == idx))
            })
            .or_else(|| {
                previously_selected.and_then(|actual_idx| {
                    self.filtered_indices
                        .iter()
                        .position(|idx| *idx == actual_idx)
                })
            });
        self.state.selected_idx = selected_visible_idx
            .filter(|visible_idx| {
                self.filtered_indices
                    .get(*visible_idx)
                    .and_then(|actual_idx| self.active_items().get(*actual_idx))
                    .is_some_and(Self::item_is_enabled)
            })
            .or_else(|| self.first_enabled_visible_idx())
            .or_else(|| (len > 0).then_some(0));

        let visible = self.visible_rows(len);
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, visible);

        // Notify the callback when filtering changes the selected actual item
        // so live preview stays in sync (e.g. typing in the theme picker).
        let new_actual = self.selected_actual_idx();
        if new_actual != previously_selected {
            self.fire_selection_changed();
        }
    }

    fn build_rows(&self) -> Vec<GenericDisplayRow> {
        let enabled_row_number_width = self
            .filtered_indices
            .iter()
            .filter(|actual_idx| {
                self.active_items()
                    .get(**actual_idx)
                    .is_some_and(|item| Self::item_is_enabled(item) && item.child_label.is_none())
            })
            .count()
            .max(1)
            .to_string()
            .len();
        let mut enabled_row_number = 0;
        let row_selection_style = Some(selection_style());
        self.filtered_indices
            .iter()
            .enumerate()
            .filter_map(|(visible_idx, actual_idx)| {
                self.active_items().get(*actual_idx).map(|item| {
                    let is_selected = self.state.selected_idx == Some(visible_idx);
                    let prefix = if is_selected { '›' } else { ' ' };
                    let name = item.name.as_str();
                    let marker = if item.is_current { " (current)" } else { "" };
                    let name_with_marker = format!("{name}{marker}");
                    let is_disabled = item.is_disabled || item.disabled_reason.is_some();
                    let wrap_prefix = if self.is_searchable {
                        // The number keys don't work when search is enabled (since we let the
                        // numbers be used for the search query).
                        format!("{prefix} ")
                    } else if !self.auto_number_shortcuts {
                        item.display_shortcut.map_or_else(
                            || format!("{prefix} "),
                            |shortcut| format!("{prefix} {}. ", shortcut.display_label()),
                        )
                    } else if is_disabled {
                        if let Some(disabled_gutter_marker) = item.disabled_gutter_marker {
                            let marker_width = UnicodeWidthStr::width(disabled_gutter_marker);
                            let marker_padding =
                                " ".repeat(enabled_row_number_width.saturating_sub(marker_width));
                            format!("{prefix} {marker_padding}{disabled_gutter_marker}  ")
                        } else {
                            format!("{prefix} {}", " ".repeat(enabled_row_number_width + 2))
                        }
                    } else if let Some(label) = item.child_label {
                        format!("{prefix}    {label}. ")
                    } else {
                        enabled_row_number += 1;
                        let n = enabled_row_number;
                        format!("{prefix} {n}. ")
                    };
                    let wrap_prefix_width = UnicodeWidthStr::width(wrap_prefix.as_str());
                    let mut name_prefix_spans = Vec::new();
                    name_prefix_spans.push(wrap_prefix.into());
                    if let Some(toggle_prefix) = selection_item_toggle_prefix(item) {
                        name_prefix_spans.push(toggle_prefix.into());
                    }
                    name_prefix_spans.extend(item.name_prefix_spans.clone());
                    let description = is_selected
                        .then(|| item.selected_description.clone())
                        .flatten()
                        .or_else(|| item.description.clone());
                    let wrap_indent = (description.is_none() && item.disabled_reason.is_none())
                        .then_some(wrap_prefix_width);
                    GenericDisplayRow {
                        category_tag: item.category_tag.clone(),
                        selection_style: row_selection_style,
                        name: name_with_marker,
                        name_prefix_spans,
                        display_shortcut: self
                            .auto_number_shortcuts
                            .then_some(item.display_shortcut)
                            .flatten(),
                        description,
                        wrap_indent,
                        is_disabled,
                        disabled_reason: item.disabled_reason.clone(),
                        ..Default::default()
                    }
                })
            })
            .collect()
    }

    fn switch_tab(&mut self, step: isize) {
        let Some(active_idx) = self.active_tab_idx else {
            return;
        };
        let len = self.tabs.len();
        if len == 0 {
            return;
        }

        let next_idx = if step.is_negative() {
            active_idx.checked_sub(1).unwrap_or(len - 1)
        } else {
            (active_idx + 1) % len
        };
        self.active_tab_idx = Some(next_idx);
        if self.search_items.is_empty() {
            self.search_query.clear();
        }
        self.rendered_item_count.set(/*val*/ 0);
        self.state.reset();
        self.apply_filter();
        if self.state.selected_idx.is_none() {
            self.select_first_enabled_row();
        }
        self.fire_selection_changed();
    }

    fn select_first_enabled_row(&mut self) {
        let selected_visible_idx = self
            .first_enabled_visible_idx()
            .or_else(|| (!self.filtered_indices.is_empty()).then_some(0));
        self.state.selected_idx = selected_visible_idx;
        self.state.scroll_top = 0;
    }

    fn first_enabled_visible_idx(&self) -> Option<usize> {
        self.filtered_indices.iter().position(|actual_idx| {
            self.active_items()
                .get(*actual_idx)
                .is_some_and(Self::item_is_enabled)
        })
    }

    fn enabled_actual_idx(&self, actual_idx: usize) -> Option<usize> {
        self.active_items()
            .get(actual_idx)
            .is_some_and(Self::item_is_enabled)
            .then_some(actual_idx)
    }

    fn item_is_enabled(item: &SelectionItem) -> bool {
        item.disabled_reason.is_none() && !item.is_disabled
    }

    fn actual_idx_for_enabled_number(&self, number: usize) -> Option<usize> {
        if number == 0 {
            return None;
        }

        self.active_items()
            .iter()
            .enumerate()
            .filter(|(_, item)| Self::item_is_enabled(item) && item.child_label.is_none())
            .nth(number - 1)
            .map(|(idx, _)| idx)
    }

    /// Moves the selection, steps past disabled rows, and reports a change.
    fn navigate(&mut self, action: ListAction) {
        let before = self.selected_actual_idx();
        let len = self.visible_len();
        let visible = self.visible_rows(len);
        match action {
            ListAction::MoveUp => {
                self.state.move_up_wrap(len);
                self.skip_disabled_up();
            }
            ListAction::MoveDown => {
                self.state.move_down_wrap(len);
                self.skip_disabled_down();
            }
            ListAction::PageUp => {
                self.state.page_up_clamped(len, visible);
                self.skip_disabled_up_clamped();
            }
            ListAction::PageDown => {
                self.state.page_down_clamped(len, visible);
                self.skip_disabled_down_clamped();
            }
            ListAction::JumpTop => {
                self.state.jump_top(len, visible);
                self.skip_disabled_down_clamped();
            }
            ListAction::JumpBottom => {
                self.state.jump_bottom(len, visible);
                self.skip_disabled_up_clamped();
            }
            // `vertical_action_for` yields only the six actions above.
            ListAction::MoveLeft
            | ListAction::MoveRight
            | ListAction::Accept
            | ListAction::Cancel => return,
        }
        self.state.ensure_visible(len, visible);
        if self.selected_actual_idx() != before {
            self.fire_selection_changed();
        }
    }

    fn fire_selection_changed(&self) {
        if let Some(cb) = &self.on_selection_changed
            && let Some(actual) = self.selected_actual_idx()
        {
            cb(actual, &self.app_event_tx);
        }
    }

    fn accept(&mut self, kind: SelectionActionKind) {
        let selected_actual_idx = self.selected_actual_idx();
        if matches!(kind, SelectionActionKind::Primary)
            && selected_actual_idx
                .and_then(|idx| self.active_items().get(idx))
                .is_some_and(|item| item.checkbox_toggle)
        {
            self.toggle_selected();
            self.last_selected_actual_idx = selected_actual_idx;
            return;
        }
        if let Some(item) = selected_actual_idx.and_then(|idx| self.active_items().get(idx)) {
            if item.is_disabled || item.disabled_reason.is_some() {
                return;
            }
            match kind {
                SelectionActionKind::Primary => {
                    for act in &item.actions {
                        act(&self.app_event_tx);
                    }
                }
                SelectionActionKind::Secondary => {
                    let Some(action) = &item.secondary_action else {
                        return;
                    };
                    (action.action)(&self.app_event_tx);
                }
            }
            if item.dismiss_on_select {
                self.completion = Some(ViewCompletion::Accepted);
            } else if item.dismiss_parent_on_child_accept {
                self.dismiss_after_child_accept = true;
            }
            self.last_selected_actual_idx = selected_actual_idx;
        } else if selected_actual_idx.is_none() {
            if let Some(cb) = &self.on_cancel {
                cb(&self.app_event_tx);
            }
            self.completion = Some(ViewCompletion::Cancelled);
        }
    }

    fn select_shortcut(&mut self, actual_idx: usize) {
        let previously_selected = self.selected_actual_idx();
        self.state.selected_idx = Some(actual_idx);
        if self
            .active_items()
            .get(actual_idx)
            .is_some_and(|item| item.require_explicit_confirmation)
        {
            if self.selected_actual_idx() != previously_selected {
                self.fire_selection_changed();
            }
        } else {
            self.accept(SelectionActionKind::Primary);
        }
    }

    /// Preserve a searchable picker's filter when its parent rebuilds the view.
    pub(crate) fn search_query(&self) -> Option<&str> {
        self.search_field_visible()
            .then_some(self.search_query.as_str())
    }

    fn search_field_visible(&self) -> bool {
        self.is_searchable && self.search_active
    }

    fn on_demand_search_active(&self) -> bool {
        self.search_active && matches!(self.search_mode, SearchMode::OnDemand(_))
    }

    fn stop_on_demand_search(&mut self) {
        self.search_active = false;
        self.edit_search_query(String::clear);
    }

    pub(crate) fn set_search_query(&mut self, query: String) {
        let selected_actual_idx = self.selected_actual_idx();
        let changes_item_source =
            self.search_items_active() != (!query.is_empty() && !self.search_items.is_empty());
        self.search_active = true;
        self.search_query = query;
        if changes_item_source {
            self.state.reset();
            self.initial_selected_idx = None;
        } else {
            self.state.selected_idx = None;
            self.initial_selected_idx = selected_actual_idx;
        }
        self.apply_filter();
    }

    pub(crate) fn take_last_selected_index(&mut self) -> Option<usize> {
        self.last_selected_actual_idx.take()
    }

    fn footer_hint_lines(&self, width: u16) -> Vec<Line<'_>> {
        let Some(hint) = self.active_footer_hint() else {
            return Vec::new();
        };
        let mut lines = wrap_styled_line(hint, width);
        if self.reserve_result_rows {
            let height = self
                .tab_footer_hints
                .iter()
                .map(|(_, hint)| wrap_styled_line(hint, width).len())
                .chain(
                    self.footer_hint
                        .iter()
                        .map(|hint| wrap_styled_line(hint, width).len()),
                )
                .chain(
                    self.items
                        .iter()
                        .chain(self.search_items.iter())
                        .chain(self.tabs.iter().flat_map(|tab| tab.items.iter()))
                        .flat_map(|item| {
                            [
                                item.footer_hint.as_ref().or_else(|| {
                                    item.secondary_action
                                        .as_ref()
                                        .map(|action| &action.footer_hint)
                                }),
                                item.search_footer_hint.as_ref(),
                            ]
                            .into_iter()
                            .flatten()
                        })
                        .map(|hint| wrap_styled_line(hint, width).len()),
                )
                .max()
                .unwrap_or(lines.len());
            lines.resize(height.max(lines.len()), Line::default());
        }
        lines
    }

    fn footer_note_lines(&self, width: u16, show_selected_description: bool) -> Vec<Line<'static>> {
        let wrap = |line| word_wrap_lines([line], usize::from(width.max(1)));
        let note = if show_selected_description {
            self.selected_actual_idx()
                .and_then(|idx| self.active_items().get(idx))
                .and_then(|item| item.description.as_ref())
                .map(|description| Line::from(description.clone()).dim())
        } else {
            None
        }
        .or_else(|| self.footer_note.clone());
        let mut lines = note.map(&wrap).unwrap_or_default();
        if self.reserve_result_rows && show_selected_description {
            let height = self
                .items
                .iter()
                .chain(self.search_items.iter())
                .chain(self.tabs.iter().flat_map(|tab| tab.items.iter()))
                .filter_map(|item| item.description.as_deref())
                .map(|description| wrap(Line::from(description).dim()).len())
                .chain(self.footer_note.iter().map(|note| wrap(note.clone()).len()))
                .max()
                .unwrap_or(lines.len());
            lines.resize(height.max(lines.len()), Line::default());
        }
        lines
    }

    fn result_area_height(&self, measured_height: u16) -> u16 {
        if self.reserve_result_rows {
            let reserved_height = self.max_visible_rows.min(usize::from(u16::MAX)) as u16;
            measured_height.max(reserved_height)
        } else {
            measured_height
        }
    }

    fn clear_to_terminal_bg(buf: &mut Buffer, area: Rect) {
        for position in area.intersection(buf.area).positions() {
            buf[position]
                .set_symbol(" ")
                .set_style(ratatui::style::Style::reset());
        }
    }

    fn force_bg_to_terminal_bg(buf: &mut Buffer, area: Rect) {
        for position in area.intersection(buf.area).positions() {
            buf[position].set_bg(ratatui::style::Color::Reset);
        }
    }

    fn stacked_side_content(&self) -> &dyn Renderable {
        self.stacked_side_content
            .as_deref()
            .unwrap_or_else(|| self.side_content.as_ref())
    }

    /// Returns `Some(side_width)` when the content area is wide enough for a
    /// side-by-side layout (list + gap + side panel), `None` otherwise.
    fn side_layout_width(&self, content_width: u16) -> Option<u16> {
        side_by_side_layout_widths(
            content_width,
            self.side_content_width,
            self.side_content_min_width,
        )
        .map(|(_, side_width)| side_width)
    }

    fn skip_disabled_down(&mut self) {
        let len = self.visible_len();
        for _ in 0..len {
            if self.selected_visible_idx_is_disabled() {
                self.state.move_down_wrap(len);
            } else {
                break;
            }
        }
    }

    fn skip_disabled_up(&mut self) {
        let len = self.visible_len();
        for _ in 0..len {
            if self.selected_visible_idx_is_disabled() {
                self.state.move_up_wrap(len);
            } else {
                break;
            }
        }
    }

    fn skip_disabled_down_clamped(&mut self) {
        let Some(start) = self.state.selected_idx else {
            return;
        };
        if !self.visible_idx_is_disabled(start) {
            return;
        }

        let len = self.visible_len();
        self.state.selected_idx = ((start + 1)..len)
            .find(|idx| !self.visible_idx_is_disabled(*idx))
            .or_else(|| {
                (0..start)
                    .rev()
                    .find(|idx| !self.visible_idx_is_disabled(*idx))
            })
            .or(Some(start));
    }

    fn skip_disabled_up_clamped(&mut self) {
        let Some(start) = self.state.selected_idx else {
            return;
        };
        if !self.visible_idx_is_disabled(start) {
            return;
        }

        let len = self.visible_len();
        self.state.selected_idx = (0..start)
            .rev()
            .find(|idx| !self.visible_idx_is_disabled(*idx))
            .or_else(|| ((start + 1)..len).find(|idx| !self.visible_idx_is_disabled(*idx)))
            .or(Some(start));
    }

    fn selected_visible_idx_is_disabled(&self) -> bool {
        self.state
            .selected_idx
            .is_some_and(|idx| self.visible_idx_is_disabled(idx))
    }

    fn visible_idx_is_disabled(&self, idx: usize) -> bool {
        self.filtered_indices
            .get(idx)
            .and_then(|actual_idx| self.active_items().get(*actual_idx))
            .is_some_and(|item| item.disabled_reason.is_some() || item.is_disabled)
    }
}

impl BottomPaneView for ListSelectionView {
    fn presentation(&self) -> super::ViewPresentation {
        self.presentation
    }

    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        crate::keymap::KeymapContextSet::new(crate::keymap::KeymapContext::List)
    }

    fn accepts_input_when_disconnected(&self) -> bool {
        self.allow_input_when_disconnected
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        // An active search reserves printable keys for the query. On-demand search leaves the
        // configured list bindings untouched until its shortcut is pressed.
        let search_field_visible = self.search_field_visible();
        let allow_plain_char_navigation =
            !search_field_visible || !is_plain_text_key_event(key_event);
        let allow_plain_char_actions =
            !self.on_demand_search_active() || !is_plain_text_key_event(key_event);
        if allow_plain_char_navigation
            && let Some(action) = self.keymap.vertical_action_for(key_event)
        {
            self.navigate(action);
            return;
        }

        match key_event {
            _ if self
                .search_toggle_key
                .is_some_and(|key| key.is_press(key_event))
                && self.keymap.action_for(key_event).is_none()
                && self.selected_item_has_toggle() =>
            {
                self.toggle_selected();
            }
            _ if allow_plain_char_navigation
                && self.tabs_enabled()
                && self.keymap.move_left.is_pressed(key_event) =>
            {
                self.switch_tab(/*step*/ -1)
            }
            _ if allow_plain_char_navigation
                && self.tabs_enabled()
                && self.keymap.move_right.is_pressed(key_event) =>
            {
                self.switch_tab(/*step*/ 1)
            }
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.on_demand_search_active() => self.stop_on_demand_search(),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.on_demand_search_active() => self.accept(SelectionActionKind::Primary),
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } if search_field_visible => {
                self.edit_search_query(|query| {
                    query.pop();
                });
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.selected_item_has_toggle()
                && self.keymap.action_for(key_event).is_none()
                && (!search_field_visible
                    || matches!(self.search_mode, SearchMode::Immediate)
                        && self.search_query.is_empty()) =>
            {
                self.toggle_selected()
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } if search_field_visible
                && matches!(self.search_mode, SearchMode::Immediate)
                && self.search_query.is_empty()
                && self.selected_item_has_toggle_placeholder() => {}
            _ if allow_plain_char_actions
                && self.allow_cancel
                && self.keymap.cancel.is_pressed(key_event) =>
            {
                self.on_ctrl_c();
            }
            _ if allow_plain_char_actions && self.keymap.accept.is_pressed(key_event) => {
                self.accept(SelectionActionKind::Primary)
            }
            _ if allow_plain_char_navigation
                && self
                    .selected_actual_idx()
                    .and_then(|idx| self.active_items().get(idx))
                    .and_then(|item| item.secondary_action.as_ref())
                    .is_some_and(|action| action.key.is_press(key_event)) =>
            {
                self.accept(SelectionActionKind::Secondary);
            }
            _ if self.is_searchable
                && !self.search_active
                && matches!(
                    self.search_mode,
                    SearchMode::OnDemand(binding) if binding.is_press(key_event)
                )
                && !self.keymap.has_chord_prefix(key_event) =>
            {
                self.search_active = true;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                ..
            } if c.is_ascii_control() => {}
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if search_field_visible
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.edit_search_query(|query| query.push(c));
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !self.is_searchable
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(idx) = self.items.iter().position(|item| {
                    (item
                        .display_shortcut
                        .is_some_and(|shortcut| shortcut.is_press(key_event))
                        || item.child_label == Some(c))
                        && Self::item_is_enabled(item)
                }) {
                    self.select_shortcut(idx);
                    return;
                }
                if self.auto_number_shortcuts
                    && let Some(idx) = c
                        .to_digit(/*radix*/ 10)
                        .map(|d| d as usize)
                        .and_then(|number| self.actual_idx_for_enabled_number(number))
                {
                    self.select_shortcut(idx);
                }
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if !self.search_field_visible() {
            return false;
        }
        let Some(pasted) = normalize_pasted_search_query(&pasted) else {
            return false;
        };
        self.edit_search_query(|query| query.push_str(&pasted));
        true
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn dismiss_after_child_accept(&self) -> bool {
        self.dismiss_after_child_accept
    }

    fn clear_dismiss_after_child_accept(&mut self) {
        self.dismiss_after_child_accept = false;
    }

    fn view_id(&self) -> Option<&'static str> {
        self.view_id
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_actual_idx()
    }

    fn selection_toggle_states(&self) -> Vec<Option<SelectionToggleState>> {
        self.active_items()
            .iter()
            .map(|item| {
                item.toggle
                    .as_ref()
                    .filter(|_| item.pending_toggle_actions > 0)
                    .map(|toggle| SelectionToggleState {
                        is_on: toggle.is_on,
                        pending_actions: item.pending_toggle_actions,
                    })
            })
            .collect()
    }

    fn complete_selection_toggle(&mut self, index: usize) {
        if let Some(item) = self.active_items_mut().get_mut(index) {
            item.pending_toggle_actions =
                item.pending_toggle_actions.saturating_sub(/*rhs*/ 1);
        }
    }

    fn search_query(&self) -> Option<&str> {
        self.is_searchable.then_some(self.search_query.as_str())
    }

    fn active_tab_id(&self) -> Option<&str> {
        ListSelectionView::active_tab_id(self)
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if self.on_demand_search_active() {
            self.stop_on_demand_search();
            return CancellationEvent::Handled;
        }
        if !self.allow_cancel {
            return CancellationEvent::NotHandled;
        }
        if let Some(cb) = &self.on_cancel {
            cb(&self.app_event_tx);
        }
        self.completion = Some(ViewCompletion::Cancelled);
        CancellationEvent::Handled
    }
}

impl Renderable for ListSelectionView {
    fn cursor_pos(&self, _area: Rect) -> Option<(u16, u16)> {
        if self.on_demand_search_active() {
            self.search_cursor.get()
        } else {
            None
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        // Inner content width after menu surface horizontal insets (2 per side).
        let inner_width = popup_content_width(width);

        // When side-by-side is active, measure the list at the reduced width
        // that accounts for the gap and side panel.
        let effective_rows_width = if let Some(side_w) = self.side_layout_width(inner_width) {
            // Leave the preview's right inset outside the full-width focus row.
            width.saturating_sub(MENU_SURFACE_HORIZONTAL_INSET / 2 + SIDE_CONTENT_GAP + side_w)
        } else {
            width
        };

        // Measure at the same width as the painted rows, including any side panel.
        let rows = self.build_rows();
        let column_width = ColumnWidthConfig::new(self.col_width_mode, self.name_column_width)
            .with_description_layout(self.description_layout);
        let rows_height = match self.row_display {
            SelectionRowDisplay::Wrapped => measure_rows_height_with_col_width_mode(
                &rows,
                &self.state,
                self.max_visible_rows,
                effective_rows_width,
                column_width,
            ),
            SelectionRowDisplay::SingleLine => {
                rows.len().clamp(/*min*/ 1, self.max_visible_rows) as u16
            }
        };

        let rows_height = self.result_area_height(rows_height);
        let tab_height = tab_bar_height(&self.tabs, inner_width);
        let search_field_visible = self.search_field_visible();
        let mut height = self.header_height(inner_width);
        let header_gap = if height == 0 { 0 } else { self.header_gap };
        let tab_gap = u16::from(tab_height > 0 && search_field_visible);
        height = height.saturating_add(tab_height + tab_gap);
        height = height.saturating_add(rows_height + 2 + header_gap);
        if self.picker_surface == PickerSurface::Panel {
            height = height.saturating_add(/*rhs*/ 1);
        }
        if search_field_visible {
            height = height.saturating_add(/*rhs*/ 1);
        }

        // Side content: when the terminal is wide enough the panel sits beside
        // the list and shares vertical space; otherwise it stacks below.
        if self.side_layout_width(inner_width).is_some() {
            // Side-by-side — side content shares list rows vertically so it
            // doesn't add to total height.
        } else {
            let side_h = self.stacked_side_content().desired_height(inner_width);
            if side_h > 0 {
                height = height.saturating_add(1 + side_h);
            }
        }

        let show_selected_description = should_show_selected_description_below(
            &rows,
            &self.state,
            self.max_visible_rows,
            effective_rows_width,
            column_width,
        );
        height = height.saturating_add(
            self.footer_note_lines(width.saturating_sub(/*rhs*/ 2), show_selected_description)
                .len() as u16,
        );
        height = height.saturating_add(
            self.footer_hint_lines(width.saturating_sub(/*rhs*/ 2))
                .len() as u16,
        );
        height
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.rendered_item_count.set(/*val*/ 0);
        self.search_cursor.set(/*val*/ None);
        if area.height == 0 || area.width == 0 {
            return;
        }
        let search_field_visible = self.search_field_visible();
        let inner_width = popup_content_width(area.width);
        let side_w = self.side_layout_width(inner_width);
        let effective_rows_width = if let Some(sw) = side_w {
            area.width
                .saturating_sub(MENU_SURFACE_HORIZONTAL_INSET / 2 + SIDE_CONTENT_GAP + sw)
        } else {
            area.width
        };
        let rows = self.build_rows();
        let column_width = ColumnWidthConfig::new(self.col_width_mode, self.name_column_width)
            .with_description_layout(self.description_layout);
        let show_selected_description = should_show_selected_description_below(
            &rows,
            &self.state,
            self.max_visible_rows,
            effective_rows_width,
            column_width,
        );
        let note_width = area.width.saturating_sub(2);
        let note_lines = self.footer_note_lines(note_width, show_selected_description);
        let note_height = note_lines.len() as u16;
        let hint_lines = self.footer_hint_lines(note_width);
        let hint_height = hint_lines.len() as u16;
        let header_height = self.header_height(inner_width);
        let tab_height = tab_bar_height(&self.tabs, inner_width);
        let mut footer_rows = note_height + hint_height;
        // Wrapped hints yield before the title, controls, and selected result.
        let minimum_content = u16::from(self.picker_surface == PickerSurface::Panel)
            + header_height.min(/*other*/ 1)
            + tab_height
            + u16::from(search_field_visible)
            + 1;
        footer_rows = footer_rows.min(area.height.saturating_sub(minimum_content));
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(footer_rows)]).areas(area);

        let outer_content_area = content_area;
        Self::clear_to_terminal_bg(buf, outer_content_area);
        if self.picker_surface == PickerSurface::Panel {
            ratatui::widgets::Block::default()
                .style(crate::style::user_message_style())
                .render(outer_content_area, buf);
        }
        // The list owns its vertical spacer rows so hints remain adjacent
        // to results even when search, tabs, or a header are present.
        let content_area = outer_content_area.inset(Insets::vh(/*v*/ 0, /*h*/ 2));
        let content_area = if self.picker_surface == PickerSurface::Panel {
            // At tiny heights, leave room for both the clipped header and selected row.
            let top_gap = u16::from(content_area.height > 2);
            Rect::new(
                content_area.x,
                content_area.y.saturating_add(top_gap),
                content_area.width,
                content_area.height.saturating_sub(top_gap),
            )
        } else {
            content_area
        };

        let header = self.active_header(inner_width);
        let rows_height = match self.row_display {
            SelectionRowDisplay::Wrapped => measure_rows_height_with_col_width_mode(
                &rows,
                &self.state,
                self.max_visible_rows,
                effective_rows_width,
                column_width,
            ),
            SelectionRowDisplay::SingleLine => {
                rows.len().clamp(/*min*/ 1, self.max_visible_rows) as u16
            }
        };

        let rows_height = self.result_area_height(rows_height);

        // Stacked (fallback) side content height — only used when not side-by-side.
        let stacked_side_h = if side_w.is_none() {
            self.stacked_side_content().desired_height(inner_width)
        } else {
            0
        };

        let [
            header_area,
            _,
            tabs_area,
            _,
            search_area,
            above_area,
            list_area,
            below_area,
            _,
            stacked_side_area,
        ] = picker_areas(
            content_area,
            PickerLayoutSizes {
                header: header_height,
                header_gap: self.header_gap,
                tabs: tab_height,
                search: u16::from(search_field_visible),
                rows: rows_height,
                side: stacked_side_h,
            },
        );

        if !search_field_visible && header_area.height < header_height {
            let [header_area, elision_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(header_area);
            header.render(header_area, buf);
            let mut spans = vec![format!("[… {header_height} lines]").into()];
            if let Some(hint) = &self.header_view_all_hint {
                spans.push(" ".into());
                spans.extend(hint.spans());
                spans.push(" view all".into());
            }
            Line::from(spans).dim().render(elision_area, buf);
        } else {
            header.render(header_area, buf);
        }

        // -- Tabs --
        if tab_height > 0 {
            render_tab_bar(
                &self.tabs,
                self.active_tab_idx.unwrap_or(/*default*/ 0),
                tabs_area,
                buf,
            );
        }

        // -- Search bar --
        if search_field_visible {
            let query_span: Span<'static> = if self.search_query.is_empty() {
                self.search_placeholder
                    .as_ref()
                    .map(|placeholder| placeholder.clone().dim())
                    .unwrap_or_else(|| "".into())
            } else {
                self.search_query.clone().into()
            };
            if self.on_demand_search_active() {
                let label = "Search › ";
                buf.set_style(search_area, crate::bottom_pane::active_tab_style());
                Line::from(vec![label.cyan().bold(), query_span]).render(search_area, buf);
                let cursor_offset = (label.width() + self.search_query.width())
                    .min(usize::from(search_area.width.saturating_sub(/*rhs*/ 1)));
                self.search_cursor.set(Some((
                    search_area.x.saturating_add(cursor_offset as u16),
                    search_area.y,
                )));
            } else {
                Line::from(query_span).render(search_area, buf);
            }
        }

        // -- List rows --
        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(/*rhs*/ 2),
                y: list_area.y,
                width: effective_rows_width.max(1),
                height: list_area.height,
            };
            let rendered_rows = match self.row_display {
                SelectionRowDisplay::Wrapped => render_rows_with_col_width_mode(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                    column_width,
                ),
                SelectionRowDisplay::SingleLine => render_rows_single_line_with_col_width_mode(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                    column_width,
                ),
            };
            self.rendered_item_count.set(rendered_rows.items);
            if above_area.height > 0 && below_area.height > 0 {
                render_scroll_indicators(
                    Rect::new(
                        render_area.x,
                        above_area.y,
                        render_area.width,
                        below_area.bottom().saturating_sub(above_area.y),
                    ),
                    buf,
                    rendered_rows,
                );
            }
        }

        // -- Side content (preview panel) --
        if let Some(sw) = side_w {
            // Side-by-side: render to the right half of the popup content
            // area so preview content can center vertically in that panel.
            let side_x = content_area.x + content_area.width - sw;
            let side_area = Rect::new(side_x, content_area.y, sw, content_area.height);

            // Clear the menu-surface background behind the side panel so the
            // preview appears on the terminal's own background.
            let clear_x = side_x.saturating_sub(SIDE_CONTENT_GAP);
            let clear_w = outer_content_area
                .x
                .saturating_add(outer_content_area.width)
                .saturating_sub(clear_x);
            let bg_area = Rect::new(
                clear_x,
                outer_content_area.y,
                clear_w,
                outer_content_area.height,
            );
            Self::clear_to_terminal_bg(buf, bg_area);
            self.side_content.render(side_area, buf);
            if !self.preserve_side_content_bg {
                Self::force_bg_to_terminal_bg(buf, bg_area);
            }
        } else if stacked_side_area.height > 0 {
            // Stacked fallback: render below the list (same as old footer_content).
            let clear_height = (outer_content_area.y + outer_content_area.height)
                .saturating_sub(stacked_side_area.y);
            let clear_area = Rect::new(
                outer_content_area.x,
                stacked_side_area.y,
                outer_content_area.width,
                clear_height,
            );
            Self::clear_to_terminal_bg(buf, clear_area);
            self.stacked_side_content().render(stacked_side_area, buf);
        }

        if footer_area.height > 0 {
            let [note_area, hint_area] = Layout::vertical([
                Constraint::Length(note_height),
                Constraint::Length(hint_height),
            ])
            .areas(footer_area);

            if !note_lines.is_empty() {
                let note_area = Rect {
                    x: note_area.x + 2,
                    y: note_area.y,
                    width: note_area.width.saturating_sub(2),
                    height: note_area.height,
                };
                for (idx, line) in note_lines.iter().enumerate() {
                    if idx as u16 >= note_area.height {
                        break;
                    }
                    let line_area = Rect {
                        x: note_area.x,
                        y: note_area.y + idx as u16,
                        width: note_area.width,
                        height: 1,
                    };
                    line.clone().render(line_area, buf);
                }
            }

            if !hint_lines.is_empty() {
                let hint_area = Rect {
                    x: hint_area.x + 2,
                    y: hint_area.y,
                    width: hint_area.width.saturating_sub(2),
                    height: hint_area.height,
                };
                Paragraph::new(hint_lines).dim().render(hint_area, buf);
            }
        }
    }
}

impl ListSelectionView {
    pub(crate) fn rendered_item_count(&self) -> usize {
        self.rendered_item_count.get()
    }
}

#[cfg(test)]
#[path = "list_selection_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "selection_picker_tests.rs"]
mod picker_tests;
