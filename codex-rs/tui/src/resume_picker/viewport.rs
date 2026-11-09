//! Keeps the selection visible and measures rendered row heights for scrolling.

use super::*;

impl PickerState {
    pub(super) fn ensure_selected_visible(&mut self) {
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
            return;
        }
        let viewport_rows = self.view_rows.unwrap_or(usize::MAX).max(1);
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        }
        while self.rendered_height_between(self.scroll_top, self.selected)
            > self.available_content_rows(viewport_rows)
            && self.scroll_top < self.selected
        {
            self.scroll_top += 1;
        }
    }

    pub(super) fn ensure_minimum_rows_for_view(&mut self, minimum_rows: usize) {
        if minimum_rows == 0 {
            return;
        }
        if self.pagination.is_loading() || self.pagination.next_cursor.is_none() {
            return;
        }
        let rendered_rows = if self.filtered_rows.is_empty() {
            0
        } else {
            self.rendered_height_between(/*start*/ 0, self.filtered_rows.len() - 1)
        };
        if rendered_rows >= self.available_content_rows(minimum_rows) {
            return;
        }
        if let Some(token) = self.search_state.active_token() {
            self.load_more_if_needed(LoadTrigger::Search { token });
        } else {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    pub(super) fn update_viewport(&mut self, rows: usize, width: u16) {
        self.view_rows = if rows == 0 { None } else { Some(rows) };
        self.view_width = Some(width);
        self.ensure_selected_visible();
    }

    pub(super) fn rendered_height_between(&self, start: usize, end_inclusive: usize) -> usize {
        self.filtered_rows
            .get(start..=end_inclusive)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let row_idx = start + offset;
                let is_selected = row_idx == self.selected;
                let is_expanded = is_selected
                    && row.thread_id.is_some()
                    && self.expanded_thread_id == row.thread_id;
                render_session_lines(
                    row,
                    self,
                    is_selected,
                    is_expanded,
                    /*is_zebra*/ false,
                    self.view_width.unwrap_or(u16::MAX),
                )
                .len()
            })
            .sum::<usize>()
            + self.row_separator_height() * end_inclusive.saturating_sub(start)
    }

    pub(super) fn has_more_above(&self) -> bool {
        self.scroll_top > 0
    }

    pub(super) fn has_more_below(&self, viewport_height: usize) -> bool {
        if self.filtered_rows.is_empty() {
            return false;
        }
        if self.pagination.next_cursor.is_some() {
            return true;
        }
        let capacity = self.available_content_rows(viewport_height);
        let mut used = 0usize;
        for (offset, row) in self.filtered_rows[self.scroll_top..].iter().enumerate() {
            let row_idx = self.scroll_top + offset;
            let is_selected = row_idx == self.selected;
            let is_expanded =
                is_selected && row.thread_id.is_some() && self.expanded_thread_id == row.thread_id;
            let row_height = render_session_lines(
                row,
                self,
                is_selected,
                is_expanded,
                /*is_zebra*/ false,
                self.view_width.unwrap_or(u16::MAX),
            )
            .len();
            let separator_height = usize::from(offset > 0) * self.row_separator_height();
            if used + separator_height + row_height > capacity {
                return true;
            }
            used += separator_height + row_height;
        }
        false
    }

    pub(super) fn available_content_rows(&self, viewport_height: usize) -> usize {
        viewport_height
            .saturating_sub(usize::from(self.has_more_above()))
            .saturating_sub(usize::from(
                self.pagination.next_cursor.is_some()
                    || self.selected + 1 < self.filtered_rows.len(),
            ))
            .max(1)
    }

    fn row_separator_height(&self) -> usize {
        match self.density {
            SessionListDensity::Comfortable => 1,
            SessionListDensity::Dense => 0,
        }
    }
}
