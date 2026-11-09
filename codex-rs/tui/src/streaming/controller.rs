//! Two-region streaming controllers for agent messages and proposed plans.
//!
//! Each stream partitions rendered markdown into a *stable region* (committed
//! to scrollback via the animation queue in `StreamState`) and a *tail region*
//! (mutable, displayed in the active-cell slot as a transient stream-tail cell).
//!
//! `StreamCore` owns the shared bookkeeping: source accumulation, re-rendering,
//! stable/tail partitioning, commit-animation queue management, and terminal
//! resize handling.  `StreamController` and `PlanStreamController` are thin
//! wrappers that add only their `emit()` styling and finalize return types.
//!
//! ## Table holdback
//!
//! Table rendering is inherently non-incremental: adding a new row can change
//! every column's width and reshape all prior rows.  The holdback mechanism
//! (`table_holdback_state`) detects pipe-table patterns (header + delimiter
//! pair) in the accumulated source and keeps content from the table header
//! onward as mutable tail until its containing top-level block is stable.
//! Holdback is enabled for agent and proposed-plan streams. Lines in `Outside`
//! and `Markdown` fence contexts are scanned; lines inside non-markdown fences
//! are skipped.
//!
//! Transformable fences stay mutable while their containing top-level block is last. Markdown
//! fences may gain literal delimiters when table rendering is disabled. For Mermaid, the closing fence replaces
//! source with a diagram, and resizing can replace a diagram that no longer fits with its source.
//! Once another block starts, the diagram enters scrollback so later prose does not grow the tail.
//!
//! ## Resize handling
//!
//! On terminal width change, `StreamCore::set_width` re-renders at the new
//! width and rebuilds the queued stable region from the current emitted line
//! count. This intentionally avoids byte-level remap complexity while the
//! stream is active; finalized content is canonicalized by transcript
//! consolidation into source-backed markdown cells.
//!
//! ## Invariants
//!
//! - `emitted_stable_len <= enqueued_stable_len <= render.lines.len()`.
//! - committed source is append-only until `reset()`; never modified mid-stream.
//! - The committed-source tail starts at `enqueued_stable_len`; a bounded prose preview may
//!   follow it without affecting any stable line counts.
//! - During confirmed table streaming, only lines from the table header onward
//!   are forced into tail; pre-table lines may remain stable.

use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::{self};
use crate::inline_visualization::InlineVisualizationContext;
use crate::markdown::render_markdown_agent_with_list_spacing;
use crate::markdown_render::ListSpacing;
use crate::style::proposed_plan_style;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use ratatui::prelude::Stylize;
use ratatui::text::Line;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use super::StreamState;
use super::prose_preview::PreviewMode;
use super::prose_preview::ProsePreview;
use super::render::StreamingRender;
#[cfg(test)]
use super::render::render_source;
use super::render::render_source_with_list_spacing;
use super::table_holdback::TableHoldbackScanner;
use super::table_holdback::TableHoldbackState;
#[cfg(test)]
use super::table_holdback::table_holdback_state;

// ---------------------------------------------------------------------------
// StreamCore — shared bookkeeping for both stream controllers
// ---------------------------------------------------------------------------

/// Shared state and logic for the two-region streaming model.
///
/// Both [`StreamController`] (agent messages) and [`PlanStreamController`]
/// (proposed plans) delegate their core bookkeeping here: source
/// accumulation, re-rendering, stable/tail partitioning, commit-animation
/// queue management, and terminal resize handling.
///
/// The wrapping controllers add only their own `emit()` styling and
/// finalize return types.
struct StreamCore {
    state: StreamState,
    /// Current rendering width (columns available for markdown content).
    width: Option<usize>,
    /// Incremental render of committed source at `width`.
    render: StreamingRender,
    /// Disposable partial prose, never counted in the stable render or queue.
    preview: ProsePreview,
    /// Lines enqueued into the commit-animation queue.
    enqueued_stable_len: usize,
    /// Lines actually emitted to scrollback.
    emitted_stable_len: usize,
    /// Session cwd used to keep local file-link display stable during stream re-renders.
    cwd: PathBuf,
    inline_visualization_context: Option<InlineVisualizationContext>,
    render_mode: HistoryRenderMode,
    /// Cached rendered line count for prefix-before-table keyed by source start and width.
    stable_prefix_len_cache: Option<StablePrefixLenCache>,
    /// Incremental holdback scanner state for append-only source updates.
    holdback_scanner: TableHoldbackScanner,
}

struct StablePrefixLenCache {
    /// Byte offset of the candidate table/header start in committed source.
    source_start: usize,
    /// Width that produced `stable_prefix_len`.
    width: Option<usize>,
    /// Rendered line count for the committed prefix before `source_start` at `width`.
    ///
    /// The streaming controller uses this to avoid repeatedly re-rendering the
    /// same stable prefix while a live table tail is still mutating.
    stable_prefix_len: usize,
}

impl StreamCore {
    fn new(
        width: Option<usize>,
        cwd: &Path,
        render_mode: HistoryRenderMode,
        inline_visualization_context: Option<InlineVisualizationContext>,
    ) -> Self {
        Self {
            state: StreamState::new(width, cwd),
            width,
            render: StreamingRender::new(),
            preview: ProsePreview::default(),
            enqueued_stable_len: 0,
            emitted_stable_len: 0,
            cwd: cwd.to_path_buf(),
            inline_visualization_context,
            render_mode,
            stable_prefix_len_cache: None,
            holdback_scanner: TableHoldbackScanner::new(),
        }
    }

    /// Push a delta and report whether the queue or prose preview changed.
    ///
    /// Only newline-terminated source is committed to the stable render. This is
    /// important for tables because an unterminated partial row must stay out
    /// of both the stable queue and the live tail until its structure is
    /// unambiguous; otherwise the user can briefly see malformed columns that
    /// immediately disappear on the next delta. Unterminated prose gets a separate,
    /// bounded preview that never advances the stable boundary.
    fn push_delta(&mut self, delta: &str) -> bool {
        if !delta.is_empty() {
            self.state.has_seen_delta = true;
        }
        self.state.collector.push_delta(delta);

        if delta.contains('\n') {
            self.preview = ProsePreview::default();
        }
        let mut enqueued = false;
        if delta.contains('\n')
            && let Some(range) = self.state.collector.commit_complete_source()
        {
            let source = self.state.collector.committed_source();
            let committed_source = &source[range];
            self.render.append(
                source,
                committed_source,
                self.width,
                self.cwd.as_path(),
                self.render_mode,
                self.inline_visualization_context.as_ref(),
            );
            self.holdback_scanner
                .scan(source, self.render.completed_source_len);
            enqueued = self.sync_stable_queue();
        }
        let preview_changed = self.refresh_preview();
        enqueued || preview_changed
    }

    fn refresh_preview(&mut self) -> bool {
        let pending = self.state.collector.pending_source();
        let prose_preview = self.holdback_scanner.allows_prose_preview();
        let math = self.render_mode == HistoryRenderMode::Rich
            && (self.render.pending_math_start.is_some()
                || prose_preview && (pending.starts_with("$$") || pending.starts_with("\\[")));
        if math || prose_preview {
            self.preview.update(
                pending,
                self.width,
                &self.cwd,
                if math {
                    PreviewMode::Math
                } else {
                    PreviewMode::Prose(self.render_mode)
                },
                self.inline_visualization_context.as_ref(),
            )
        } else {
            self.preview = ProsePreview::default();
            false
        }
    }

    /// Drain the collector, render the final source snapshot, and return lines not yet emitted.
    ///
    /// This intentionally re-renders from the full raw source instead of
    /// trying to stitch together queued stable lines and the current tail. The
    /// remaining rows keep the stream's spacing so emitted offsets stay valid. Consolidation
    /// renders the returned source with the completed cell's spacing policy. Callers that skip
    /// `reset()` can accidentally replay a finished stream into the next answer.
    fn finalize_remaining(&mut self) -> (Vec<HyperlinkLine>, String) {
        let source = self.state.collector.finalize_and_take_source();
        let mut rendered = render_source_with_list_spacing(
            &source,
            self.width,
            self.cwd.as_path(),
            self.render_mode,
            self.inline_visualization_context.as_ref(),
            self.render.list_spacing,
        );
        let remaining = rendered.split_off(self.emitted_stable_len.min(rendered.len()));
        (remaining, source)
    }

    /// Step animation: dequeue one line, update the emitted count.
    fn tick(&mut self) -> Vec<HyperlinkLine> {
        let step = self.state.step();
        self.emitted_stable_len += step.len();
        step
    }

    /// Batch drain: dequeue up to `max_lines`, update the emitted count.
    fn tick_batch(&mut self, max_lines: usize) -> Vec<HyperlinkLine> {
        if max_lines == 0 {
            return Vec::new();
        }
        let step = self.state.drain_n(max_lines);
        if step.is_empty() {
            return step;
        }
        self.emitted_stable_len += step.len();
        step
    }

    // Trivial StreamCore accessors inlined — called on every animation tick
    // and render frame during active streaming.

    #[inline]
    fn is_idle(&self) -> bool {
        self.state.is_idle()
    }

    #[inline]
    fn queued_lines(&self) -> usize {
        self.state.queued_len()
    }

    #[inline]
    fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.state.oldest_queued_age(now)
    }

    /// Lines that belong to the mutable tail, not yet queued for stable commit.
    ///
    /// The tail starts at `enqueued_stable_len`, so this returns the portion
    /// of the current render snapshot that is still allowed to change without
    /// violating scrollback ordering. If callers were to derive the tail from
    /// `emitted_stable_len` instead, queued-but-not-yet-emitted lines could
    /// reappear in the active cell and duplicate content on screen.
    #[inline]
    fn current_tail_lines(&self) -> Vec<HyperlinkLine> {
        let start = self.enqueued_stable_len.min(self.render.lines.len());
        let mut lines = self.render.lines[start..].to_vec();
        lines.extend(self.preview.lines.iter().cloned());
        lines
    }

    /// Whether committed Markdown needs tail reflow. Disposable prose previews do not.
    #[inline]
    fn has_tail(&self) -> bool {
        self.enqueued_stable_len < self.render.lines.len()
    }

    /// Update rendering width and rebuild queued stable lines for the new layout.
    ///
    /// Re-renders once at the new width and rebuilds queue state from the
    /// current emitted line count.
    ///
    /// Resize is the point where source-backed rendering matters most:
    /// previously emitted prose must stay in scrollback order, while any live
    /// table tail is free to reshape at the new width. This method preserves
    /// that split without attempting byte-for-byte line remapping.
    fn set_width(&mut self, width: Option<usize>) {
        if self.width == width {
            return;
        }
        let had_pending_queue = self.state.queued_len() > 0;
        let had_live_tail = self.has_tail();
        let previous_width = self.width;
        self.width = width;
        self.state.collector.set_width(width);
        let source = self.state.collector.committed_source();
        if source.is_empty() {
            self.refresh_preview();
            return;
        }

        self.recompute_render(previous_width, self.render_mode);
        self.refresh_preview();
        if had_pending_queue
            && self.emitted_stable_len == self.render.lines.len()
            && self.emitted_stable_len > 0
        {
            // If wrapped remainder compresses into fewer lines at the new width,
            // keep at least one line un-emitted so pre-resize pending content is
            // not skipped permanently.
            self.emitted_stable_len -= 1;
        }
        self.state.clear_queue();
        if self.emitted_stable_len > 0 && !had_pending_queue && !had_live_tail {
            // Avoid replaying already-emitted content after resize when no
            // stable lines were waiting in the queue and there was no mutable
            // tail to preserve.
            self.emitted_stable_len = self.render.lines.len();
            self.enqueued_stable_len = self.render.lines.len();
            return;
        }
        self.rebuild_stable_queue_from_render();
    }

    /// Clear all accumulated state for current stream.
    fn reset(&mut self) {
        self.state.clear();
        self.render.clear();
        self.preview = ProsePreview::default();
        self.enqueued_stable_len = 0;
        self.emitted_stable_len = 0;
        self.stable_prefix_len_cache = None;
        self.holdback_scanner.reset();
    }

    fn set_render_mode(&mut self, render_mode: HistoryRenderMode) {
        if self.render_mode == render_mode {
            return;
        }

        let had_pending_queue = self.state.queued_len() > 0;
        let had_live_tail = self.has_tail();
        let previous_render_mode = self.render_mode;
        self.render_mode = render_mode;
        let source = self.state.collector.committed_source();
        if source.is_empty() {
            self.refresh_preview();
            return;
        }

        self.recompute_render(self.width, previous_render_mode);
        self.refresh_preview();
        if had_pending_queue
            && self.emitted_stable_len == self.render.lines.len()
            && self.emitted_stable_len > 0
        {
            self.emitted_stable_len -= 1;
        }
        self.state.clear_queue();
        if self.emitted_stable_len > 0 && !had_pending_queue && !had_live_tail {
            self.emitted_stable_len = self.render.lines.len();
            self.enqueued_stable_len = self.render.lines.len();
            return;
        }
        self.rebuild_stable_queue_from_render();
    }

    /// Preserve an emitted source prefix when resizing changes earlier diagrams' heights.
    fn recompute_render(
        &mut self,
        previous_width: Option<usize>,
        previous_render_mode: HistoryRenderMode,
    ) {
        let previous_tail_start = self.active_tail_source_start(previous_render_mode);
        let source = self.state.collector.committed_source();
        self.render.recompute(
            source,
            self.width,
            self.cwd.as_path(),
            self.render_mode,
            self.inline_visualization_context.as_ref(),
        );
        self.holdback_scanner
            .scan(source, self.render.completed_source_len);
        if let Some(start) = previous_tail_start.or(self.active_tail_source_start(self.render_mode))
        {
            let prefix_len = |width, mode| {
                render_source_with_list_spacing(
                    &source[..start],
                    width,
                    self.cwd.as_path(),
                    mode,
                    self.inline_visualization_context.as_ref(),
                    self.render.list_spacing,
                )
                .len()
            };
            let previous_prefix_len = prefix_len(previous_width, previous_render_mode);
            let prefix_len = prefix_len(self.width, self.render_mode);
            if self.emitted_stable_len >= previous_prefix_len {
                self.emitted_stable_len =
                    prefix_len.saturating_add(self.emitted_stable_len - previous_prefix_len);
            } else {
                self.emitted_stable_len = self.emitted_stable_len.min(prefix_len);
            }
        }
        self.emitted_stable_len = self.emitted_stable_len.min(self.render.lines.len());
    }

    fn active_tail_source_start(&self, render_mode: HistoryRenderMode) -> Option<usize> {
        if render_mode == HistoryRenderMode::Raw {
            return None;
        }
        let table_start = match self.holdback_scanner.state() {
            TableHoldbackState::Confirmed { table_start }
            | TableHoldbackState::PendingHeader {
                header_start: table_start,
            } => Some(table_start),
            TableHoldbackState::None => None,
        };
        let source = self.state.collector.committed_source();
        let source = source.strip_suffix('\n').unwrap_or(source);
        let marker_start = source.rfind('\n').map_or(0, |index| index + 1);
        let marker = source[marker_start..]
            .trim()
            .trim_start_matches(['>', ' ', '\t']);
        let bare_list_marker = matches!(marker, "-" | "+" | "*")
            || marker.strip_suffix(['.', ')']).is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            });
        [
            table_start,
            self.render.mutable_fence_start,
            self.render.pending_math_start,
            bare_list_marker.then_some(marker_start),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Compute how many rendered lines should be in the stable region.
    fn compute_target_stable_len(&mut self) -> usize {
        let tail_budget = self.active_tail_budget_lines();
        self.render
            .lines
            .len()
            .saturating_sub(tail_budget)
            .max(self.emitted_stable_len)
    }

    /// Advance `enqueued_stable_len` toward the target stable boundary and enqueue any
    /// newly-stable lines. Returns `true` if new lines were enqueued.
    fn sync_stable_queue(&mut self) -> bool {
        let target_stable_len = self.compute_target_stable_len();

        // A structural rewrite moved the stable boundary backward into enqueue-but-unemitted
        // lines. Rebuild queue from the latest snapshot.
        if target_stable_len < self.enqueued_stable_len {
            self.state.clear_queue();
            if self.emitted_stable_len < target_stable_len {
                self.state.enqueue(
                    self.render.lines[self.emitted_stable_len..target_stable_len].to_vec(),
                );
            }
            self.enqueued_stable_len = target_stable_len;
            return self.state.queued_len() > 0;
        }

        if target_stable_len == self.enqueued_stable_len {
            return false;
        }

        self.state
            .enqueue(self.render.lines[self.enqueued_stable_len..target_stable_len].to_vec());
        self.enqueued_stable_len = target_stable_len;
        true
    }

    /// Rebuild the stable queue from the current render snapshot.
    ///
    /// This is used after `set_width()`, where any queued lines were computed
    /// against the old width and can no longer be trusted to line up with the
    /// current render.
    fn rebuild_stable_queue_from_render(&mut self) {
        let target_stable_len = self.compute_target_stable_len();
        self.state.clear_queue();
        if self.emitted_stable_len < target_stable_len {
            self.state
                .enqueue(self.render.lines[self.emitted_stable_len..target_stable_len].to_vec());
        }
        self.enqueued_stable_len = target_stable_len;
    }

    /// How many rendered lines to withhold as mutable tail.
    ///
    /// When a table is detected (`Confirmed` or `PendingHeader`), the entire
    /// table region is held as tail because adding a row can reshape table
    /// column widths. For `PendingHeader`, only content from the speculative
    /// header line onward is kept mutable so earlier prose can continue
    /// streaming. When no table is detected, everything flows directly to
    /// stable. Unclosed display math also stays mutable until its closing delimiter arrives.
    fn active_tail_budget_lines(&mut self) -> usize {
        if self.render_mode == HistoryRenderMode::Raw {
            return 0;
        }
        let scan_start = Instant::now();
        let holdback_state = self.holdback_scanner.state();
        let tail_budget = self
            .active_tail_source_start(self.render_mode)
            .map_or(0, |start| self.tail_budget_from_source_start(start));
        tracing::trace!(
            state = ?holdback_state,
            tail_budget,
            elapsed_us = scan_start.elapsed().as_micros(),
            "table holdback decision",
        );
        tail_budget
    }

    /// Convert a raw-source boundary into the number of rendered tail lines.
    ///
    /// The important contract here is that the holdback scanner reasons in
    /// byte offsets while the queue operates in rendered lines. This helper is
    /// the only place where those coordinate systems are bridged.
    fn tail_budget_from_source_start(&mut self, source_start: usize) -> usize {
        if source_start == 0 {
            return self.render.lines.len();
        }
        let source_start = source_start.min(self.state.collector.committed_source().len());
        let stable_prefix_len = self.stable_prefix_len_for_source_start(source_start);
        self.render.lines.len().saturating_sub(stable_prefix_len)
    }

    /// Render the stable prefix before `source_start` and return its line count.
    ///
    /// This value is cached because dense table streams can call into this path
    /// for every committed line while the header/delimiter/body are still
    /// arriving incrementally.
    fn stable_prefix_len_for_source_start(&mut self, source_start: usize) -> usize {
        if let Some(cache) = &self.stable_prefix_len_cache
            && cache.source_start == source_start
            && cache.width == self.width
        {
            tracing::trace!(
                source_start,
                width = ?self.width,
                stable_prefix_len = cache.stable_prefix_len,
                "table holdback stable-prefix cache hit",
            );
            return cache.stable_prefix_len;
        }

        let render_start = Instant::now();
        let source = self.state.collector.committed_source();
        let stable_prefix_render = render_markdown_agent_with_list_spacing(
            &source[..source_start.min(source.len())],
            self.width,
            Some(self.cwd.as_path()),
            self.inline_visualization_context.as_ref(),
            self.render.list_spacing,
        );
        let stable_prefix_len = stable_prefix_render.len();
        tracing::trace!(
            source_start,
            width = ?self.width,
            stable_prefix_len,
            elapsed_us = render_start.elapsed().as_micros(),
            "table holdback stable-prefix render",
        );
        self.stable_prefix_len_cache = Some(StablePrefixLenCache {
            source_start,
            width: self.width,
            stable_prefix_len,
        });
        stable_prefix_len
    }
}

/// Controller for streaming agent message content with table-aware holdback.
///
/// Wraps [`StreamCore`] and adds `AgentMessageCell` emission styling.
pub(crate) struct StreamController {
    core: StreamCore,
    header_emitted: bool,
}

impl StreamController {
    /// Select spacing before the first delta; final source-backed cells choose their own layout.
    pub(crate) fn with_list_spacing(mut self, list_spacing: ListSpacing) -> Self {
        self.core.render.list_spacing = list_spacing;
        self
    }

    /// Create a controller whose markdown renderer shortens local file links relative to `cwd`.
    ///
    /// `width` is the content width available to markdown rendering, not necessarily the full
    /// terminal width. Passing a stale width after resize will keep queued live output wrapped for
    /// the old viewport until app-level reflow repairs the finalized transcript.
    #[cfg(test)]
    pub(crate) fn new(width: Option<usize>, cwd: &Path, render_mode: HistoryRenderMode) -> Self {
        Self::new_with_inline_visualizations(
            width,
            cwd,
            render_mode,
            /*inline_visualization_context*/ None,
        )
    }

    pub(crate) fn new_with_inline_visualizations(
        width: Option<usize>,
        cwd: &Path,
        render_mode: HistoryRenderMode,
        inline_visualization_context: Option<InlineVisualizationContext>,
    ) -> Self {
        Self {
            core: StreamCore::new(width, cwd, render_mode, inline_visualization_context),
            header_emitted: false,
        }
    }

    pub(crate) fn push(&mut self, delta: &str) -> bool {
        self.core.push_delta(delta)
    }

    /// Finalize the active stream. Returns the final cell (if any remaining lines) and the raw
    /// markdown source for consolidation.
    pub(crate) fn finalize(&mut self) -> (Option<Box<dyn HistoryCell>>, Option<String>) {
        let (remaining, source) = self.core.finalize_remaining();
        if source.is_empty() {
            self.core.reset();
            return (None, None);
        }

        let out = self.emit(remaining);
        self.core.reset();
        (out, Some(source))
    }

    pub(crate) fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.core.tick();
        (self.emit(step), self.core.is_idle())
    }

    pub(crate) fn on_commit_tick_batch(
        &mut self,
        max_lines: usize,
    ) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.core.tick_batch(max_lines);
        (self.emit(step), self.core.is_idle())
    }

    // Thin StreamController accessors inlined — one-liner delegates called
    // on every render frame and animation tick.

    #[inline]
    pub(crate) fn queued_lines(&self) -> usize {
        self.core.queued_lines()
    }

    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.core.oldest_queued_age(now)
    }

    #[inline]
    pub(crate) fn current_tail_lines(&self) -> Vec<HyperlinkLine> {
        self.core.current_tail_lines()
    }

    #[inline]
    pub(crate) fn tail_starts_stream(&self) -> bool {
        !self.header_emitted && self.core.enqueued_stable_len == 0
    }

    #[inline]
    pub(crate) fn has_live_tail(&self) -> bool {
        self.core.has_tail()
    }

    /// Include the unterminated line when handing an in-flight answer to another view.
    pub(crate) fn source_snapshot(&self) -> String {
        let collector = &self.core.state.collector;
        format!(
            "{}{}",
            collector.committed_source(),
            collector.pending_source()
        )
    }

    /// Completion must repair emitted rows when reference definitions can change earlier blocks.
    pub(crate) fn needs_scrollback_reflow(&self) -> bool {
        if self.has_live_tail() || self.core.render.has_reference_link_definition {
            return true;
        }
        if self.core.render_mode == HistoryRenderMode::Raw
            || self.core.state.collector.pending_source().is_empty()
        {
            return false;
        }
        let source = self.source_snapshot();
        pulldown_cmark::Parser::new(&source)
            .reference_definitions()
            .iter()
            .next()
            .is_some()
    }

    pub(crate) fn clear_queue(&mut self) {
        self.core.state.clear_queue();
        self.core.enqueued_stable_len = self.core.emitted_stable_len;
    }

    pub(crate) fn set_width(&mut self, width: Option<usize>) {
        self.core.set_width(width);
    }

    pub(crate) fn set_render_mode(&mut self, render_mode: HistoryRenderMode) {
        self.core.set_render_mode(render_mode);
    }

    fn emit(&mut self, lines: Vec<HyperlinkLine>) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }
        Some(Box::new(
            history_cell::AgentMessageCell::new_hyperlink_lines(lines, {
                let header_emitted = self.header_emitted;
                self.header_emitted = true;
                !header_emitted
            }),
        ))
    }
}
// ---------------------------------------------------------------------------
// PlanStreamController — proposed plan streams
// ---------------------------------------------------------------------------

/// Controller that streams proposed plan markdown into a styled plan block.
///
/// Wraps [`StreamCore`] and adds plan-specific header, indentation, and
/// background styling.
pub(crate) struct PlanStreamController {
    core: StreamCore,
    header_emitted: bool,
    top_padding_emitted: bool,
}

impl PlanStreamController {
    /// Select spacing before the first delta; no list-specific holdback is needed.
    pub(crate) fn with_list_spacing(mut self, list_spacing: ListSpacing) -> Self {
        self.core.render.list_spacing = list_spacing;
        self
    }

    /// Create a plan-stream controller whose markdown renderer shortens local file links relative
    /// to `cwd`.
    ///
    /// The width has the same meaning as in `StreamController`: it is the markdown body width, and
    /// callers must update it when the terminal width changes.
    pub(crate) fn new(width: Option<usize>, cwd: &Path, render_mode: HistoryRenderMode) -> Self {
        Self {
            core: StreamCore::new(
                width,
                cwd,
                render_mode,
                /*inline_visualization_context*/ None,
            ),
            header_emitted: false,
            top_padding_emitted: false,
        }
    }

    pub(crate) fn push(&mut self, delta: &str) -> bool {
        self.core.push_delta(delta)
    }

    /// Finalize the active stream. Returns the final cell (if any remaining
    /// lines) plus raw markdown source for consolidation.
    pub(crate) fn finalize(&mut self) -> (Option<Box<dyn HistoryCell>>, Option<String>) {
        let (remaining, source) = self.core.finalize_remaining();
        if source.is_empty() {
            self.core.reset();
            return (None, None);
        }

        let out = self.emit(remaining, /*include_bottom_padding*/ true);
        self.core.reset();
        (out, Some(source))
    }

    pub(crate) fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.core.tick();
        (
            self.emit(step, /*include_bottom_padding*/ false),
            self.core.is_idle(),
        )
    }

    pub(crate) fn on_commit_tick_batch(
        &mut self,
        max_lines: usize,
    ) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.core.tick_batch(max_lines);
        (
            self.emit(step, /*include_bottom_padding*/ false),
            self.core.is_idle(),
        )
    }

    #[inline]
    pub(crate) fn queued_lines(&self) -> usize {
        self.core.queued_lines()
    }

    #[inline]
    pub(crate) fn has_live_tail(&self) -> bool {
        self.core.has_tail()
    }

    #[inline]
    pub(crate) fn current_tail_lines(&self) -> Vec<HyperlinkLine> {
        self.core.current_tail_lines()
    }

    #[inline]
    pub(crate) fn tail_starts_stream(&self) -> bool {
        !self.header_emitted && self.core.enqueued_stable_len == 0
    }

    pub(crate) fn current_tail_display_lines(&self) -> Vec<HyperlinkLine> {
        let lines = self.current_tail_lines();
        if lines.is_empty() {
            return Vec::new();
        }
        self.render_display_lines(lines, /*include_bottom_padding*/ false)
    }

    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.core.oldest_queued_age(now)
    }

    pub(crate) fn clear_queue(&mut self) {
        self.core.state.clear_queue();
        self.core.enqueued_stable_len = self.core.emitted_stable_len;
    }

    pub(crate) fn set_width(&mut self, width: Option<usize>) {
        self.core.set_width(width);
    }

    pub(crate) fn set_render_mode(&mut self, render_mode: HistoryRenderMode) {
        self.core.set_render_mode(render_mode);
    }

    fn emit(
        &mut self,
        lines: Vec<HyperlinkLine>,
        include_bottom_padding: bool,
    ) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() && !include_bottom_padding {
            return None;
        }

        let is_stream_continuation = self.header_emitted;
        let out_lines = self.render_display_lines(lines, include_bottom_padding);
        self.header_emitted = true;
        self.top_padding_emitted = true;

        Some(Box::new(history_cell::new_proposed_plan_stream(
            out_lines,
            is_stream_continuation,
        )))
    }

    fn render_display_lines(
        &self,
        lines: Vec<HyperlinkLine>,
        include_bottom_padding: bool,
    ) -> Vec<HyperlinkLine> {
        let mut out_lines: Vec<HyperlinkLine> = Vec::with_capacity(/*capacity*/ 4);
        if !self.header_emitted {
            out_lines.push(HyperlinkLine::new(
                vec!["• ".dim(), "Proposed Plan".bold()].into(),
            ));
            out_lines.push(HyperlinkLine::new(Line::from(" ")));
        }

        let mut plan_lines: Vec<HyperlinkLine> = Vec::with_capacity(/*capacity*/ 4);
        if !self.top_padding_emitted {
            plan_lines.push(HyperlinkLine::new(Line::from(" ")));
        }
        plan_lines.extend(lines);
        if include_bottom_padding {
            plan_lines.push(HyperlinkLine::new(Line::from(" ")));
        }

        let plan_style = proposed_plan_style();
        let plan_lines = prefix_hyperlink_lines(plan_lines, "  ".into(), "  ".into())
            .into_iter()
            .map(|line| history_cell::style_proposed_plan_line(line, plan_style))
            .collect::<Vec<_>>();
        out_lines.extend(plan_lines);
        out_lines
    }
}

#[cfg(test)]
#[path = "math_tests.rs"]
mod math_tests;

#[cfg(test)]
#[path = "controller_preview_tests.rs"]
mod preview_tests;

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rendering_preferences_tests.rs"]
mod rendering_preferences_tests;
