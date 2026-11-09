//! Opens full transcript overlays and inline transcript previews for the selected session.

use super::*;

impl PickerState {
    pub(super) fn is_transcript_loading(&self) -> bool {
        self.pending_transcript_open.is_some()
    }

    pub(super) fn note_transcript_loading_frame_drawn(&mut self) -> bool {
        if self.pending_transcript_open.is_some() {
            self.transcript_loading_frame_shown = true;
            true
        } else {
            false
        }
    }

    pub(super) fn open_pending_transcript_if_ready(&mut self) {
        if !self.transcript_loading_frame_shown {
            return;
        }
        let Some((backend_id, thread_id)) = self.pending_transcript_open else {
            return;
        };
        let Some(SessionTranscriptState::Loaded(cells)) =
            self.transcript_cells.get(&(backend_id, thread_id))
        else {
            return;
        };
        let mut overlay = Overlay::new_transcript(
            cells.cells.clone(),
            self.keymap.pager.clone(),
            self.copy_on_select,
            self.mouse_scroll_speed,
        )
        .with_copy_on_select_clear_selection(self.copy_on_select_clear_selection);
        if let Overlay::Transcript(view) = &mut overlay {
            view.set_keymap_bindings(&self.keymap);
        }
        self.chord_matcher.cancel();
        self.overlay_receipt =
            cells
                .unread_revision
                .clone()
                .map(|revision| PickerLoadRequest::Acknowledge {
                    backend_id,
                    thread_id,
                    revision,
                });
        self.overlay = Some(overlay);
        self.pending_transcript_open = None;
        self.transcript_loading_frame_shown = false;
        self.request_frame();
    }

    pub(super) fn handle_overlay_event(&mut self, tui: &mut Tui, event: TuiEvent) -> Result<()> {
        let Some(overlay) = &mut self.overlay else {
            return Ok(());
        };
        let draw = matches!(
            event,
            TuiEvent::Draw | TuiEvent::FocusGained | TuiEvent::Resume | TuiEvent::Resize(_)
        );
        overlay.handle_event(tui, event)?;
        if draw
            && tui.is_terminal_focused()
            && let Some(receipt) = self.overlay_receipt.take()
        {
            (self.picker_loader)(receipt);
        }
        if overlay.is_done() {
            self.overlay = None;
            self.request_frame();
        }
        Ok(())
    }

    pub(super) fn update_unread(&mut self, key: (BackendId, ThreadId), revision: Option<String>) {
        if let Some(revision) = revision {
            self.unread.insert(key, revision);
        } else {
            self.unread.remove(&key);
        }
    }

    pub(super) fn unread_revision(&self, row: &Row) -> Option<&String> {
        self.unread
            .get(&(row.backend_id.unwrap_or(self.backend_id), row.thread_id?))
    }

    pub(super) fn open_selected_transcript(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            self.inline_error = Some("No transcript available for this session".to_string());
            self.request_frame();
            return;
        };
        let backend_id = row.backend_id.unwrap_or(self.backend_id);
        let key = (backend_id, thread_id);
        self.pending_transcript_open = Some(key);
        self.transcript_loading_frame_shown = false;
        self.request_frame();
        match self.transcript_cells.get(&key) {
            Some(SessionTranscriptState::Loaded(transcript))
                if self.unread.get(&key) == transcript.unread_revision.as_ref() => {}
            Some(SessionTranscriptState::Loading) => {}
            _ => {
                self.transcript_cells
                    .insert(key, SessionTranscriptState::Loading);
                let (cancellation_tx, cancellation) = oneshot::channel();
                self.pending_transcript_cancellation = Some(cancellation_tx);
                (self.picker_loader)(PickerLoadRequest::Transcript {
                    backend_id,
                    thread_id,
                    cancellation,
                });
            }
        }
    }

    pub(super) fn toggle_selected_expansion(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            return;
        };
        let backend_id = row.backend_id.unwrap_or(self.backend_id);
        if self.expanded_thread_id == Some(thread_id) {
            self.expanded_thread_id = None;
            self.request_frame();
            return;
        }
        self.expanded_thread_id = Some(thread_id);
        if let std::collections::hash_map::Entry::Vacant(e) =
            self.transcript_previews.entry(thread_id)
        {
            e.insert(TranscriptPreviewState::Loading);
            (self.picker_loader)(PickerLoadRequest::Preview {
                backend_id,
                thread_id,
            });
        }
        self.request_frame();
    }
}
