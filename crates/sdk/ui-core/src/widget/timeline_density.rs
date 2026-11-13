//! How much a time grid (a calendar's week, a schedule) shows of each item, decided from the width
//! of one day column and the theme's type, so every app that lays out days in columns follows one
//! rule.
use crate::theme::Theme;
use crate::tokens::TextRole;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimelineDensity {
    #[default]
    Regular,
    Narrow,
}

impl TimelineDensity {
    #[must_use]
    pub fn narrow_below(theme: &Theme) -> f64 {
        10.0 * f64::from(theme.text(TextRole::Caption).size)
    }

    #[must_use]
    pub fn of(column_width: f64, theme: &Theme) -> Self {
        if column_width < Self::narrow_below(theme) {
            Self::Narrow
        } else {
            Self::Regular
        }
    }

    #[must_use]
    pub const fn short_block_lines(self) -> usize {
        match self {
            Self::Regular => 1,
            Self::Narrow => 2,
        }
    }

    #[must_use]
    pub const fn chip_lines(self) -> usize {
        self.short_block_lines()
    }

    #[must_use]
    pub const fn chip_marker(self) -> bool {
        matches!(self, Self::Regular)
    }

    /// The shortest an hour row gets: a half hour holds
    /// [`Self::short_block_lines`] lines of `title_line` and an `s1` of
    /// padding (a block's smallest height), and an hour is never shorter
    /// than a list row grown by the text scale ([`Theme::text_growth`]).
    #[must_use]
    pub fn min_hour_height(self, theme: &Theme, title_line: f64) -> f64 {
        let pad = f64::from(theme.tokens.spacing.s1);
        #[allow(clippy::cast_precision_loss, reason = "at most two lines")]
        let half_hour = self.short_block_lines() as f64 * title_line + pad;
        let row = f64::from(theme.tokens.metrics.panel_row_height) * f64::from(theme.text_growth());
        (2.0 * half_hour).max(row).ceil()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_column_gives_short_blocks_two_lines_and_a_taller_hour() {
        let theme = Theme::dark();
        let line = f64::from(theme.text(TextRole::Caption).line_height_px());
        let edge = TimelineDensity::narrow_below(&theme);
        let narrow = TimelineDensity::of(edge - 1.0, &theme);
        let regular = TimelineDensity::of(edge + 1.0, &theme);
        assert_eq!(narrow, TimelineDensity::Narrow);
        assert_eq!(regular, TimelineDensity::Regular);
        assert!(
            narrow.min_hour_height(&theme, line) >= 2.0 * (2.0 * line),
            "a narrow half hour holds two title lines"
        );
        assert!(narrow.min_hour_height(&theme, line) > regular.min_hour_height(&theme, line));
        assert!(!narrow.chip_marker() && regular.chip_marker());
    }

    #[test]
    fn the_hour_grows_with_the_text_scale() {
        let one = Theme::dark();
        let two = Theme::dark().with_text_scale(2.0);
        let line = |t: &Theme| f64::from(t.text(TextRole::Caption).line_height_px());
        for density in [TimelineDensity::Regular, TimelineDensity::Narrow] {
            let at_one = density.min_hour_height(&one, line(&one));
            let at_two = density.min_hour_height(&two, line(&two));
            assert!(
                at_two >= 1.5 * at_one,
                "{density:?}: an hour at 2x text is {at_two}, at 1x {at_one}"
            );
        }
        assert!(TimelineDensity::narrow_below(&two) > TimelineDensity::narrow_below(&one));
    }
}
