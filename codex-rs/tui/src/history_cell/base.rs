//! Shared history-cell building blocks reused across transcript concerns.
//! Wrapped prefixes remain display-only while annotated rows retain their logical source.

use super::*;

#[derive(Debug)]
pub(crate) struct PlainHistoryCell {
    pub(super) lines: Vec<Line<'static>>,
}

impl PlainHistoryCell {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for PlainHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.lines.clone())
    }
}

#[derive(Debug)]
pub(crate) struct WebHyperlinkHistoryCell {
    lines: Vec<HyperlinkLine>,
}

impl WebHyperlinkHistoryCell {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines: crate::terminal_hyperlinks::annotate_web_urls(lines),
        }
    }

    pub(crate) fn new_hyperlink_lines(lines: Vec<HyperlinkLine>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for WebHyperlinkHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.iter().map(|line| line.line.clone()).collect()
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.lines.clone()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.lines.iter().map(|line| line.line.clone()))
    }
}
#[derive(Debug)]
pub(crate) struct PrefixedWrappedHistoryCell {
    text: Text<'static>,
    initial_prefix: Line<'static>,
    subsequent_prefix: Line<'static>,
}

impl PrefixedWrappedHistoryCell {
    pub(crate) fn new(
        text: impl Into<Text<'static>>,
        initial_prefix: impl Into<Line<'static>>,
        subsequent_prefix: impl Into<Line<'static>>,
    ) -> Self {
        Self {
            text: text.into(),
            initial_prefix: initial_prefix.into(),
            subsequent_prefix: subsequent_prefix.into(),
        }
    }
}

impl HistoryCell for PrefixedWrappedHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if width == 0 {
            return Vec::new();
        }
        let opts = RtOptions::new(usize::from(width))
            .initial_indent(self.initial_prefix.clone())
            .subsequent_indent(self.subsequent_prefix.clone());
        crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            &plain_hyperlink_lines(self.text.lines.clone()),
            opts,
        )
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.text.clone().lines)
    }
}
#[derive(Debug)]
pub(crate) struct CompositeHistoryCell {
    pub(super) parts: Vec<Box<dyn HistoryCell>>,
}

impl CompositeHistoryCell {
    pub(crate) fn new(parts: Vec<Box<dyn HistoryCell>>) -> Self {
        Self { parts }
    }

    /// Concatenates each part's non-empty lines, separating parts with one blank line.
    fn join_parts<T: From<&'static str>>(
        &self,
        render: impl Fn(&dyn HistoryCell) -> Vec<T>,
    ) -> Vec<T> {
        let mut out = Vec::new();
        for lines in self.parts.iter().map(|part| render(part.as_ref())) {
            if lines.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(T::from(""));
            }
            out.extend(lines);
        }
        out
    }
}

impl HistoryCell for CompositeHistoryCell {
    fn warning_entries(&self) -> Vec<WarningEntry> {
        self.parts
            .iter()
            .flat_map(|part| part.warning_entries())
            .collect()
    }

    fn live_raw_lines(&self) -> Vec<Line<'static>> {
        self.join_parts(HistoryCell::live_raw_lines)
    }

    fn warning_keys(&self) -> Vec<WarningKey<'_>> {
        self.parts
            .iter()
            .flat_map(|part| part.warning_keys())
            .collect()
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.join_parts(|part| part.compact_hyperlink_lines(width))
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.join_parts(|part| part.display_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.join_parts(|part| part.display_hyperlink_lines(width))
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.join_parts(|part| part.transcript_hyperlink_lines(width))
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.join_parts(HistoryCell::raw_lines)
    }

    fn has_stable_transcript_height(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[path = "base_tests.rs"]
mod tests;
