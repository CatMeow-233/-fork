use ratatui::layout::Rect;

use super::DESIRED_SPACERS_BETWEEN_SECTIONS;
use super::RequestUserInputOverlay;

pub(super) struct LayoutSections {
    pub(super) progress_area: Rect,
    pub(super) question_area: Rect,
    // Wrapped question text lines to render in the question area.
    pub(super) question_lines: Vec<crate::terminal_hyperlinks::HyperlinkLine>,
    pub(super) options_area: Rect,
    pub(super) notes_area: Rect,
    // Number of footer rows (status + hints).
    pub(super) footer_lines: u16,
}

/// Row heights for each section, top to bottom.
#[derive(Clone, Copy, Debug, Default)]
struct LayoutPlan {
    progress_height: u16,
    question_height: u16,
    spacer_after_question: u16,
    options_height: u16,
    spacer_after_options: u16,
    notes_height: u16,
    footer_lines: u16,
}

impl RequestUserInputOverlay {
    /// Compute layout sections, collapsing notes and hints as space shrinks.
    pub(super) fn layout_sections(&self, area: Rect) -> LayoutSections {
        let has_options = self.has_options();
        let mut question_lines = self.wrapped_question_lines(area.width);
        // Questions with options always leave at least one row for the options.
        let question_height =
            (question_lines.len() as u16).min(area.height.saturating_sub(u16::from(has_options)));
        question_lines.truncate(usize::from(question_height));
        let plan = if has_options {
            self.layout_with_options(area, question_height)
        } else {
            self.layout_without_options(area, question_height)
        };

        let mut y = area.y;
        let mut take_rows = |height| {
            let rows = Rect { y, height, ..area };
            y = y.saturating_add(height);
            rows
        };
        let progress_area = take_rows(plan.progress_height);
        let question_area = take_rows(plan.question_height);
        take_rows(plan.spacer_after_question);
        let options_area = take_rows(plan.options_height);
        take_rows(plan.spacer_after_options);
        let notes_area = take_rows(plan.notes_height);
        LayoutSections {
            progress_area,
            question_area,
            question_lines,
            options_area,
            notes_area,
            footer_lines: plan.footer_lines,
        }
    }

    /// Layout when options are present: allocate footer + progress first, and
    /// only allocate notes (and its label) when explicitly visible.
    fn layout_with_options(&self, area: Rect, question_height: u16) -> LayoutPlan {
        let notes_visible = self.notes_ui_visible();
        let footer_pref = self.footer_required_height(area.width);
        // Options prefer their full height, so one measurement serves both roles.
        let full_options_height = self.options_required_height(area.width);
        let max_options_height = area.height.saturating_sub(question_height);
        let min_options_height = max_options_height.min(1);
        let mut options_height = full_options_height
            .min(max_options_height)
            .max(min_options_height);
        let used = question_height.saturating_add(options_height);
        let mut remaining = area.height.saturating_sub(used);

        // When notes are hidden, prefer to reserve room for progress, footer,
        // and spacers by shrinking the options window if needed.
        let desired_spacers = if notes_visible {
            // Notes already separate options from the footer, so only keep a
            // single spacer between the question and options.
            1
        } else {
            DESIRED_SPACERS_BETWEEN_SECTIONS
        };
        let required_extra = footer_pref
            .saturating_add(1) // progress line
            .saturating_add(desired_spacers);
        if remaining < required_extra {
            let deficit = required_extra.saturating_sub(remaining);
            let reducible = options_height.saturating_sub(min_options_height);
            let reduce_by = deficit.min(reducible);
            options_height = options_height.saturating_sub(reduce_by);
            remaining = remaining.saturating_add(reduce_by);
        }

        let progress_height = u16::from(remaining > 0);
        remaining = remaining.saturating_sub(progress_height);

        if !notes_visible {
            let spacer_after_options = u16::from(remaining > footer_pref);
            remaining = remaining.saturating_sub(spacer_after_options);
            let footer_lines = footer_pref.min(remaining);
            remaining = remaining.saturating_sub(footer_lines);
            let spacer_after_question = u16::from(remaining > 0);
            remaining = remaining.saturating_sub(spacer_after_question);
            let grow_by = remaining.min(full_options_height.saturating_sub(options_height));
            return LayoutPlan {
                question_height,
                progress_height,
                spacer_after_question,
                options_height: options_height.saturating_add(grow_by),
                spacer_after_options,
                notes_height: 0,
                footer_lines,
            };
        }

        let footer_lines = footer_pref.min(remaining);
        remaining = remaining.saturating_sub(footer_lines);
        // Prefer a spacer before the notes; the notes then take every remaining row.
        let spacer_after_question = u16::from(remaining > 0);
        remaining = remaining.saturating_sub(spacer_after_question);
        LayoutPlan {
            question_height,
            progress_height,
            spacer_after_question,
            options_height,
            spacer_after_options: 0,
            notes_height: remaining,
            footer_lines,
        }
    }

    /// Layout when no options are present: notes take their preferred height, then
    /// the footer and progress line, and notes absorb any leftover rows. A question
    /// that fills the area leaves every other section empty.
    fn layout_without_options(&self, area: Rect, question_height: u16) -> LayoutPlan {
        let mut remaining = area.height.saturating_sub(question_height);
        let notes_height = self.notes_input_height(area.width).min(remaining);
        remaining = remaining.saturating_sub(notes_height);
        let footer_lines = self.footer_required_height(area.width).min(remaining);
        remaining = remaining.saturating_sub(footer_lines);
        let progress_height = u16::from(remaining > 0);
        remaining = remaining.saturating_sub(progress_height);
        LayoutPlan {
            question_height,
            progress_height,
            notes_height: notes_height.saturating_add(remaining),
            footer_lines,
            ..LayoutPlan::default()
        }
    }
}
