//! Keeps the command, file-search, and mention popups in sync with the token under the cursor.

use super::*;

impl ChatComposer {
    pub(crate) fn sync_popups(&mut self) {
        self.sync_slash_command_elements();
        if self.history_search.is_some() || self.draft.textarea.vim_query().is_some() {
            self.cancel_file_search();
            self.popups.active = ActivePopup::None;
            self.popups.dismissed_file_token = None;
            self.popups.dismissed_mention_token = None;
            return;
        }
        if !self.popups_enabled() || self.draft.textarea.mouse_selection_range().is_some() {
            self.popups.active = ActivePopup::None;
            return;
        }
        let mut mentions_v2_token = self.current_mentions_v2_token_range();
        let mut file_token = if self.mentions_v2_enabled {
            None
        } else {
            self.current_editable_at_token_range_with_options(/*allow_empty*/ false)
        };
        let browsing_history = self
            .history
            .should_handle_navigation(&self.current_text(), self.history_navigation_cursor());
        // When browsing input history (shell-style Up/Down recall), skip all popup
        // synchronization so nothing steals focus from continued history navigation.
        if browsing_history {
            self.cancel_file_search();
            self.popups.active = ActivePopup::None;
            return;
        }
        let mut mention_target = self.current_mention_target();
        let at_token_start = mentions_v2_token
            .as_ref()
            .or(file_token.as_ref())
            .map(|(range, _)| range.start);
        let mention_token_start = mention_target.as_ref().map(|target| target.range.start);
        if let (Some(at_token_start), Some(mention_token_start)) =
            (at_token_start, mention_token_start)
        {
            if at_token_start > mention_token_start {
                mention_target = None;
            } else if mention_token_start > at_token_start {
                mentions_v2_token = None;
                file_token = None;
            }
        }

        let allow_command_popup = self.slash_commands_enabled()
            && !self.draft.is_bash_mode
            && file_token.is_none()
            && mentions_v2_token.is_none()
            && mention_target.is_none();
        self.sync_command_popup(allow_command_popup);

        if matches!(self.popups.active, ActivePopup::Command(_)) {
            self.cancel_file_search();
            self.popups.dismissed_file_token = None;
            self.popups.dismissed_mention_token = None;
            return;
        }

        if let Some((range, token)) = mentions_v2_token {
            self.sync_mentions_v2_popup(range, token);
            return;
        }

        if let Some(target) = mention_target {
            self.cancel_file_search();
            self.sync_mention_popup(target);
            return;
        }
        self.popups.dismissed_mention_token = None;

        if let Some((range, token)) = file_token {
            self.sync_file_search_popup(range, token);
            return;
        }

        self.cancel_file_search();
        self.popups.dismissed_file_token = None;
        if matches!(
            self.popups.active,
            ActivePopup::File(_) | ActivePopup::Skill(_) | ActivePopup::MentionV2(_)
        ) {
            self.popups.active = ActivePopup::None;
        }
    }

    /// Synchronize `self.command_popup` with the current text in the
    /// textarea. This must be called after every modification that can change
    /// the text so the popup is shown/updated/hidden as appropriate.
    fn sync_command_popup(&mut self, allow: bool) {
        let text = self.draft.textarea.text();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        // Keep an explicitly dismissed popup closed until the command token changes.
        let command_token = slash_input::command_popup_filter_text(first_line, /*cursor*/ 0);
        if let Some(command_token) = command_token.as_deref()
            && self.popups.dismissed_command_token.as_deref() == Some(command_token)
        {
            return;
        }
        self.popups.dismissed_command_token = None;

        if !allow {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        // Determine whether the caret is inside the initial '/name' token on the first line.
        let cursor = self.draft.textarea.cursor();
        let caret_on_first_line = cursor <= first_line_end;

        let is_editing_slash_command_name = caret_on_first_line
            && self
                .slash_input()
                .is_editing_command_name(first_line, cursor);
        let command_filter_text = caret_on_first_line
            .then(|| slash_input::command_popup_filter_text(first_line, cursor))
            .flatten();

        // If the cursor is currently positioned within an `@token`, prefer the
        // file-search popup over the slash popup so users can insert a file path
        // as an argument to the command (e.g., "/review @docs/...").
        if Self::current_at_token(&self.draft.textarea).is_some() {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        match &mut self.popups.active {
            ActivePopup::Command(popup) => {
                if is_editing_slash_command_name {
                    if let Some(command_filter_text) = command_filter_text.as_deref() {
                        popup.on_composer_text_change(command_filter_text.to_string());
                    }
                } else {
                    self.popups.active = ActivePopup::None;
                }
            }
            _ => {
                if is_editing_slash_command_name
                    && let Some(command_filter_text) = command_filter_text.as_deref()
                {
                    let command_popup = self.slash_input().command_popup(command_filter_text);
                    self.popups.active = ActivePopup::Command(command_popup);
                }
            }
        }
    }

    /// Synchronize the legacy file-search popup with the current `@` token.
    fn sync_file_search_popup(&mut self, range: Range<usize>, query: String) {
        if self
            .popups
            .dismissed_file_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        self.app_event_tx
            .send(AppEvent::StartFileSearch(query.clone()));
        if !matches!(self.popups.active, ActivePopup::File(_)) {
            self.popups.active = ActivePopup::File(FileSearchPopup::new());
        }
        if let ActivePopup::File(popup) = &mut self.popups.active {
            if query.is_empty() {
                popup.set_empty_prompt();
            } else {
                popup.set_query(&query);
            }
        }
        self.popups.current_file_query = (!query.is_empty()).then_some(query);
        self.popups.dismissed_file_token = None;
    }

    /// Ends the active `@` file search, if any; the app drops its search session on an empty query.
    pub(super) fn cancel_file_search(&mut self) {
        if self.popups.current_file_query.take().is_some() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
        }
    }

    fn sync_mention_popup(&mut self, target: MentionCompletionTarget) {
        let MentionCompletionTarget {
            range,
            query,
            prebuilt_mentions,
        } = target;
        if self
            .popups
            .dismissed_mention_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        let mentions = prebuilt_mentions.unwrap_or_else(|| self.mention_items());
        if mentions.is_empty() {
            self.popups.active = ActivePopup::None;
            return;
        }

        match &mut self.popups.active {
            ActivePopup::Skill(popup) => {
                popup.set_query(&query);
                popup.set_mentions(mentions);
            }
            _ => {
                let mut popup =
                    SkillPopup::new(mentions).with_frequencies(self.mention_usage.frequencies());
                popup.set_query(&query);
                self.popups.active = ActivePopup::Skill(popup);
            }
        }
    }

    fn sync_mentions_v2_popup(&mut self, range: Range<usize>, query: String) {
        if self
            .popups
            .dismissed_mention_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        if query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        } else {
            let new_popup = !matches!(self.popups.active, ActivePopup::MentionV2(_));
            if new_popup {
                // A fresh popup has no cached matches, and the app-owned file-search manager can
                // retain an identical query. Reset it before issuing the query so results arrive.
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
            }
            if new_popup || self.popups.current_file_query.as_deref() != Some(query.as_str()) {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(query.clone()));
                self.popups.current_file_query = Some(query.clone());
            }
        }

        match &mut self.popups.active {
            ActivePopup::MentionV2(popup) => {
                popup.set_query(&query);
            }
            _ => {
                let candidates = super::mentions_v2::build_search_catalog(
                    self.skills.as_deref(),
                    self.plugins.as_deref(),
                    self.task_mentions.as_deref().unwrap_or_default(),
                );
                self.popups.active =
                    ActivePopup::MentionV2(MentionV2Popup::new(candidates, &query));
            }
        }

        self.popups.dismissed_mention_token = None;
    }

    pub(super) fn mention_items(&self) -> Vec<MentionItem> {
        let mut mentions = Vec::new();
        if let Some(skills) = self.skills.as_ref() {
            for skill in skills {
                let display_name = skill_display_name(skill);
                let description = skill_description(skill);
                let skill_name = skill.name.clone();
                let search_terms = if display_name == skill.name {
                    vec![skill_name.clone()]
                } else {
                    vec![skill_name.clone(), display_name.clone()]
                };
                mentions.push(MentionItem {
                    display_name,
                    description,
                    insert_text: format!("${skill_name}"),
                    search_terms,
                    path: Some(skill.path.as_str().to_owned()),
                    category_tag: Some("[Skill]".to_string()),
                    sort_rank: 1,
                });
            }
        }

        if !self.mentions_v2_enabled
            && let Some(plugins) = self.plugins.as_ref()
        {
            for plugin in plugins {
                let (plugin_name, marketplace_name) = plugin
                    .config_name
                    .split_once('@')
                    .unwrap_or((plugin.config_name.as_str(), ""));
                let mut capability_labels = Vec::new();
                if plugin.has_skills {
                    capability_labels.push("skills".to_string());
                }
                if !plugin.mcp_server_names.is_empty() {
                    let mcp_server_count = plugin.mcp_server_names.len();
                    capability_labels.push(if mcp_server_count == 1 {
                        "1 MCP server".to_string()
                    } else {
                        format!("{mcp_server_count} MCP servers")
                    });
                }
                if !plugin.app_connector_ids.is_empty() {
                    let app_count = plugin.app_connector_ids.len();
                    capability_labels.push(if app_count == 1 {
                        "1 app".to_string()
                    } else {
                        format!("{app_count} apps")
                    });
                }
                let description = plugin.description.clone().or_else(|| {
                    Some(if capability_labels.is_empty() {
                        "Plugin".to_string()
                    } else {
                        format!("Plugin · {}", capability_labels.join(" · "))
                    })
                });
                let mut search_terms = vec![plugin_name.to_string(), plugin.config_name.clone()];
                if plugin.display_name != plugin_name {
                    search_terms.push(plugin.display_name.clone());
                }
                if !marketplace_name.is_empty() {
                    search_terms.push(marketplace_name.to_string());
                }
                mentions.push(MentionItem {
                    display_name: plugin.display_name.clone(),
                    description,
                    insert_text: format!("${plugin_name}"),
                    search_terms,
                    path: Some(format!("plugin://{}", plugin.config_name)),
                    category_tag: Some("[Plugin]".to_string()),
                    sort_rank: 0,
                });
            }
        }

        if self.connectors_enabled
            && let Some(snapshot) = self.connectors_snapshot.as_ref()
        {
            for connector in &snapshot.connectors {
                if !connector.is_accessible || !connector.is_enabled {
                    continue;
                }
                let display_name = codex_connectors::metadata::connector_display_label(connector);
                let description = Some(Self::connector_brief_description(connector));
                let slug = codex_connectors::metadata::connector_mention_slug(connector);
                let search_terms = vec![display_name.clone(), connector.id.clone(), slug.clone()];
                let connector_id = connector.id.as_str();
                mentions.push(MentionItem {
                    display_name: display_name.clone(),
                    description,
                    insert_text: format!("${slug}"),
                    search_terms,
                    path: Some(format!("app://{connector_id}")),
                    category_tag: Some("[App]".to_string()),
                    sort_rank: 1,
                });
            }
        }

        mentions
    }

    fn connector_brief_description(connector: &AppInfo) -> String {
        Self::connector_description(connector).unwrap_or_default()
    }

    fn connector_description(connector: &AppInfo) -> Option<String> {
        connector
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string)
    }
}

fn skill_description(skill: &SkillMetadata) -> Option<String> {
    let description = skill
        .interface
        .as_ref()
        .and_then(|interface| interface.short_description.as_deref())
        .or(skill.short_description.as_deref())
        .unwrap_or(&skill.description);
    let trimmed = description.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
