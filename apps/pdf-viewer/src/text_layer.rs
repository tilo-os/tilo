//! The per-page **text layer** that sits on top of a rendered page.
//!
//! The raster backend paints each page as a bitmap, which alone carries no
//! machine-readable text — so search finds nothing and there is nothing to
//! select. This module holds the extracted glyph geometry ([`PageText`]) and
//! the pure logic that makes it *useful*: substring search that resolves to
//! on-page rectangles, point → glyph hit-testing for selection, and the
//! band-merging that turns a run of glyphs into the tidy highlight rectangles a
//! reader expects.
//!
//! Everything here is pure data + geometry in **page-point space** (origin at
//! the page's top-left, y increasing downward — the same convention the rest of
//! the reader uses). A real page's layer comes from the PDF engine
//! ([`tilo_pdf::TextLayer`]): its glyphs are already in reading order with
//! ligatures expanded, and copy and highlight bands go through the engine's
//! layer so hyphenation, right-to-left lines and columns behave. The
//! geometry here also works on its own, which keeps it unit-testable.

use crate::model::{PtRect, SearchOptions};

/// One extracted glyph: its character and its bounding box on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharBox {
    pub ch: char,
    /// Glyph bounding box in page-point space (origin = page top-left).
    pub rect: PtRect,
    /// Byte offset of `ch` within the owning [`PageText::text`].
    pub byte_start: usize,
}

/// The text layer of a single page: the concatenated text plus one
/// index-aligned [`CharBox`] per character, in reading order.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    pub text: String,
    pub chars: Vec<CharBox>,
    /// The engine's analysed layer this text came from, index-aligned with
    /// `chars`; `None` for synthetic text.
    pub layer: Option<std::sync::Arc<tilo_pdf::TextLayer>>,
}

/// A search hit expressed as a run of glyphs on a page. `page` is 1-based to
/// match the reader's page numbering; `char_start`/`char_len` index into
/// [`PageText::chars`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextMatch {
    pub page: usize,
    pub char_start: usize,
    pub char_len: usize,
}

impl PageText {
    /// Build a page's text layer from glyphs already extracted (character +
    /// page-point box), in reading order. Byte offsets and the concatenated
    /// `text` are derived here so the two never disagree.
    pub fn from_glyphs(glyphs: impl IntoIterator<Item = (char, PtRect)>) -> Self {
        let mut text = String::new();
        let mut chars = Vec::new();
        for (ch, rect) in glyphs {
            let byte_start = text.len();
            text.push(ch);
            chars.push(CharBox {
                ch,
                rect,
                byte_start,
            });
        }
        PageText {
            text,
            chars,
            layer: None,
        }
    }

    /// The reader's view of an engine text layer. Positions are shared: glyph
    /// `i` here is character `i` of `layer`.
    pub fn from_layer(layer: std::sync::Arc<tilo_pdf::TextLayer>) -> Self {
        let mut page = Self::from_glyphs(layer.chars().iter().map(|c| {
            (
                c.ch,
                PtRect::new(
                    f64::from(c.rect.x0),
                    f64::from(c.rect.y0),
                    f64::from(c.rect.width()),
                    f64::from(c.rect.height()),
                ),
            )
        }));
        page.layer = Some(layer);
        page
    }

    /// The engine char index (stable for a given document) of glyph `i`.
    pub fn source_index(&self, i: usize) -> Option<u32> {
        let layer = self.layer.as_ref()?;
        layer
            .chars()
            .get(i)
            .map(|c| c.source)
            .filter(|s| *s != u32::MAX)
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Find original glyph runs using the same Unicode search as the text
    /// backend. Expanded lowercase spellings map back to their source glyphs.
    pub fn search(&self, page: usize, needle: &str, opts: SearchOptions) -> Vec<TextMatch> {
        crate::model::search_text(&self.text, needle, opts, crate::model::MAX_SEARCH_RESULTS)
            .into_iter()
            .map(|range| {
                let first = self
                    .chars
                    .partition_point(|glyph| glyph.byte_start < range.start);
                let last = self
                    .chars
                    .partition_point(|glyph| glyph.byte_start < range.end);
                TextMatch {
                    page,
                    char_start: first,
                    char_len: last - first,
                }
            })
            .collect()
    }

    /// The glyph index whose box contains `(x, y)`, else the nearest glyph on
    /// the same visual row within `row_tol` points, else `None`. This is the
    /// hit-test a selection drag uses to find where the caret landed.
    pub fn char_at(&self, x: f64, y: f64) -> Option<usize> {
        if let Some(layer) = &self.layer {
            return layer.hit_test(tilo_pdf::Point::new(x as f32, y as f32));
        }
        // Exact containment first.
        for (i, c) in self.chars.iter().enumerate() {
            if c.rect.w > 0.0 && c.rect.h > 0.0 && c.rect.contains(x, y) {
                return Some(i);
            }
        }
        // Nearest glyph on the row the point falls within.
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in self.chars.iter().enumerate() {
            if c.rect.h <= 0.0 {
                continue;
            }
            if y >= c.rect.y - 2.0 && y <= c.rect.y + c.rect.h + 2.0 {
                let cx = c.rect.x + c.rect.w / 2.0;
                let d = (cx - x).abs();
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// The glyph nearest `(x, y)` anywhere on the page (Euclidean distance to
    /// the glyph centre). Used as the selection fallback when a drag runs past
    /// the last line so the endpoint still resolves to a real glyph.
    pub fn nearest_char(&self, x: f64, y: f64) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in self.chars.iter().enumerate() {
            if c.rect.h <= 0.0 {
                continue;
            }
            let (cx, cy) = c.rect.center();
            let d = (cx - x).hypot(cy - y);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Merge the glyph run `[start, end)` into one rectangle per visual row —
    /// the tidy bands a reader draws for a selection or a search hit. Rows are
    /// split where a glyph's vertical band no longer overlaps the running row.
    pub fn bands_for_range(&self, start: usize, end: usize) -> Vec<PtRect> {
        if let Some(layer) = &self.layer {
            return layer
                .rects_for(&layer.select_range(start..end))
                .into_iter()
                .map(|r| {
                    PtRect::new(
                        f64::from(r.x0),
                        f64::from(r.y0),
                        f64::from(r.width()),
                        f64::from(r.height()),
                    )
                })
                .collect();
        }
        let end = end.min(self.chars.len());
        if start >= end {
            return Vec::new();
        }
        let mut bands: Vec<PtRect> = Vec::new();
        let Some(range) = self.chars.get(start..end) else {
            // Unreachable given the guards above (end <= len, start < end);
            // stay safe rather than assume it.
            return bands;
        };
        let mut cur: Option<PtRect> = None;
        for c in range {
            let r = c.rect;
            if r.w <= 0.0 || r.h <= 0.0 {
                continue;
            }
            match cur.as_mut() {
                Some(b) => {
                    // Same row when the vertical spans overlap by any amount.
                    let same_row = r.y < b.y + b.h && r.y + r.h > b.y;
                    if same_row {
                        let x0 = b.x.min(r.x);
                        let x1 = (b.x + b.w).max(r.x + r.w);
                        let y0 = b.y.min(r.y);
                        let y1 = (b.y + b.h).max(r.y + r.h);
                        *b = PtRect::new(x0, y0, x1 - x0, y1 - y0);
                    } else {
                        bands.push(*b);
                        cur = Some(r);
                    }
                }
                None => cur = Some(r),
            }
        }
        if let Some(b) = cur {
            bands.push(b);
        }
        bands
    }

    /// The half-open glyph range `[start, end)` of the **word** containing the
    /// glyph at `glyph_idx`, resolved through the shared `ui::word_boundaries`
    /// core so double-click word selection behaves identically to every text
    /// field and the ebook reader. Falls back to the single glyph when it sits
    /// on a boundary with no surrounding word (e.g. isolated punctuation).
    pub fn word_range(&self, glyph_idx: usize) -> (usize, usize) {
        if self.chars.is_empty() {
            return (0, 0);
        }
        let g = glyph_idx.min(self.chars.len() - 1);
        let Some(byte) = self.chars.get(g).map(|c| c.byte_start) else {
            // Unreachable: `g` is clamped to a valid index by the guards above.
            return (0, 0);
        };
        let (bs, be) = ui::word_boundaries(&self.text, byte);
        let gs = self.chars.partition_point(|c| c.byte_start < bs);
        let ge = self.chars.partition_point(|c| c.byte_start < be);
        if ge > gs { (gs, ge) } else { (g, g + 1) }
    }

    /// The half-open glyph range `[start, end)` of the **visual row** (line)
    /// containing the glyph at `glyph_idx` — the contiguous run of glyphs whose
    /// vertical band overlaps that glyph's row. Drives triple-click line
    /// selection. Visual rows are used rather than `\n` boundaries because an
    /// extracted PDF text layer routinely carries no newlines; a degenerate
    /// (zero-size) glyph such as an inter-word space never breaks the run.
    pub fn row_range(&self, glyph_idx: usize) -> (usize, usize) {
        if self.chars.is_empty() {
            return (0, 0);
        }
        let g = glyph_idx.min(self.chars.len() - 1);
        let Some(a) = self.chars.get(g).map(|c| c.rect) else {
            // Unreachable: `g` is clamped to a valid index by the guard above.
            return (0, 0);
        };
        let cont = |r: &PtRect| r.w <= 0.0 || r.h <= 0.0 || (r.y < a.y + a.h && r.y + r.h > a.y);
        let mut s = g;
        while s > 0 && self.chars.get(s - 1).is_some_and(|c| cont(&c.rect)) {
            s -= 1;
        }
        let mut e = g + 1;
        while self.chars.get(e).is_some_and(|c| cont(&c.rect)) {
            e += 1;
        }
        (s, e)
    }

    /// The text of the glyph run `[start, end)` — what a copy of the selection
    /// yields.
    pub fn text_for_range(&self, start: usize, end: usize) -> String {
        if let Some(layer) = &self.layer {
            return layer.text_for(&layer.select_range(start..end));
        }
        let end = end.min(self.chars.len());
        if start >= end {
            return String::new();
        }
        self.chars
            .get(start..end)
            .into_iter()
            .flatten()
            .map(|c| c.ch)
            .collect()
    }
}

/// Search every page's text layer, returning hits in document order. `pages`
/// is indexed 0-based; the emitted [`TextMatch::page`] is 1-based.
pub fn search_pages(pages: &[PageText], query: &str, opts: SearchOptions) -> Vec<TextMatch> {
    let mut out = Vec::new();
    if query.trim().is_empty() {
        return out;
    }
    for (i, pt) in pages.iter().enumerate() {
        let remaining = crate::model::MAX_SEARCH_RESULTS.saturating_sub(out.len());
        out.extend(pt.search(i + 1, query, opts).into_iter().take(remaining));
        if out.len() >= crate::model::MAX_SEARCH_RESULTS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rows of glyphs: "PDF text " on row 1 (y≈10) and "layer" on row 2
    /// (y≈30). Each glyph is 8pt wide, advancing left-to-right. A space glyph
    /// ends row 1 so "text" and "layer" are distinct words (a real extracted
    /// layer carries the inter-word space), which the whole-word test needs.
    fn sample_layer() -> PageText {
        let mut glyphs = Vec::new();
        let mut x = 10.0;
        for ch in "PDF text".chars() {
            glyphs.push((ch, PtRect::new(x, 10.0, 8.0, 12.0)));
            x += 8.0;
        }
        // Inter-word space on row 1 (a real word boundary between "text" and
        // "layer").
        glyphs.push((' ', PtRect::new(x, 10.0, 8.0, 12.0)));
        x = 10.0;
        for ch in "layer".chars() {
            glyphs.push((ch, PtRect::new(x, 30.0, 8.0, 12.0)));
            x += 8.0;
        }
        PageText::from_glyphs(glyphs)
    }

    #[test]
    fn from_glyphs_builds_aligned_text_and_offsets() {
        let pt = sample_layer();
        assert_eq!(pt.text, "PDF text layer");
        assert_eq!(pt.chars.len(), pt.text.chars().count());
        // Byte offsets are contiguous and index the concatenated text.
        for c in &pt.chars {
            assert_eq!(pt.text[c.byte_start..].chars().next(), Some(c.ch));
        }
    }

    #[test]
    fn search_finds_word_and_maps_to_real_glyph_rects() {
        let pt = sample_layer();
        let hits = pt.search(1, "text", SearchOptions::default());
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        // The matched glyph run reads back as the query.
        assert_eq!(
            pt.text_for_range(m.char_start, m.char_start + m.char_len),
            "text"
        );
        // And resolves to a single band on row 1, spanning the four glyphs.
        let bands = pt.bands_for_range(m.char_start, m.char_start + m.char_len);
        assert_eq!(bands.len(), 1, "one row -> one band");
        assert!(
            (bands[0].w - 32.0).abs() < 0.01,
            "band spans four 8pt glyphs"
        );
        assert!((bands[0].y - 10.0).abs() < 0.01, "band sits on row 1");
    }

    #[test]
    fn search_is_case_insensitive_by_default_and_case_sensitive_on_request() {
        let pt = sample_layer();
        assert_eq!(pt.search(1, "PDF", SearchOptions::default()).len(), 1);
        assert_eq!(pt.search(1, "pdf", SearchOptions::default()).len(), 1);
        let cs = SearchOptions {
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(
            pt.search(1, "pdf", cs).len(),
            0,
            "case-sensitive misses lowercase"
        );
        assert_eq!(pt.search(1, "PDF", cs).len(), 1);
    }

    #[test]
    fn unicode_search_maps_expanded_spelling_to_original_glyphs() {
        let text = "İ test K TEST";
        let page = PageText::from_glyphs(
            text.chars()
                .enumerate()
                .map(|(index, ch)| (ch, PtRect::new(index as f64 * 8.0, 0.0, 8.0, 12.0))),
        );
        let hits = page.search(2, "test", SearchOptions::default());
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0],
            TextMatch {
                page: 2,
                char_start: 2,
                char_len: 4
            }
        );
        assert_eq!(
            hits[1],
            TextMatch {
                page: 2,
                char_start: 9,
                char_len: 4
            }
        );
        for (query, expected, start) in [("i\u{307}", "İ", 0), ("k", "K", 7)] {
            let hits = page.search(2, query, SearchOptions::default());
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].char_start, start);
            assert_eq!(hits[0].char_len, 1);
            assert_eq!(page.text_for_range(start, start + 1), expected);
            let bands = page.bands_for_range(start, start + 1);
            assert_eq!(bands.len(), 1);
            assert_eq!(bands[0].w, 8.0, "highlight covers exactly the source glyph");
        }
    }

    #[test]
    fn unicode_whole_word_search_rejects_accented_word_fragments() {
        let page = PageText::from_glyphs(
            "étest testé e\u{301}test test\u{301} test"
                .chars()
                .map(|ch| (ch, PtRect::new(0.0, 0.0, 8.0, 12.0))),
        );
        let hits = page.search(
            1,
            "test",
            SearchOptions {
                whole_word: true,
                case_sensitive: false,
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].char_start, page.chars.len() - 4);
        assert_eq!(hits[0].char_len, 4);
    }

    #[test]
    fn search_materialization_is_bounded() {
        let text = "a".repeat(crate::model::MAX_SEARCH_RESULTS * 4);
        let page =
            PageText::from_glyphs(text.chars().map(|ch| (ch, PtRect::new(0.0, 0.0, 1.0, 1.0))));
        let hits = page.search(1, "a", SearchOptions::default());
        assert_eq!(hits.len(), crate::model::MAX_SEARCH_RESULTS);
        assert_eq!(hits.last().unwrap().char_start, hits.len() - 1);
        let pages = vec![page.clone(), page];
        let hits = search_pages(&pages, "a", SearchOptions::default());
        assert_eq!(hits.len(), crate::model::MAX_SEARCH_RESULTS);
        assert!(hits.iter().all(|hit| hit.page == 1));
    }

    #[test]
    fn whole_word_rejects_substrings() {
        // "layer" contains "aye"; whole-word must reject it, substring accept.
        let pt = sample_layer();
        assert_eq!(pt.search(1, "aye", SearchOptions::default()).len(), 1);
        let ww = SearchOptions {
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(pt.search(1, "aye", ww).len(), 0);
        assert_eq!(
            pt.search(1, "layer", ww).len(),
            1,
            "the full word still matches"
        );
    }

    #[test]
    fn char_at_hits_the_glyph_under_the_point() {
        let pt = sample_layer();
        // Centre of the first 'P' at (10,10)+(4,6).
        let i = pt.char_at(14.0, 16.0).expect("a glyph under the point");
        assert_eq!(pt.chars[i].ch, 'P');
        // A point on row 2 near its start resolves to a row-2 glyph.
        let j = pt.char_at(12.0, 36.0).expect("row-2 glyph");
        assert!((pt.chars[j].rect.y - 30.0).abs() < 0.01);
    }

    #[test]
    fn bands_split_across_rows() {
        // Selecting the whole layer yields two bands, one per row.
        let pt = sample_layer();
        let bands = pt.bands_for_range(0, pt.chars.len());
        assert_eq!(bands.len(), 2);
        assert!(bands[0].y < bands[1].y);
    }

    #[test]
    fn search_pages_reports_one_based_page_numbers() {
        let pages = vec![PageText::default(), sample_layer()];
        let hits = search_pages(&pages, "layer", SearchOptions::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, 2, "second page is page 2, not index 1");
    }

    #[test]
    fn empty_query_finds_nothing() {
        let pt = sample_layer();
        assert!(pt.search(1, "   ", SearchOptions::default()).is_empty());
        assert!(search_pages(&[pt], "", SearchOptions::default()).is_empty());
    }

    #[test]
    fn word_range_snaps_to_the_word_under_the_glyph() {
        // text "PDF text layer": glyphs 0..3="PDF", 3=' ', 4..8="text",
        // 8=' ', 9..14="layer". Double-clicking inside "text" selects it.
        let pt = sample_layer();
        // A glyph inside "text" (index 5 = 'e') snaps to the whole word 4..8.
        assert_eq!(pt.word_range(5), (4, 8));
        assert_eq!(pt.text_for_range(4, 8), "text");
        // Inside "PDF" (index 1).
        assert_eq!(pt.word_range(1), (0, 3));
        assert_eq!(pt.text_for_range(0, 3), "PDF");
        // Inside "layer" on row 2 (index 10).
        let (s, e) = pt.word_range(10);
        assert_eq!(pt.text_for_range(s, e), "layer");
    }

    #[test]
    fn row_range_selects_the_whole_visual_line() {
        // Row 1 is "PDF text" + the trailing space (glyphs 0..=8); row 2 is
        // "layer" (glyphs 8..13, 0-based 8..13). A triple-click on row 1 selects
        // the whole first line across the inter-word space, not just one word.
        let pt = sample_layer();
        let (s, e) = pt.row_range(1); // inside "PDF" on row 1
        assert_eq!(s, 0, "row starts at the first glyph");
        // Row 1 covers "PDF text " — the eight letters + the space glyph.
        assert_eq!(pt.text_for_range(s, e).trim_end(), "PDF text");
        // A triple-click on row 2 stays on row 2.
        let (s2, _) = pt.row_range(10);
        assert!(s2 >= 9, "row 2 does not bleed into row 1: {s2}");
    }
}
