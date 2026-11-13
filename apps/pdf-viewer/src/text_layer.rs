//! A page's text layer over its bitmap: the extracted glyph geometry ([`PageText`])
//! and the pure logic that makes it usable — search that resolves to on-page
//! rectangles, point-to-glyph hit-testing and the band merging a highlight needs.
//! Everything is in page-point space, origin at the page's top-left and y downward.

use crate::model::{PtRect, SearchOptions};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharBox {
    pub ch: char,
    pub rect: PtRect,
    pub byte_start: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PageText {
    pub text: String,
    pub chars: Vec<CharBox>,
    pub layer: Option<std::sync::Arc<tilo_pdf::TextLayer>>,
    pub map: Option<crate::model::PageMap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextMatch {
    pub page: usize,
    pub char_start: usize,
    pub char_len: usize,
}

impl PageText {
    /// Build a layer from glyphs in reading order; byte offsets and the text are
    /// derived here, so the two never disagree.
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
            map: None,
        }
    }

    #[must_use]
    pub fn with_map(&self, map: crate::model::PageMap) -> Self {
        if map.is_identity() {
            return self.clone();
        }
        let mut out = self.clone();
        for c in &mut out.chars {
            if c.rect.w > 0.0 || c.rect.h > 0.0 {
                c.rect = map.layer_rect_to_card(&c.rect);
            }
        }
        out.map = Some(map);
        out
    }

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

    pub fn range_between(&self, a: (f64, f64), b: (f64, f64)) -> Option<(usize, usize)> {
        let layer = self.layer.as_ref()?;
        let to_layer = |p: (f64, f64)| {
            let (x, y) = self.map.map_or(p, |m| m.card_to_layer(p));
            tilo_pdf::Point::new(x as f32, y as f32)
        };
        let span = layer.select_between(to_layer(a), to_layer(b)).span()?;
        (span.end > span.start).then_some((span.start, span.end))
    }

    pub fn source_range(&self, start: usize, end: usize) -> Option<(u32, u32)> {
        let sources: Vec<u32> = (start..end).filter_map(|i| self.source_index(i)).collect();
        let first = sources.iter().min()?;
        let last = sources.iter().max()?;
        Some((*first, last.saturating_add(1)))
    }

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

    pub fn char_at(&self, x: f64, y: f64) -> Option<usize> {
        if let Some(layer) = &self.layer {
            let (x, y) = self.map.map_or((x, y), |m| m.card_to_layer((x, y)));
            return layer.hit_test(tilo_pdf::Point::new(x as f32, y as f32));
        }
        for (i, c) in self.chars.iter().enumerate() {
            if c.rect.w > 0.0 && c.rect.h > 0.0 && c.rect.contains(x, y) {
                return Some(i);
            }
        }
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

    pub fn bands_for_range(&self, start: usize, end: usize) -> Vec<PtRect> {
        if let Some(layer) = &self.layer {
            return layer
                .rects_for(&layer.select_range(start..end))
                .into_iter()
                .map(|r| {
                    let rect = PtRect::new(
                        f64::from(r.x0),
                        f64::from(r.y0),
                        f64::from(r.width()),
                        f64::from(r.height()),
                    );
                    self.map.map_or(rect, |m| m.layer_rect_to_card(&rect))
                })
                .collect();
        }
        let end = end.min(self.chars.len());
        if start >= end {
            return Vec::new();
        }
        let mut bands: Vec<PtRect> = Vec::new();
        let Some(range) = self.chars.get(start..end) else {
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

    pub fn word_range(&self, glyph_idx: usize) -> (usize, usize) {
        if self.chars.is_empty() {
            return (0, 0);
        }
        if let Some(layer) = &self.layer {
            // The engine knows its lines: a word never runs across one.
            let range = layer.word_range(glyph_idx.min(self.chars.len() - 1));
            return (range.start, range.end);
        }
        let g = glyph_idx.min(self.chars.len() - 1);
        let Some(byte) = self.chars.get(g).map(|c| c.byte_start) else {
            return (0, 0);
        };
        let (bs, be) = ui::word_boundaries(&self.text, byte);
        let gs = self.chars.partition_point(|c| c.byte_start < bs);
        let ge = self.chars.partition_point(|c| c.byte_start < be);
        if ge > gs { (gs, ge) } else { (g, g + 1) }
    }

    /// The half-open glyph range of the visual row containing `glyph_idx`, which a
    /// triple click selects. Visual rows are used because an extracted layer often
    /// carries no newlines, and a zero-size glyph never breaks a run.
    pub fn row_range(&self, glyph_idx: usize) -> (usize, usize) {
        if self.chars.is_empty() {
            return (0, 0);
        }
        if let Some(layer) = &self.layer {
            let mut range = layer.line_range(glyph_idx.min(self.chars.len() - 1));
            while range.end > range.start
                && layer
                    .chars()
                    .get(range.end - 1)
                    .is_some_and(tilo_pdf::TextChar::is_line_break)
            {
                range.end -= 1;
            }
            while range.start < range.end
                && layer
                    .chars()
                    .get(range.start)
                    .is_some_and(tilo_pdf::TextChar::is_line_break)
            {
                range.start += 1;
            }
            return (range.start, range.end);
        }
        let g = glyph_idx.min(self.chars.len() - 1);
        let Some(a) = self.chars.get(g).map(|c| c.rect) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layer() -> PageText {
        let mut glyphs = Vec::new();
        let mut x = 10.0;
        for ch in "PDF text".chars() {
            glyphs.push((ch, PtRect::new(x, 10.0, 8.0, 12.0)));
            x += 8.0;
        }
        glyphs.push((' ', PtRect::new(x, 10.0, 8.0, 12.0)));
        x = 10.0;
        for ch in "layer".chars() {
            glyphs.push((ch, PtRect::new(x, 30.0, 8.0, 12.0)));
            x += 8.0;
        }
        PageText::from_glyphs(glyphs)
    }

    #[test]
    fn glyph_layer_builds_aligned_text_and_hit_tests_words_rows_and_bands() {
        let pt = sample_layer();
        assert_eq!(pt.text, "PDF text layer");
        assert_eq!(pt.chars.len(), pt.text.chars().count());
        for c in &pt.chars {
            assert_eq!(pt.text[c.byte_start..].chars().next(), Some(c.ch));
        }

        let i = pt.char_at(14.0, 16.0).expect("a glyph under the point");
        assert_eq!(pt.chars[i].ch, 'P');
        let j = pt.char_at(12.0, 36.0).expect("row-2 glyph");
        assert!((pt.chars[j].rect.y - 30.0).abs() < 0.01);

        let bands = pt.bands_for_range(0, pt.chars.len());
        assert_eq!(bands.len(), 2);
        assert!(bands[0].y < bands[1].y);

        for (glyph, range, word) in [
            (5, Some((4, 8)), "text"),
            (1, Some((0, 3)), "PDF"),
            (10, None, "layer"),
        ] {
            let (s, e) = pt.word_range(glyph);
            if let Some(range) = range {
                assert_eq!((s, e), range, "word range at glyph {glyph}");
            }
            assert_eq!(pt.text_for_range(s, e), word, "word at glyph {glyph}");
        }

        let (s, e) = pt.row_range(1); // inside "PDF" on row 1
        assert_eq!(s, 0, "row starts at the first glyph");
        assert_eq!(pt.text_for_range(s, e).trim_end(), "PDF text");
        let (s2, _) = pt.row_range(10);
        assert!(s2 >= 9, "row 2 does not bleed into row 1: {s2}");
    }

    #[test]
    fn layer_search_honours_options_and_maps_hits_to_glyph_bands() {
        let pt = sample_layer();
        let cs = SearchOptions {
            case_sensitive: true,
            ..Default::default()
        };
        let ww = SearchOptions {
            whole_word: true,
            ..Default::default()
        };
        for (case, query, options, count) in [
            (
                "default is case-insensitive (exact case)",
                "PDF",
                SearchOptions::default(),
                1,
            ),
            (
                "default is case-insensitive (lowercase)",
                "pdf",
                SearchOptions::default(),
                1,
            ),
            ("case-sensitive misses lowercase", "pdf", cs, 0),
            ("case-sensitive finds exact case", "PDF", cs, 1),
            // "layer" contains "aye"; whole-word must reject it, substring accept.
            (
                "substring accepts a word fragment",
                "aye",
                SearchOptions::default(),
                1,
            ),
            ("whole-word rejects a word fragment", "aye", ww, 0),
            ("the full word still matches", "layer", ww, 1),
            (
                "blank query finds nothing",
                "   ",
                SearchOptions::default(),
                0,
            ),
        ] {
            assert_eq!(pt.search(1, query, options).len(), count, "case {case}");
        }

        let hits = pt.search(1, "text", SearchOptions::default());
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        assert_eq!(
            pt.text_for_range(m.char_start, m.char_start + m.char_len),
            "text"
        );
        let bands = pt.bands_for_range(m.char_start, m.char_start + m.char_len);
        assert_eq!(bands.len(), 1, "one row -> one band");
        assert!(
            (bands[0].w - 32.0).abs() < 0.01,
            "band spans four 8pt glyphs"
        );
        assert!((bands[0].y - 10.0).abs() < 0.01, "band sits on row 1");
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "one monospace glyph advance is exactly representable"
    )]
    fn unicode_and_bounded_layer_search_map_to_source_glyphs() {
        let text = "İ test \u{212a} TEST";
        let page = PageText::from_glyphs(
            text.chars()
                .enumerate()
                .map(|(index, ch)| (ch, PtRect::new(index as f64 * 8.0, 0.0, 8.0, 12.0))),
        );
        let hits = page.search(2, "test", SearchOptions::default());
        assert_eq!(
            hits,
            vec![
                TextMatch {
                    page: 2,
                    char_start: 2,
                    char_len: 4
                },
                TextMatch {
                    page: 2,
                    char_start: 9,
                    char_len: 4
                }
            ]
        );
        for (query, expected, start) in [("i\u{307}", "İ", 0), ("k", "\u{212a}", 7)] {
            let hits = page.search(2, query, SearchOptions::default());
            assert_eq!(hits.len(), 1, "query {query:?}");
            assert_eq!(hits[0].char_start, start, "query {query:?}");
            assert_eq!(hits[0].char_len, 1, "query {query:?}");
            assert_eq!(page.text_for_range(start, start + 1), expected);
            let bands = page.bands_for_range(start, start + 1);
            assert_eq!(bands.len(), 1, "query {query:?}");
            assert_eq!(bands[0].w, 8.0, "highlight covers exactly the source glyph");
        }

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

        let text = "a".repeat(crate::model::MAX_SEARCH_RESULTS * 4);
        let page =
            PageText::from_glyphs(text.chars().map(|ch| (ch, PtRect::new(0.0, 0.0, 1.0, 1.0))));
        let hits = page.search(1, "a", SearchOptions::default());
        assert_eq!(hits.len(), crate::model::MAX_SEARCH_RESULTS);
        assert_eq!(hits.last().unwrap().char_start, hits.len() - 1);
    }
}
