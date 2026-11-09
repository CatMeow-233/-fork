//! Feature toggles, catalogs, keymaps, and footer/status setters that configure the composer.

use super::*;

impl ChatComposer {
    pub(crate) fn set_frame_requester(&mut self, frame_requester: FrameRequester) {
        self.frame_requester = Some(frame_requester);
    }

    /// Records the effective reasoning tier, captures the outgoing status
    /// line, and queues the one-shot effects for a genuine Max/Ultra change
    /// after the initial baseline.
    pub(crate) fn set_active_reasoning_effort(
        &mut self,
        effort: Option<&ReasoningEffort>,
        animations_enabled: bool,
    ) -> bool {
        let tier = EffortTier::from_effort(effort);
        let is_baseline = !self.effort_observed;
        self.effort_observed = true;
        if self.effort_tier == tier {
            return false;
        }
        self.effort_tier = tier;
        self.effort_ignition = None;
        self.effort_status_line_transition = None;
        if let Some(tier) = tier
            && !is_baseline
            && animations_enabled
        {
            let style = IgnitionStyle::random(self.effort_animation_style);
            self.effort_ignition = Some(EffortIgnition::new(tier, style));
            self.effort_animation_style = Some(style);
            if self.footer.status_line_enabled
                && let Some(FooterContent::HyperlinkLine(previous)) =
                    passive_footer_status_line(&self.footer_props())
            {
                self.effort_status_line_transition =
                    Some(EffortStatusLineTransition::new(tier, previous));
            }
            if let Some(frame_requester) = &self.frame_requester {
                frame_requester.schedule_frame();
            }
        }
        true
    }

    /// Establishes the current tier without retaining or starting a one-shot effect.
    pub(crate) fn set_active_reasoning_effort_baseline(
        &mut self,
        effort: Option<&ReasoningEffort>,
    ) {
        self.effort_tier = EffortTier::from_effort(effort);
        self.effort_observed = true;
        self.effort_ignition = None;
        self.effort_status_line_transition = None;
    }

    pub fn set_skill_mentions(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.skills = skills;
        self.refresh_mentions_v2_popup_candidates();
        self.sync_popups();
    }

    pub fn set_plugin_mentions(&mut self, plugins: Option<Vec<PluginCapabilitySummary>>) {
        self.plugins = plugins;
        self.refresh_mentions_v2_popup_candidates();
        self.sync_popups();
    }

    pub(crate) fn set_task_mentions_enabled(&mut self, enabled: bool) {
        self.task_mentions = enabled.then(Vec::new);
        self.refresh_mentions_v2_popup_candidates();
        if enabled
            && matches!(self.popups.active, ActivePopup::MentionV2(_))
            && let Some(query) = self.current_mentions_v2_token()
            && !query.is_empty()
        {
            self.app_event_tx.send(AppEvent::StartFileSearch(query));
        }
    }

    pub(crate) fn task_mentions_enabled(&self) -> bool {
        self.task_mentions.is_some()
    }

    pub(crate) fn on_task_search_result(
        &mut self,
        query: &str,
        mut matches: Vec<crate::task_mentions::TaskMention>,
    ) {
        if self.current_mentions_v2_token().as_deref() != Some(query) {
            return;
        }
        let mentioned_ids = self
            .current_mention_elements()
            .into_iter()
            .filter_map(|(id, _, _)| self.draft.mention_bindings.get(&id))
            .filter_map(|binding| crate::task_mentions::valid_thread_path(&binding.path))
            .collect::<std::collections::HashSet<_>>();
        matches.retain(|task| {
            mentioned_ids.len() < crate::task_mentions::MAX_REFERENCED_TASKS
                && !mentioned_ids.contains(task.thread_id.as_str())
        });
        let Some(task_mentions) = &mut self.task_mentions else {
            return;
        };
        *task_mentions = matches;
        self.refresh_mentions_v2_popup_candidates();
    }

    /// Refreshes an open mention catalog when skill or plugin metadata changes.
    fn refresh_mentions_v2_popup_candidates(&mut self) {
        let ActivePopup::MentionV2(popup) = &mut self.popups.active else {
            return;
        };
        popup.set_candidates(super::mentions_v2::build_search_catalog(
            self.skills.as_deref(),
            self.plugins.as_deref(),
            self.task_mentions.as_deref().unwrap_or_default(),
        ));
    }

    pub fn set_plugins_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.plugins_command_enabled = enabled);
    }

    pub fn set_token_activity_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.token_activity_command_enabled = enabled
        });
    }

    pub fn set_mentions_v2_enabled(&mut self, enabled: bool) {
        self.mentions_v2_enabled = enabled;
        self.history.set_at_mention_restore_enabled(enabled);
        self.sync_popups();
    }

    /// Toggle composer-side image paste handling.
    ///
    /// This only affects whether image-like paste content is converted into attachments; the
    /// `ChatWidget` layer still performs capability checks before images are submitted.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.config.image_paste_enabled = enabled;
    }

    pub fn set_connector_mentions(&mut self, connectors_snapshot: Option<ConnectorsSnapshot>) {
        self.connectors_snapshot = connectors_snapshot;
        self.sync_popups();
    }

    pub(crate) fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        let elements = self.current_mention_elements();
        let mut ordered = Vec::new();
        for (id, sigil, mention) in elements {
            if let Some(binding) = self.draft.mention_bindings.remove(&id)
                && binding.sigil == sigil
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    sigil: binding.sigil,
                    mention: binding.mention,
                    path: binding.path,
                });
            }
        }
        self.draft.mention_bindings.clear();
        ordered
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.collaboration_modes_enabled = enabled
        });
    }

    pub fn set_connectors_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.connectors_enabled = enabled);
    }

    pub fn set_service_tier_commands_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.service_tier_commands_enabled = enabled
        });
    }

    pub fn set_service_tier_commands(&mut self, commands: Vec<ServiceTierCommand>) {
        self.update_slash_command_catalog(|composer| composer.service_tier_commands = commands);
    }

    pub fn set_goal_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.goal_command_enabled = enabled);
    }

    pub fn set_voice_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.voice_command_enabled = enabled);
    }

    pub(super) fn update_slash_command_catalog(&mut self, update: impl FnOnce(&mut Self)) {
        update(self);
        self.sync_popups();
    }

    /// Refresh the event destination when a retained editor survives reconnection.
    #[allow(dead_code, reason = "Used by later layers of the TUI refresh stack.")]
    pub(crate) fn set_app_event_sender(&mut self, app_event_tx: AppEventSender) {
        self.app_event_tx = app_event_tx;
    }

    /// Replace composer, editor, and footer-hint key bindings from one runtime snapshot.
    ///
    /// Submit and queue bindings are cached here because composer dispatch must
    /// check them before generic textarea editing. The embedded textarea receives
    /// the same snapshot's editor bindings so a live remap cannot leave submit
    /// keys updated while cursor/editing keys still use old defaults.
    pub(crate) fn set_keymap_bindings(&mut self, keymap: &RuntimeKeymap) {
        self.submit_keys = keymap.composer.submit.clone();
        self.queue_keys = keymap.composer.queue.clone();
        self.toggle_shortcuts_keys = keymap.composer.toggle_shortcuts.clone();
        self.history_search_previous_keys = keymap.composer.history_search_previous.clone();
        self.history_search_next_keys = keymap.composer.history_search_next.clone();
        self.editor_keymap = keymap.editor.clone();
        self.vim_normal_keymap = keymap.vim_normal.clone();
        self.draft.textarea.set_keymap_bindings(keymap);
        self.footer.external_editor_key =
            keymap.primary_hint(KeymapContext::Global, "open_external_editor");
        self.footer.show_warnings_key = keymap.primary_hint(KeymapContext::Global, "open_warnings");
        self.footer.show_transcript_key =
            keymap.primary_hint(KeymapContext::Global, "open_transcript");
        self.footer.find_transcript_key =
            keymap.primary_hint(KeymapContext::Global, "find_transcript");
        self.footer.focus_activity_key =
            keymap.primary_hint(KeymapContext::Global, "focus_activity");
        self.footer.insert_newline_key =
            match keymap.primary_hint(KeymapContext::Editor, "insert_newline") {
                hint @ Some(ShortcutHint::Chord { .. }) => hint,
                _ => footer_insert_newline_key(
                    &keymap.editor.insert_newline,
                    self.footer.use_shift_enter_hint,
                )
                .map(ShortcutHint::from),
            };
        self.footer.queue_key = keymap.primary_hint(KeymapContext::Composer, "queue");
        self.footer.toggle_shortcuts_key =
            keymap.primary_hint(KeymapContext::Composer, "toggle_shortcuts");
        self.footer.history_search_key =
            keymap.primary_hint(KeymapContext::Composer, "history_search_previous");
        self.footer.reasoning_down_key =
            keymap.primary_hint(KeymapContext::Chat, "decrease_reasoning_effort");
        self.footer.reasoning_up_key =
            keymap.primary_hint(KeymapContext::Chat, "increase_reasoning_effort");
        self.footer.toggle_voice_key = keymap.primary_hint(KeymapContext::Chat, "toggle_voice");
    }

    /// Return the contexts whose handlers can consume the next composer key.
    pub(crate) fn keymap_contexts(&self) -> KeymapContextSet {
        if self.draft.textarea.vim_query().is_some() {
            return KeymapContextSet::new(KeymapContext::Editor);
        }
        if self.history_search.is_some() {
            return KeymapContextSet::new(KeymapContext::Composer);
        }
        if !self.draft.input_enabled || self.popups.active() {
            return KeymapContextSet::default();
        }
        let contexts = self.draft.textarea.keymap_contexts();
        contexts.with(KeymapContext::Composer)
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.footer.collaboration_mode_indicator = indicator;
    }

    pub fn set_goal_status_indicator(&mut self, indicator: Option<GoalStatusIndicator>) {
        self.footer.goal_status_indicator = indicator;
    }

    pub fn set_ide_context_active(&mut self, active: bool) {
        self.footer.ide_context_active = active;
    }

    pub fn set_side_conversation_active(&mut self, active: bool) {
        self.update_slash_command_catalog(|composer| composer.side_conversation_active = active);
    }

    /// Compatibility shim for tests that still toggle the removed steer mode flag.
    #[cfg(test)]
    pub fn set_steer_enabled(&mut self, _enabled: bool) {}
    /// Centralized feature gating keeps config checks out of call sites.
    pub(super) fn popups_enabled(&self) -> bool {
        self.config.popups_enabled
    }

    pub(super) fn slash_commands_enabled(&self) -> bool {
        self.config.slash_commands_enabled
    }

    pub(super) fn image_paste_enabled(&self) -> bool {
        self.config.image_paste_enabled
    }
    #[cfg(target_os = "windows")]
    pub fn set_windows_degraded_sandbox_active(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.windows_degraded_sandbox_active = enabled
        });
    }

    pub(super) fn set_has_focus(&mut self, has_focus: bool) {
        self.has_focus = has_focus;
    }

    #[allow(dead_code)]
    pub(crate) fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.draft.input_enabled = enabled;
        self.draft.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && self.popups.active() {
            self.popups.active = ActivePopup::None;
        }
    }

    pub(crate) fn show_shutdown_in_progress(&mut self) {
        self.set_input_enabled(/*enabled*/ false, Some("Shutting down...".to_string()));
        self.footer.quit_shortcut_expires_at = None;
        self.footer.mode = FooterMode::ComposerEmpty;
        self.footer.hint_override = Some(Vec::new());
        self.footer.flash = None;
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.is_task_running = running;
    }

    pub(crate) fn set_queue_submissions(&mut self, queue_submissions: bool) {
        self.queue_submissions = queue_submissions;
    }

    pub(crate) fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.footer.context_window_percent == percent
            && self.footer.context_window_used_tokens == used_tokens
        {
            return;
        }
        self.footer.context_window_percent = percent;
        self.footer.context_window_used_tokens = used_tokens;
    }

    pub(crate) fn set_context_window_pending(&mut self, pending: bool) {
        self.footer.context_window_pending = pending;
    }

    pub(crate) fn set_esc_backtrack_hint(&mut self, show: bool) {
        self.footer.esc_backtrack_hint = show;
        if show {
            self.footer.mode = esc_hint_mode(self.footer.mode, self.is_task_running);
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
    }

    /// Replaces the contextual footer label for the currently viewed agent.
    ///
    /// Returning `false` means the value was unchanged, so callers can skip redraw work. This
    /// field is intentionally just cached presentation state; `ChatComposer` does not infer which
    /// thread is active on its own.
    pub(crate) fn set_active_agent_label(&mut self, active_agent_label: Option<String>) -> bool {
        if self.footer.active_agent_label == active_agent_label {
            return false;
        }
        self.footer.active_agent_label = active_agent_label;
        true
    }
}

impl ChatComposer {
    pub(crate) fn set_luna_reserve_active(&mut self, active: bool) -> bool {
        if self.luna_reserve_active == active {
            return false;
        }
        self.luna_reserve_active = active;
        true
    }
}

pub(super) fn footer_insert_newline_key(
    bindings: &[KeyBinding],
    enhanced_keys_supported: bool,
) -> Option<KeyBinding> {
    let bindings = user_bindings(bindings);
    let shift_enter = key_hint::shift(KeyCode::Enter);
    if enhanced_keys_supported && bindings.contains(&shift_enter) {
        return Some(shift_enter);
    }

    let plain_enter = key_hint::plain(KeyCode::Enter);
    bindings
        .iter()
        .copied()
        .find(|binding| *binding != plain_enter)
        .or_else(|| bindings.first().copied())
}
