//! Spacing and geometry ratchet: the pixel literals the `Theme` should own.
//!
//! `design_token_audit.rs` ratchets colour escape hatches; this file does
//! the same for spacing in widget, chrome, shell and app code. Counts live in
//! `tests/baselines/spacing/<unit>.txt` (see `support::ratchet`) and may only
//! fall.
//!
//! Four shapes are counted, all outside comments and `#[cfg(test)]` items:
//!
//! * `pixel_const` — a module-level `const NAME: f32|f64 = <whole number ≥ 1>;`.
//!   Whole numbers because pixel constants are whole; `0.1`, `0.9` and
//!   `1.25` are rates, fractions and ratios and are not counted. Arithmetic
//!   (`PAD + CTRL_H`, `x / 2.0`) is not a literal and is not counted.
//! * `taffy_length` — a non-zero literal handed to taffy's `length(...)`
//!   inside a `Style` (`padding`, `gap`, `margin`, `size`). `length(0.0)`
//!   is a deliberate "none" and is not counted; `length(theme.gap_standard)`
//!   is what the ratchet wants and is not a literal.
//! * `literal_gap` — a literal handed to a container's `with_gap`,
//!   `with_padding`, `with_row_gap` or `with_column_gap`.
//! * `literal_spacer` — `Spacer::fixed(<literal>)`.
//!
//! A new hit is a request to read the value from `theme.tokens.spacing` /
//! `theme.tokens.metrics` or to let the composition own the space.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use regex::Captures;

use crate::support::ratchet::{Pattern, Ratchet, no_exemptions};

/// `"12"`, `"12.0"`, `"12.00"` → a whole pixel count of at least one.
/// `"0"`, `"0.0"`, `"0.5"`, `"1.25"` → not a pixel constant.
fn is_whole_pixel_literal(literal: &str) -> bool {
    let (int, frac) = literal.split_once('.').unwrap_or((literal, ""));
    int.parse::<u64>().is_ok_and(|v| v >= 1) && frac.chars().all(|c| c == '0')
}

/// `"0"`, `"0.0"`, `"0.00"` → the deliberate zero; anything else is a value.
fn is_nonzero_literal(literal: &str) -> bool {
    literal.chars().any(|c| c.is_ascii_digit() && c != '0')
}

fn whole_pixel(c: &Captures<'_>) -> bool {
    is_whole_pixel_literal(&c[1])
}

fn nonzero(c: &Captures<'_>) -> bool {
    is_nonzero_literal(&c[1])
}

/// Spacing and geometry literals in widget, chrome, shell and app
/// production code.
pub fn spacing_ratchet() -> Ratchet {
    Ratchet {
        name: "spacing",
        roots: &[
            "crates/sdk/ui-core/src/widget",
            "crates/sdk/ui-core/src/chrome",
            "crates/session/shell/src",
            "apps/*/src",
        ],
        patterns: vec![
            Pattern::new(
                "pixel_const",
                r"^\s*(?:pub(?:\([a-z]+\))?\s+)?const\s+[A-Z][A-Z0-9_]*\s*:\s*f(?:32|64)\s*=\s*(\d+(?:\.\d+)?)(?:_f(?:32|64))?\s*;",
                "module-level pixel constant — read it from theme.tokens.spacing / theme.tokens.metrics",
            )
            .keep(whole_pixel),
            Pattern::new(
                "taffy_length",
                r"\blength\(\s*(\d+(?:\.\d+)?)(?:_f(?:32|64))?\s*\)",
                "literal taffy length in a Style — pass a spacing token (insets_all / insets_xy take one)",
            )
            .keep(nonzero),
            Pattern::new(
                "literal_gap",
                // Zero is "no space", not a spacing value, so it is not counted.
                r"\.with_(?:gap|padding|row_gap|column_gap)\(\s*(?:\d*[1-9]\d*(?:\.\d+)?|\d+\.\d*[1-9]\d*)\s*\)",
                "literal gap or padding on a container — pass a spacing token, or drop it and \
                 let the composition space its children",
            ),
            Pattern::new(
                "literal_spacer",
                r"Spacer::fixed\(\s*\d",
                "fixed literal spacer — space comes from the composition or a spacing token",
            ),
        ],
        exempt: no_exemptions,
        advice: "Read the value from theme.tokens.spacing or theme.tokens.metrics (insets_all / \
                 insets_xy build a padding Rect from one), or let the composition own it.",
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn spacing_audit_sees_what_it_polices_and_literals_do_not_grow() {
    // Case `spacing_patterns_match_the_shapes_they_claim`.
    // The scanner must be able to see what it polices. Each rule is exercised
    // on a line that must count and one that must not, so a regex edit that
    // silently stops matching fails here instead of passing an empty audit.
    {
        let ratchet = spacing_ratchet();
        let pixel = ratchet.pattern("pixel_const");
        let length = ratchet.pattern("taffy_length");
        let gap = ratchet.pattern("literal_gap");
        let spacer = ratchet.pattern("literal_spacer");
        let count = |p: &Pattern, line: &str| p.hits(line);

        assert_eq!(count(pixel, "const PAD: f64 = 12.0;"), 1);
        assert_eq!(
            count(pixel, "pub const DEFAULT_SHELL_BREAKPOINT: f64 = 720.0;"),
            1
        );
        assert_eq!(count(pixel, "pub(crate) const BAR: f32 = 8.0_f32;"), 1);
        assert_eq!(count(pixel, "    const CHIP_SIZE: f64 = 36.0;"), 1);
        assert_eq!(
            count(pixel, "const MAX_FRAME_DT: f64 = 0.1;"),
            0,
            "a rate is not a pixel"
        );
        assert_eq!(
            count(pixel, "const OVERDRAW: f64 = 1.25;"),
            0,
            "a ratio is not a pixel"
        );
        assert_eq!(
            count(pixel, "const NONE: f32 = 0.0;"),
            0,
            "zero is the deliberate absence"
        );
        assert_eq!(
            count(pixel, "const BAND: f64 = PAD + CTRL_H + PAD;"),
            0,
            "arithmetic is not a literal"
        );
        assert_eq!(count(pixel, "const LIMIT: usize = 8;"), 0, "not a float");
        assert_eq!(
            count(pixel, "let pad: f32 = 12.0;"),
            0,
            "a local is not a module constant"
        );

        assert_eq!(count(length, "left: length(12.0),"), 1);
        assert_eq!(
            count(length, "width: length(8.0_f32), height: length(8.0_f32),"),
            2
        );
        assert_eq!(count(length, "size: length(320.0)"), 1);
        assert_eq!(
            count(length, "length(0.5)"),
            1,
            "a half-pixel hairline is still a literal"
        );
        assert_eq!(
            count(length, "left: length(0.0_f32),"),
            0,
            "zero is the deliberate absence"
        );
        assert_eq!(
            count(length, "length(theme.gap_standard)"),
            0,
            "a token is the goal"
        );
        assert_eq!(count(length, "length(pad)"), 0);
        assert_eq!(
            count(length, "length(w / 2.0)"),
            0,
            "arithmetic is not a literal"
        );
        assert_eq!(count(length, "Dimension::length(24.0)"), 1);

        assert_eq!(count(gap, "Row::new().with_gap(12.0)"), 1);
        assert_eq!(count(gap, "col.with_padding(16)"), 1);
        assert_eq!(count(gap, "grid.with_row_gap( 8.0 )"), 1);
        assert_eq!(count(gap, "grid.with_column_gap(4.0)"), 1);
        assert_eq!(count(gap, "row.with_gap(0.5)"), 1);
        assert_eq!(count(gap, "row.with_gap(10)"), 1);
        assert_eq!(
            count(gap, "Column::new().with_gap(0.0)"),
            0,
            "no gap is not a spacing value"
        );
        assert_eq!(
            count(gap, "col.with_padding(0)"),
            0,
            "no padding is not a spacing value"
        );
        assert_eq!(
            count(gap, "Row::new().with_gap(spacing.s3)"),
            0,
            "a token is the goal"
        );
        assert_eq!(
            count(gap, "with_padding(pad * 2.0)"),
            0,
            "arithmetic is not a literal"
        );
        assert_eq!(count(spacer, "Spacer::fixed(12.0)"), 1);
        assert_eq!(count(spacer, "Spacer::fixed(spacing.s4)"), 0);
        assert_eq!(count(spacer, "Spacer::flex()"), 0);
    }

    // Case `spacing_literals_do_not_grow`.
    // The `#[cfg(test)]` span finder's shapes are pinned once in
    // `support::ratchet` (every ratchet scans through the same finder).
    // Spacing literals in widget, chrome, shell and app production code may
    // not grow; the engine also fails if any root yields no file.
    spacing_ratchet().check();
}
