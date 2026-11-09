//! Applies search queries and cwd/repository filters to the loaded picker rows.

use super::*;

impl PickerState {
    pub(super) fn apply_filter(&mut self) {
        self.chord_matcher.cancel();
        let base_iter = self
            .all_rows
            .iter()
            .filter(|row| self.row_matches_filter(row));
        if self.query.is_empty() {
            self.filtered_rows = base_iter.cloned().collect();
        } else {
            let q = self.query.to_lowercase();
            self.filtered_rows = base_iter.filter(|r| r.matches_query(&q)).cloned().collect();
        }
        if self.selected >= self.filtered_rows.len() {
            self.selected = self.filtered_rows.len().saturating_sub(1);
        }
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
        }
        self.ensure_selected_visible();
        self.request_frame();
    }

    pub(super) fn row_matches_filter(&self, row: &Row) -> bool {
        self.filter_mode == SessionFilterMode::All
            || self.local_filter_cwd.is_none()
            || row
                .cwd
                .as_ref()
                .and_then(|cwd| self.local_cwd_matches.get(cwd))
                .copied()
                .unwrap_or(false)
    }

    pub(super) fn row_matches_local_cwd(&self, row: &Row) -> bool {
        let Some(filter_cwd) = self.local_filter_cwd.as_ref() else {
            return true;
        };
        let Some(row_cwd) = row.cwd.as_ref() else {
            return false;
        };
        paths_match(row_cwd, filter_cwd)
            || (self.filter_mode == SessionFilterMode::Repo
                && codex_git_utils::repository_identity(row_cwd)
                    .zip(codex_git_utils::repository_identity(filter_cwd))
                    .is_some_and(|(row, filter)| {
                        row.common_dir == filter.common_dir
                            && row.relative_cwd == filter.relative_cwd
                    }))
    }

    pub(super) fn set_query(&mut self, new_query: String) {
        if self.query == new_query {
            return;
        }
        self.query = new_query;
        self.selected = 0;
        self.apply_filter();
        if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_cap || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        let token = self.allocate_search_token();
        self.search_state = SearchState::Active { token };
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    pub(super) fn clear_query_preserving_selection(&mut self) {
        let selected_key = self
            .filtered_rows
            .get(self.selected)
            .and_then(Row::seen_key);
        self.query.clear();
        self.search_state = SearchState::Idle;
        self.apply_filter();
        if let Some(selected_key) = selected_key
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.seen_key().as_ref() == Some(&selected_key))
        {
            self.selected = index;
            self.ensure_selected_visible();
            self.request_frame();
        }
    }

    pub(super) fn continue_search_if_needed(&mut self) {
        let Some(token) = self.search_state.active_token() else {
            return;
        };
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_cap || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    pub(super) fn continue_search_if_token_matches(&mut self, completed_token: Option<usize>) {
        let Some(active) = self.search_state.active_token() else {
            return;
        };
        if let Some(token) = completed_token
            && token != active
        {
            return;
        }
        self.continue_search_if_needed();
    }
}

pub(crate) fn repository_cwd_filter(
    cwd: &Path,
    uses_remote_filesystem: bool,
) -> ThreadListCwdFilter {
    if !uses_remote_filesystem
        && let Some(cwds) = codex_git_utils::linked_worktree_cwds(cwd)
        && cwds.len() > 1
    {
        return ThreadListCwdFilter::Many(
            cwds.into_iter()
                .map(|cwd| cwd.to_string_lossy().into_owned())
                .collect(),
        );
    }
    ThreadListCwdFilter::One(cwd.to_string_lossy().into_owned())
}

pub(super) fn local_repo_filter_available(cwd: &Path) -> bool {
    codex_git_utils::linked_worktree_cwds(cwd).is_some_and(|cwds| {
        cwds.into_iter()
            .filter_map(|cwd| dunce::canonicalize(cwd).ok())
            .collect::<HashSet<_>>()
            .len()
            > 1
    })
}

pub(super) fn paths_match(a: &Path, b: &Path) -> bool {
    path_utils::paths_match_after_normalization(a, b)
}
