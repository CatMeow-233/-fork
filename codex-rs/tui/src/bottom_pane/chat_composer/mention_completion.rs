//! Completion token detection and insertion, pending-paste expansion, and mention bindings.

use super::*;

impl ChatComposer {
    fn is_image_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".gif")
            || lower.ends_with(".webp")
    }

    /// Leaves the cursor after one horizontal separator following a completion.
    ///
    /// Another separator is preserved before any non-whitespace suffix so subsequent typing does
    /// not merge into it. Line breaks are never reused as separators; a space is inserted before
    /// them so subsequent typing stays on the completed token's line.
    ///
    /// Returns the position of the space when completion inserted one.
    fn advance_past_completion_separator(&mut self) -> Option<usize> {
        let cursor = self.draft.textarea.cursor();
        let existing_separator_len = self.draft.textarea.text()[cursor..]
            .chars()
            .next()
            .filter(|c| {
                c.is_whitespace()
                    && !matches!(
                        *c,
                        '\n' | '\r'
                            | '\u{000B}'
                            | '\u{000C}'
                            | '\u{0085}'
                            | '\u{2028}'
                            | '\u{2029}'
                    )
            })
            .map(char::len_utf8);
        if let Some(separator_len) = existing_separator_len {
            let after_separator = cursor + separator_len;
            let separator_precedes_suffix = self.draft.textarea.text()[after_separator..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
            if !separator_precedes_suffix {
                self.draft.textarea.set_cursor(after_separator);
                return None;
            }
        }
        self.draft.textarea.insert_str_at(cursor, " ");
        Some(cursor)
    }

    /// Dismisses popup synchronization only for the exact token occurrence just inserted.
    ///
    /// Matching both range and text prevents an identical token later in the draft from inheriting
    /// the completed token's dismissal state.
    fn dismiss_completed_prefixed_token(
        &mut self,
        prefix: char,
        inserted_range: Range<usize>,
        inserted_text: &str,
    ) {
        let Some(completed_token) = inserted_text.strip_prefix(prefix) else {
            return;
        };
        // Completion leaves the cursor on separator whitespace, where normal token affinity would
        // otherwise immediately reopen the popup for sigil-prefixed inserted text.
        let Some((current_range, current_token)) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            /*allow_empty*/ true,
        ) else {
            return;
        };
        if current_range != inserted_range || current_token != completed_token {
            return;
        }

        if prefix == '@' && !self.mentions_v2_enabled {
            self.popups.dismissed_file_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        } else {
            self.popups.dismissed_mention_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        }
    }

    pub(super) fn insert_selected_file_path(
        &mut self,
        token_range: Range<usize>,
        selected_path: &str,
    ) {
        let started_vim_edit = self.begin_direct_vim_edit();
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        if Self::is_image_path(selected_path) {
            let path_buf = PathBuf::from(selected_path);
            match image::image_dimensions(&path_buf) {
                Ok((width, height)) => {
                    tracing::debug!("selected image dimensions={}x{}", width, height);
                    let start_idx = token_range.start;
                    self.draft.textarea.replace_range(token_range, "");
                    self.draft.textarea.set_cursor(start_idx);
                    self.attach_image(path_buf);
                    self.advance_past_completion_separator();
                }
                Err(err) => {
                    tracing::trace!("image dimensions lookup failed: {err}");
                    self.insert_selected_path(token_range, selected_path);
                }
            }
        } else {
            self.insert_selected_path(token_range, selected_path);
        }
        if started_vim_edit {
            self.finish_vim_edit();
        }
    }

    pub(super) fn trim_text_elements(
        original: &str,
        trimmed: &str,
        elements: Vec<TextElement>,
    ) -> Vec<TextElement> {
        if trimmed.is_empty() || elements.is_empty() {
            return Vec::new();
        }
        let trimmed_start = original.len().saturating_sub(original.trim_start().len());
        let trimmed_end = trimmed_start.saturating_add(trimmed.len());

        elements
            .into_iter()
            .filter_map(|elem| {
                let start = elem.byte_range.start;
                let end = elem.byte_range.end;
                if end <= trimmed_start || start >= trimmed_end {
                    return None;
                }
                let new_start = start.saturating_sub(trimmed_start);
                let new_end = end.saturating_sub(trimmed_start).min(trimmed.len());
                if new_start >= new_end {
                    return None;
                }
                let placeholder = trimmed.get(new_start..new_end).map(str::to_string);
                Some(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ))
            })
            .collect()
    }

    /// Expand large-paste placeholders using element ranges and rebuild other element spans.
    pub(crate) fn expand_pending_pastes(
        text: &str,
        mut elements: Vec<TextElement>,
        pending_pastes: &[(String, String)],
    ) -> (String, Vec<TextElement>) {
        if pending_pastes.is_empty() || elements.is_empty() {
            return (text.to_string(), elements);
        }

        // Stage 1: index pending paste payloads by placeholder for deterministic replacements.
        let mut pending_by_placeholder: HashMap<&str, VecDeque<&str>> = HashMap::new();
        for (placeholder, actual) in pending_pastes {
            pending_by_placeholder
                .entry(placeholder.as_str())
                .or_default()
                .push_back(actual.as_str());
        }

        // Stage 2: walk elements in order and rebuild text/spans in a single pass.
        elements.sort_by_key(|elem| elem.byte_range.start);

        let mut rebuilt = String::with_capacity(text.len());
        let mut rebuilt_elements = Vec::with_capacity(elements.len());
        let mut cursor = 0usize;

        for elem in elements {
            let start = elem.byte_range.start.min(text.len());
            let end = elem.byte_range.end.min(text.len());
            if start > end {
                continue;
            }
            if start > cursor {
                rebuilt.push_str(&text[cursor..start]);
            }
            let elem_text = &text[start..end];
            let placeholder = elem.placeholder(text).map(str::to_string);
            let replacement = placeholder
                .as_deref()
                .and_then(|ph| pending_by_placeholder.get_mut(ph))
                .and_then(VecDeque::pop_front);
            if let Some(actual) = replacement {
                // Stage 3: inline actual paste payloads and drop their placeholder elements.
                rebuilt.push_str(actual);
            } else {
                // Stage 4: keep non-paste elements, updating their byte ranges for the new text.
                let new_start = rebuilt.len();
                rebuilt.push_str(elem_text);
                let new_end = rebuilt.len();
                let placeholder = placeholder.or_else(|| Some(elem_text.to_string()));
                rebuilt_elements.push(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ));
            }
            cursor = end;
        }

        // Stage 5: append any trailing text that followed the last element.
        if cursor < text.len() {
            rebuilt.push_str(&text[cursor..]);
        }

        (rebuilt, rebuilt_elements)
    }

    pub fn skills(&self) -> Option<&Vec<SkillMetadata>> {
        self.skills.as_ref()
    }

    pub fn plugins(&self) -> Option<&Vec<PluginCapabilitySummary>> {
        self.plugins.as_ref()
    }

    fn mentions_enabled(&self) -> bool {
        let skills_ready = self
            .skills
            .as_ref()
            .is_some_and(|skills| !skills.is_empty());
        let plugins_ready = self
            .plugins
            .as_ref()
            .is_some_and(|plugins| !plugins.is_empty());
        let connectors_ready = self.connectors_enabled
            && self
                .connectors_snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.connectors.is_empty());
        skills_ready || plugins_ready || connectors_ready
    }

    fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<String> {
        completion_target::current_prefixed_token_range(textarea, prefix, allow_empty)
            .map(|(_, token)| token)
    }

    /// Extract the `@token` that the cursor is currently positioned on, if any.
    ///
    /// The returned string **does not** include the leading `@`.
    pub(super) fn current_at_token(textarea: &TextArea) -> Option<String> {
        Self::current_prefixed_token(textarea, '@', /*allow_empty*/ false)
    }

    /// Returns the active prefixed token only when its sigil and name remain editable plaintext.
    ///
    /// Atomic elements and tokens whose mention prefix is already atomic are excluded so bound
    /// mentions are not offered for completion again.
    fn current_editable_prefixed_token_range(
        &self,
        prefix: char,
        allow_empty: bool,
    ) -> Option<(Range<usize>, String)> {
        let (range, token) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            allow_empty,
        )?;
        completion_target::prefixed_token_range_is_editable(
            &self.draft.textarea,
            prefix,
            &range,
            &token,
        )
        .then_some((range, token))
    }

    pub(super) fn current_editable_at_token_range_with_options(
        &self,
        allow_empty: bool,
    ) -> Option<(Range<usize>, String)> {
        self.current_editable_prefixed_token_range('@', allow_empty)
    }

    pub(super) fn current_mentions_v2_token_range(&self) -> Option<(Range<usize>, String)> {
        if !self.mentions_v2_enabled {
            return None;
        }
        self.current_editable_at_token_range_with_options(/*allow_empty*/ true)
    }

    pub(super) fn current_mentions_v2_token(&self) -> Option<String> {
        self.current_mentions_v2_token_range()
            .map(|(_, token)| token)
    }

    /// Resolves the active legacy `$` target without eagerly cloning the mention catalog.
    ///
    /// Definite shell syntax is rejected. Ambiguous shell-like queries are accepted only when a
    /// bindable mention matches. The query-independent catalog built during that check is carried
    /// forward only when the selected target is also ambiguous.
    pub(super) fn current_mention_target(&self) -> Option<MentionCompletionTarget> {
        if !self.mentions_enabled() {
            return None;
        }
        let mentions: OnceCell<Vec<MentionItem>> = OnceCell::new();
        let (range, query) = {
            let dollar_query_is_completable =
                |query: &str| match completion_target::dollar_query_kind(query) {
                    completion_target::DollarQueryKind::Completable => true,
                    completion_target::DollarQueryKind::AmbiguousShellParameter => mentions
                        .get_or_init(|| {
                            self.mention_items()
                                .into_iter()
                                .filter(|mention| {
                                    mention.path.is_some()
                                        && Self::mention_token_from_insert_text(
                                            &mention.insert_text,
                                        )
                                        .is_some()
                                })
                                .collect()
                        })
                        .iter()
                        .any(|mention| mention.fuzzy_match_query(query).is_some()),
                    completion_target::DollarQueryKind::ShellVariable
                    | completion_target::DollarQueryKind::DefiniteShellParameter
                    | completion_target::DollarQueryKind::Invalid => false,
                };
            let (range, query) =
                completion_target::current_prefixed_token_range_with_dollar_predicate(
                    &self.draft.textarea,
                    '$',
                    /*allow_empty*/ true,
                    dollar_query_is_completable,
                )?;
            if !completion_target::prefixed_token_range_is_editable(
                &self.draft.textarea,
                '$',
                &range,
                &query,
            ) || !dollar_query_is_completable(&query)
            {
                return None;
            }
            (range, query)
        };
        let prebuilt_mentions = if matches!(
            completion_target::dollar_query_kind(&query),
            completion_target::DollarQueryKind::AmbiguousShellParameter
        ) {
            mentions.into_inner()
        } else {
            None
        };

        Some(MentionCompletionTarget {
            range,
            query,
            prebuilt_mentions,
        })
    }

    /// Replace the active `@token` (the one under the cursor) with `path`.
    pub(super) fn insert_selected_path(&mut self, token_range: Range<usize>, path: &str) {
        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid adding
        // quotes when the path already contains one to keep behavior simple.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace just the active `@token` so unrelated text elements, such as
        // large-paste placeholders, remain atomic and can still expand on submit.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, &inserted);
        let inserted_range = start_idx..start_idx.saturating_add(inserted.len());
        self.draft.textarea.set_cursor(inserted_range.end);
        self.advance_past_completion_separator();
        self.dismiss_completed_prefixed_token('@', inserted_range, &inserted);
    }

    pub(super) fn insert_selected_mention(
        &mut self,
        token_range: Range<usize>,
        insert_text: &str,
        path: Option<&str>,
    ) {
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        // Remove the active token and insert the selected mention as an atomic element.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, "");
        self.draft.textarea.set_cursor(start_idx);
        let id = self.draft.textarea.insert_element(insert_text);
        let inserted_range = start_idx..start_idx.saturating_add(insert_text.len());

        let mention = if path
            .and_then(crate::task_mentions::valid_thread_path)
            .is_some()
        {
            insert_text
                .strip_prefix('@')
                .filter(|name| !name.is_empty())
                .map(|name| ('@', name.to_string()))
        } else {
            Self::mention_token_from_insert_text(insert_text)
        };
        if let (Some(path), Some((sigil, mention))) = (path, mention) {
            self.draft.mention_bindings.insert(
                id,
                ComposerMentionBinding {
                    sigil,
                    mention,
                    path: path.to_string(),
                },
            );
        }
        if path
            .and_then(crate::task_mentions::valid_thread_path)
            .is_some()
            && let Some(task_mentions) = &mut self.task_mentions
        {
            task_mentions.clear();
        }

        let inserted_space = self.advance_past_completion_separator();
        let is_skill = path.is_some_and(|path| {
            self.skills
                .as_deref()
                .is_some_and(|skills| skills.iter().any(|skill| skill.path.as_str() == path))
        });
        self.draft.skill_completion_space = inserted_space.filter(|_| is_skill);
        if let Some(sigil) = insert_text.chars().next()
            && matches!(sigil, '$' | '@')
        {
            self.dismiss_completed_prefixed_token(sigil, inserted_range, insert_text);
        }
    }

    /// Inserts a leading separator when a completion starts directly after an atomic element.
    ///
    /// The returned range is shifted to keep replacing the same editable token after insertion.
    fn separate_completion_from_adjacent_element(
        &mut self,
        mut token_range: Range<usize>,
    ) -> Range<usize> {
        let starts_after_element = self
            .draft
            .textarea
            .text_element_ranges()
            .any(|range| range.end == token_range.start);
        if starts_after_element {
            self.draft
                .textarea
                .replace_range(token_range.start..token_range.start, " ");
            token_range.start += 1;
            token_range.end += 1;
        }
        token_range
    }

    fn mention_token_from_insert_text(insert_text: &str) -> Option<(char, String)> {
        let sigil = insert_text.chars().next()?;
        if !matches!(sigil, '$' | '@') {
            return None;
        }
        let name = &insert_text[sigil.len_utf8()..];
        if name.is_empty() {
            return None;
        }
        if name
            .as_bytes()
            .iter()
            .all(|byte| is_mention_name_char(*byte))
        {
            Some((sigil, name.to_string()))
        } else {
            None
        }
    }

    pub(super) fn current_mention_elements(&self) -> Vec<(u64, char, String)> {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                if let Some(binding) = self.draft.mention_bindings.get(&snapshot.id)
                    && crate::task_mentions::valid_thread_path(&binding.path).is_some()
                    && snapshot.text == format!("{}{}", binding.sigil, binding.mention)
                {
                    return Some((snapshot.id, binding.sigil, binding.mention.clone()));
                }
                Self::mention_token_from_insert_text(snapshot.text.as_str())
                    .map(|(sigil, mention)| (snapshot.id, sigil, mention))
            })
            .collect()
    }

    pub(super) fn snapshot_mention_bindings(&self) -> Vec<MentionBinding> {
        self.snapshot_mention_bindings_with_ranges().0
    }

    pub(super) fn snapshot_mention_bindings_with_ranges(
        &self,
    ) -> (Vec<MentionBinding>, Vec<Range<usize>>) {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|element| {
                let binding = self.draft.mention_bindings.get(&element.id)?;
                if element.text != format!("{}{}", binding.sigil, binding.mention)
                    || (crate::task_mentions::valid_thread_path(&binding.path).is_none()
                        && Self::mention_token_from_insert_text(&element.text).is_none())
                {
                    return None;
                }
                Some((
                    MentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention.clone(),
                        path: binding.path.clone(),
                    },
                    element.range,
                ))
            })
            .unzip()
    }

    pub(super) fn bind_mentions_from_snapshot(&mut self, mention_bindings: Vec<MentionBinding>) {
        self.draft.mention_bindings.clear();
        if mention_bindings.is_empty() {
            return;
        }

        let text = self.draft.textarea.text().to_string();
        let mut scan_from = 0usize;
        for binding in mention_bindings {
            let token = format!("{}{}", binding.sigil, binding.mention);
            let range = if crate::task_mentions::valid_thread_path(&binding.path).is_some() {
                self.draft
                    .textarea
                    .text_element_snapshots()
                    .into_iter()
                    .find(|element| element.range.start >= scan_from && element.text == token)
                    .map(|element| element.range)
            } else {
                find_next_mention_token_range(text.as_str(), token.as_str(), scan_from)
            };
            let Some(range) = range else {
                continue;
            };

            let id = if let Some(id) = self.draft.textarea.add_element_range(range.clone()) {
                Some(id)
            } else {
                self.draft
                    .textarea
                    .element_id_for_exact_range(range.clone())
            };

            if let Some(id) = id {
                self.draft.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
                scan_from = range.end;
            }
        }
    }

    pub(super) fn plugin_at_mention_highlights(&self) -> Vec<(Range<usize>, Style)> {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                let binding = self.draft.mention_bindings.get(&snapshot.id)?;
                if !binding.path.starts_with("plugin://") || !snapshot.text.starts_with('@') {
                    return None;
                }
                Some((snapshot.range, Style::default().fg(Color::Magenta)))
            })
            .collect()
    }
}

pub(super) fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

pub(super) fn ends_plaintext_at_mention(bytes: &[u8], index: usize) -> bool {
    bytes.get(index).is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || *byte == b'.'
                && bytes.get(index + 1).is_none_or(|next| {
                    next.is_ascii_whitespace()
                        || !next.is_ascii_alphanumeric() && *next != b'_' && *next != b'-'
                })
            || !matches!(*byte, b'.' | b'/' | b'\\')
                && !byte.is_ascii_alphanumeric()
                && *byte != b'_'
                && *byte != b'-'
    })
}

pub(super) fn ends_plaintext_at_dollar_mention(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index)
        .is_none_or(|byte| !is_mention_name_char(*byte))
}

fn starts_plaintext_at_mention(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }

    text.get(..index)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|ch| ch.is_whitespace() || !is_mention_name_char_char(ch))
}

fn is_mention_name_char_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn find_next_mention_token_range(text: &str, token: &str, from: usize) -> Option<Range<usize>> {
    if token.is_empty() || from >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let token_bytes = token.as_bytes();
    let sigil = *token_bytes.first()?;
    let mut index = from;

    while index < bytes.len() {
        if bytes[index] != sigil {
            index += 1;
            continue;
        }

        let end = index.saturating_add(token_bytes.len());
        if end > bytes.len() {
            return None;
        }
        if &bytes[index..end] != token_bytes {
            index += 1;
            continue;
        }

        // Fix for restored `@` mentions: rebinding must not attach to embedded substrings such
        // as email addresses, while preserving the existing `$` mention matching behavior.
        let starts_plaintext_mention = if sigil == b'@' {
            starts_plaintext_at_mention(text, index)
        } else {
            true
        };
        // Fix for restored `@` mentions: mirror history encoding's trailing boundary so path-like
        // text such as `@sample/pkg` is not rebound as the plain `@sample` mention.
        let ends_plaintext_mention = if sigil == b'@' {
            ends_plaintext_at_mention(bytes, end)
        } else {
            ends_plaintext_at_dollar_mention(bytes, end)
        };

        if starts_plaintext_mention && ends_plaintext_mention {
            return Some(index..end);
        }

        index = end;
    }

    None
}
