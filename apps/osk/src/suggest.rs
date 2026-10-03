//! The suggestion bar — a row of tappable word chips above the keys.
//!
//! After a swipe it shows the ranked decode candidates; after a word commits
//! it shows next-word predictions. The top chip is the "auto" pick (the one
//! already inserted, after a swipe); tapping any chip replaces the current
//! word with it. A word that names an emoji ("pizza") also gets emoji chips,
//! which insert the emoji after the word.
//!
//! This module holds the **pure geometry** + the data model, mirroring
//! [`crate::toolbar`]: [`suggestion_rects`] splits a row into evenly-spaced
//! chips so the widget can hit-test taps the same way it does for keys and
//! toolbar items. Rendering + theming live in [`crate::widget`].

use vello::kurbo::Rect;

/// One suggestion chip.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// The word the chip inserts.
    pub word: String,
    /// Decode/prediction score in `0.0..=1.0` (for ordering / future styling).
    pub score: f32,
    /// The chip inserts an emoji the word stands for rather than a word.
    pub emoji: bool,
}

impl Suggestion {
    pub fn new(word: impl Into<String>, score: f32) -> Self {
        Self {
            word: word.into(),
            score,
            emoji: false,
        }
    }

    /// A chip inserting `emoji` for the word being typed.
    pub fn emoji(emoji: impl Into<String>) -> Self {
        Self {
            word: emoji.into(),
            score: 0.0,
            emoji: true,
        }
    }
}

/// **Pure geometry** for the suggestion bar: split `area` into `n` equal chips
/// in order, returning each chip's index + rect. Returns empty for an empty
/// area or no suggestions (so the widget can collapse the row).
pub fn suggestion_rects(suggestions: &[Suggestion], area: Rect) -> Vec<(usize, Rect)> {
    let n = suggestions.len();
    if n == 0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return Vec::new();
    }
    let unit_w = area.width() / n as f64;
    let mut out = Vec::with_capacity(n);
    let mut x = area.x0;
    for i in 0..n {
        // Accumulate to avoid rounding gaps; last chip ends exactly at x1.
        let x1 = if i + 1 == n { area.x1 } else { x + unit_w };
        out.push((i, Rect::new(x, area.y0, x1, area.y1)));
        x += unit_w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sugg(words: &[&str]) -> Vec<Suggestion> {
        words
            .iter()
            .enumerate()
            .map(|(i, w)| Suggestion::new(*w, 1.0 - i as f32 * 0.1))
            .collect()
    }

    #[test]
    fn suggestion_chip_rects_split_evenly_and_fill_width() {
        eprintln!("phase rects_split_evenly_and_fill_width");
        {
            let s = sugg(&["quick", "quack", "quiche"]);
            let rects = suggestion_rects(&s, Rect::new(0.0, 0.0, 600.0, 40.0));
            assert_eq!(rects.len(), 3);
            for (_, r) in &rects {
                assert!((r.width() - 200.0).abs() < 1.0);
            }
            assert!((rects.last().unwrap().1.x1 - 600.0).abs() < 1e-6);
            assert!((rects.first().unwrap().1.x0 - 0.0).abs() < 1e-6);
        }

        eprintln!("phase no_suggestions_yields_no_rects");
        {
            assert!(suggestion_rects(&[], Rect::new(0.0, 0.0, 600.0, 40.0)).is_empty());
            let s = sugg(&["x"]);
            assert!(suggestion_rects(&s, Rect::new(0.0, 0.0, 0.0, 40.0)).is_empty());
        }

        eprintln!("phase single_chip_fills_the_row");
        {
            let s = sugg(&["hello"]);
            let rects = suggestion_rects(&s, Rect::new(10.0, 0.0, 610.0, 40.0));
            assert_eq!(rects.len(), 1);
            assert_eq!(rects[0].1, Rect::new(10.0, 0.0, 610.0, 40.0));
        }
    }
}
