//! The `Renderable` implementation and the composer's full render pass.

use super::*;

impl Renderable for ChatComposer {
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_options(area, ComposerRenderOptions::default())
    }

    fn cursor_style(&self, _area: Rect) -> crossterm::cursor::SetCursorStyle {
        if self.draft.textarea.uses_vim_insert_cursor() {
            crossterm::cursor::SetCursorStyle::SteadyBar
        } else {
            crossterm::cursor::SetCursorStyle::DefaultUserShape
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.desired_height_with_options(width, ComposerRenderOptions::default())
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_mask(area, buf, /*mask_char*/ None);
    }
}

impl ChatComposer {
    pub(crate) fn render_with_mask(&self, area: Rect, buf: &mut Buffer, mask_char: Option<char>) {
        self.render_with_options(area, buf, mask_char, ComposerRenderOptions::default());
    }

    pub(crate) fn render_with_options(
        &self,
        area: Rect,
        buf: &mut Buffer,
        mask_char: Option<char>,
        options: ComposerRenderOptions<'_>,
    ) {
        let ComposerLayout {
            status,
            composer: composer_rect,
            remote_images: remote_images_rect,
            thumbnails,
            textarea: textarea_rect,
            popup: popup_rect,
            footer: footer_rect,
        } = self.layout_with_options(area, options);
        self.render_status_surface(status, buf, options);
        if self.popups.active.is_above_composer()
            && options.command_popup_placement != CommandPopupPlacement::Hidden
        {
            let popup_rect = if options.command_popup_placement == CommandPopupPlacement::Overlay {
                let rows_above_composer = composer_rect
                    .y
                    .saturating_sub(options.overlay_top.max(buf.area.y));
                let height = self
                    .popups
                    .active
                    .required_height(composer_rect.width, /*footer_total_height*/ 0)
                    .min(rows_above_composer);
                // Command and unified mention menus can render in a single row.
                let height = if matches!(
                    self.popups.active,
                    ActivePopup::Command(_) | ActivePopup::MentionV2(_)
                ) || height > 2
                {
                    height
                } else {
                    0
                };
                Rect::new(
                    composer_rect.x,
                    composer_rect.y - height,
                    composer_rect.width,
                    height,
                )
            } else {
                popup_rect
            };
            // Overlay rows must erase both glyphs and styles from the transcript underneath.
            ratatui::widgets::Clear.render(popup_rect, buf);
            match &self.popups.active {
                ActivePopup::Command(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::MentionV2(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::File(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::Skill(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::None => {
                    unreachable!("only suggestion menus use above-composer placement")
                }
            }
        }
        let transcript_hint_area = inset_footer_hint_area(footer_rect);
        let warning_notice = self
            .warning_notice_layout(transcript_hint_area, options)
            .filter(|_| !options.unfocused);
        let warning_area = warning_notice.as_ref().map(|(area, _)| *area);
        match options.footer {
            _ if options.unfocused => {}
            Some(footer) => {
                self.render_transcript_footer(transcript_hint_area, buf, footer, warning_area);
            }
            None if self.shortcuts_above_composer(options) => {
                let props = self.hint_footer_props(options);
                super::shortcut_overlay::render(&props, inset_footer_hint_area(popup_rect), buf);
                super::shortcut_overlay::close_hint(&props, transcript_hint_area.width)
                    .render(transcript_hint_area, buf);
            }
            None => {
                let hint_rect = warning_notice::before_warning(footer_rect, warning_area);
                if let Some(input) = self.draft.textarea.vim_query() {
                    input.render(inset_footer_hint_area(hint_rect), buf);
                } else if let Some(line) = self.history_search_footer_line() {
                    render_footer_line(hint_rect, buf, line);
                } else if self.shows_shared_status(options) {
                    self.render_status_surface(hint_rect, buf, options);
                } else {
                    let footer_props = self.hint_footer_props(options);
                    let show_cycle_hint = !self.has_separate_status(options)
                        && !footer_props.is_task_running
                        && self.footer.collaboration_mode_indicator.is_some();
                    let show_shortcuts_hint =
                        footer_props.mode == FooterMode::ComposerEmpty && !self.is_in_paste_burst();
                    let show_queue_hint = footer_props.mode == FooterMode::ComposerHasDraft
                        && footer_props.is_task_running;
                    let status_line_active = uses_passive_footer_status_layout(&footer_props);
                    let left_mode_indicator =
                        if status_line_active || self.has_separate_status(options) {
                            None
                        } else {
                            self.footer.collaboration_mode_indicator
                        };
                    let active_footer_hint_override = self.footer.hint_override.as_ref();
                    let left_width = if self.footer.flash_visible() {
                        self.footer
                            .flash
                            .as_ref()
                            .map(|flash| flash.line.width() as u16)
                            .unwrap_or(0)
                    } else if let Some(items) = active_footer_hint_override {
                        footer_hint_items_width(items)
                    } else {
                        footer_line_width(
                            &footer_props,
                            left_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        )
                    };
                    let right_line =
                        if let Some(label) = self.footer.side_conversation_context_label.as_ref() {
                            Some(side_conversation_context_line(label))
                        } else if let Some(line) = self.shell_mode_footer_line() {
                            Some(line)
                        } else if self.has_separate_status(options) {
                            None
                        } else if status_line_active {
                            let full = self.mode_indicator_line(show_cycle_hint);
                            let compact = self.mode_indicator_line(/*show_cycle_hint*/ false);
                            let full_width = full.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                            if can_show_left_with_context(hint_rect, left_width, full_width) {
                                full
                            } else {
                                compact
                            }
                        } else {
                            Some(self.right_footer_line_with_context())
                        };
                    let right_width = right_line.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                    let can_show_left_and_context =
                        can_show_left_with_context(hint_rect, left_width, right_width);
                    let has_override =
                        self.footer.flash_visible() || active_footer_hint_override.is_some();
                    let single_line_layout = if has_override || status_line_active {
                        None
                    } else {
                        match footer_props.mode {
                            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                                // Both of these modes render the single-line footer style (with
                                // either the shortcuts hint or the optional queue hint). We still
                                // want the single-line collapse rules so the mode label can win over
                                // the context indicator on narrow widths.
                                Some(single_line_footer_layout(
                                    hint_rect,
                                    right_width,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                    footer_props.key_hints,
                                ))
                            }
                            FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay => None,
                        }
                    };
                    let show_right = if matches!(
                        footer_props.mode,
                        FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay
                    ) {
                        false
                    } else {
                        single_line_layout
                            .as_ref()
                            .map(|(_, show_context)| *show_context)
                            .unwrap_or(can_show_left_and_context)
                    };

                    if let Some((summary_left, _)) = single_line_layout {
                        match summary_left {
                            SummaryLeft::Default => {
                                render_footer_from_props(
                                    hint_rect,
                                    buf,
                                    &footer_props,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                );
                            }
                            SummaryLeft::Custom(line) => {
                                render_footer_line(hint_rect, buf, line);
                            }
                            SummaryLeft::None => {}
                        }
                    } else if self.footer.flash_visible() {
                        if let Some(flash) = self.footer.flash.as_ref() {
                            Widget::render(&flash.line, inset_footer_hint_area(hint_rect), buf);
                        }
                    } else if let Some(items) = active_footer_hint_override {
                        render_footer_hint_items(hint_rect, buf, items);
                    } else {
                        render_footer_from_props(
                            hint_rect,
                            buf,
                            &footer_props,
                            self.footer.collaboration_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        );
                    }
                    if show_right && let Some(line) = &right_line {
                        render_context_right(hint_rect, buf, line);
                    }
                }
            }
        }
        self.footer.warning_notice_area.set(warning_area);
        if let Some((warning_area, line)) = warning_notice {
            line.render(warning_area, buf);
        }
        if let Some(selection) = options.rendered_selection
            && !self.popup_active()
            && options.footer.is_none_or(|footer| !footer.is_interactive)
        {
            let revision = (options.warning_count, self.footer_props().is_task_running);
            let changed = self.footer.selection_revision.replace(revision) != revision;
            let mut selection = selection.borrow_mut();
            selection.register(status.intersection(buf.area), changed);
            selection.register(footer_rect.intersection(buf.area), changed);
        }
        let style = user_message_style();
        Block::default().style(style).render(composer_rect, buf);
        if let Some(previews) = options.image_previews {
            previews.render(
                thumbnails,
                buf,
                &self.attachments.local_images,
                &self.draft.textarea,
            );
        }
        if !remote_images_rect.is_empty() {
            let first = self
                .attachments
                .selected_remote_image_index
                .unwrap_or_default()
                .saturating_sub(usize::from(remote_images_rect.height) - 1);
            Paragraph::new(
                self.attachments
                    .remote_image_lines()
                    .into_iter()
                    .skip(first)
                    .collect::<Vec<_>>(),
            )
            .style(style)
            .render(remote_images_rect, buf);
        }
        if !textarea_rect.is_empty() {
            let prompt = if self.draft.input_enabled {
                if self.draft.is_bash_mode {
                    Span::from("!").light_red().bold()
                } else if self.luna_reserve_active {
                    // Reserve keeps one arrow at every reasoning effort; only its foreground changes.
                    "›"
                        .fg(crate::terminal_palette::best_color((246, 197, 67)))
                        .bold()
                } else if let Some(tier) = self.effort_tier {
                    let charge = self
                        .effort_ignition
                        .as_ref()
                        .map(EffortIgnition::charge_alpha)
                        .unwrap_or(1.0);
                    tier.prompt(charge)
                } else {
                    "›".bold()
                }
            } else {
                "›".dim()
            };
            buf.set_span(
                textarea_rect.x - LIVE_PREFIX_COLS,
                textarea_rect.y,
                &prompt,
                textarea_rect.width,
            );
        }

        let mut state = self.draft.textarea_state.borrow_mut();
        if options.max_height.is_none() {
            state.follow_cursor();
        }
        let textarea_is_empty = self.draft.textarea.text().is_empty() && !self.draft.is_bash_mode;
        if self.draft.input_enabled {
            if let Some(mask_char) = mask_char {
                self.draft
                    .textarea
                    .render_ref_masked(textarea_rect, buf, &mut state, mask_char);
            } else {
                let mut highlights = self.plugin_at_mention_highlights();
                let search_highlight_style =
                    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
                highlights.extend(
                    self.history_search_highlight_ranges()
                        .into_iter()
                        .chain(self.draft.textarea.vim_search_highlights())
                        .map(|range| (range, search_highlight_style)),
                );
                if highlights.is_empty() {
                    StatefulWidgetRef::render_ref(
                        &(&self.draft.textarea),
                        textarea_rect,
                        buf,
                        &mut state,
                    );
                } else {
                    self.draft.textarea.render_ref_styled_with_highlights(
                        textarea_rect,
                        buf,
                        &mut state,
                        Style::default(),
                        &highlights,
                    );
                }
            }
        }
        if !options.unfocused && (!self.draft.input_enabled || textarea_is_empty) {
            let text = if self.draft.input_enabled {
                self.placeholder_text.as_str().to_string()
            } else {
                self.draft
                    .input_disabled_placeholder
                    .as_deref()
                    .unwrap_or("Input disabled.")
                    .to_string()
            };
            if !textarea_rect.is_empty() {
                let placeholder = Span::from(text).dim();
                Line::from(vec![placeholder]).render(textarea_rect.inner(Margin::new(0, 0)), buf);
            }
        }
        // The reserved right margin indicates hidden rows without changing wrapping.
        if options.max_height.is_some()
            && self.draft.input_enabled
            && !textarea_rect.is_empty()
            && textarea_rect.right() < composer_rect.right()
        {
            if state.scroll > 0 {
                buf.set_span(textarea_rect.right(), textarea_rect.y, &"↑".dim(), 1);
            }
            if state.scroll.saturating_add(textarea_rect.height)
                < self.draft.textarea.desired_height(textarea_rect.width)
            {
                buf.set_span(
                    textarea_rect.right(),
                    textarea_rect.bottom() - 1,
                    &"↓".dim(),
                    1,
                );
            }
        }
        if matches!(self.popups.active, ActivePopup::None)
            && let Some(ignition) = &self.effort_ignition
            && !ignition.is_finished()
        {
            let protected_top = if remote_images_rect.is_empty() {
                textarea_rect.y
            } else {
                remote_images_rect.y
            };
            let protected = Rect::new(
                composer_rect.x,
                protected_top,
                composer_rect.width,
                textarea_rect.bottom().saturating_sub(protected_top),
            );
            if ignition.render(composer_rect, protected, buf)
                && let Some(frame_requester) = &self.frame_requester
            {
                frame_requester.schedule_frame_in(IGNITION_FRAME_TICK);
            }
        }
        drop(state);
        self.render_voice_strip(composer_rect, buf);
        self.render_sparkle(
            composer_rect,
            textarea_rect,
            self.cursor_pos_with_options(area, options),
            buf,
        );
        if options.unfocused {
            buf.set_style(composer_rect, crate::style::quiet_workspace_style());
        } else if options.footer.is_some_and(|footer| footer.is_interactive) {
            buf.set_style(composer_rect, Style::default().dim());
        }
    }
}
