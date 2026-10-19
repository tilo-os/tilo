//! Density as a token mode (`docs/design-language.md`, "Space and
//! density").
//!
//! A window, or one region of it (a sidebar, a table, a list of files),
//! runs at [`Density::Comfortable`] or [`Density::Compact`]. Compact scales
//! the spacing rhythm by [`Density::COMPACT_RHYTHM`] and shortens rows to
//! 28 / 44 / 52; control heights and the text size stay, so a compact
//! region still has the same pointer targets and reads at the same size.
//! An app chooses compact for a data-heavy region (Monitor's process list,
//! the Files list view, a table) with [`crate::Theme::with_density`]; it
//! never crams by hand.

/// How tightly a window or region packs its rows and gaps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Density {
    /// The stock density.
    #[default]
    Comfortable,
    /// Data-heavy content: rhythm × 0.75, rows 28 / 44 / 52.
    Compact,
}

/// The heights of the three kinds of list row, in logical pixels at 1×.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowHeights {
    /// One line: a title, maybe a trailing value or control.
    pub single: f32,
    /// A title and a secondary line.
    pub double: f32,
    /// A row led by a 40 px avatar or icon.
    pub avatar: f32,
}

impl Density {
    /// How much compact scales the spacing rhythm by.
    pub const COMPACT_RHYTHM: f32 = 0.75;

    /// Row heights at 1×.
    #[must_use]
    pub const fn rows(self) -> RowHeights {
        match self {
            Self::Comfortable => RowHeights {
                single: 36.0,
                double: 52.0,
                avatar: 64.0,
            },
            Self::Compact => RowHeights {
                single: 28.0,
                double: 44.0,
                avatar: 52.0,
            },
        }
    }

    /// The factor this density applies to the spacing rhythm.
    #[must_use]
    pub const fn rhythm_factor(self) -> f32 {
        match self {
            Self::Comfortable => 1.0,
            Self::Compact => Self::COMPACT_RHYTHM,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_tightens_rows_and_rhythm_and_keeps_controls() {
        let comfy = crate::Theme::dark();
        let compact = comfy.clone().with_density(Density::Compact);
        assert_eq!(comfy.tokens.density, Density::Comfortable);
        assert_eq!(compact.tokens.density, Density::Compact);
        let m = |t: &crate::Theme| {
            (
                t.tokens.metrics.row_single,
                t.tokens.metrics.row_double,
                t.tokens.metrics.row_avatar,
            )
        };
        assert_eq!(m(&comfy), (36.0, 52.0, 64.0));
        assert_eq!(m(&compact), (28.0, 44.0, 52.0));
        let r = |t: &crate::Theme| t.tokens.spacing.rhythm;
        assert!((r(&compact).sibling - r(&comfy).sibling * 0.75).abs() < 1e-4);
        assert!((r(&compact).section - 12.0).abs() < 1e-4);
        // Control heights and text stay.
        assert!(
            (compact.tokens.metrics.min_pointer_target - comfy.tokens.metrics.min_pointer_target)
                .abs()
                < f32::EPSILON
        );
        assert_eq!(
            compact.text(crate::tokens::TextRole::Body),
            comfy.text(crate::tokens::TextRole::Body)
        );
        // Going back restores the comfortable tokens exactly, and applying a
        // density twice is applying it once.
        let back = compact.clone().with_density(Density::Comfortable);
        assert_eq!(m(&back), m(&comfy));
        assert_eq!(r(&back), r(&comfy));
        let twice = compact.clone().with_density(Density::Compact);
        assert_eq!(m(&twice), m(&compact));
        // A UI scale scales the compact rows too.
        let big = crate::Theme::dark()
            .with_ui_scale(2.0)
            .with_density(Density::Compact);
        assert!((big.tokens.metrics.row_single - 56.0).abs() < 1e-3);
    }
}
