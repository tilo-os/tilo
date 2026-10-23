//! The page text layer: glyph geometry in reading order, selection, copy and
//! search.
//!
//! An engine reports what it extracted from the content stream as
//! [`RawChar`]s, in its own order. [`TextLayer::from_raw`] turns that into a
//! layer a reader can use, independently of which engine produced it:
//!
//! - **Copy-correct Unicode.** Presentation-form ligatures (`ﬁ`, `ﬄ`, …) and
//!   Arabic presentation forms are expanded to the characters they stand
//!   for, each getting its share of the glyph box, so selecting half of a
//!   ligature and copying yields the right letters. Copied text is NFC.
//! - **Lines and reading order.** Glyphs are grouped into lines by geometry;
//!   lines into blocks by recursive XY-cut, so a multi-column page reads
//!   column by column and a drag inside one column never picks up its
//!   neighbour. A line's direction comes from its strong characters, so
//!   right-to-left scripts copy in logical order even when an engine hands
//!   them over in visual order.
//! - **Selection.** By caret positions between two points, by a character
//!   range, by word/line, or by a box. Every selection yields per-line
//!   highlight rectangles and its copy text; soft hyphens at line ends are
//!   removed when the selection spans the break.
//! - **Search.** Case- (and optionally diacritic-) insensitive, matching
//!   across line breaks and hyphenation, resolving to character ranges and
//!   hit rectangles.
//!
//! Character positions (`usize`) index [`TextLayer::chars`], i.e. they are
//! reading-order positions. [`TextChar::source`] keeps the engine's own char
//! index for each position.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::geom::{PageSize, Point, Rect, Rotation};

/// Flag bits on a [`RawChar`].
pub mod raw_flags {
    /// The engine synthesised this character (a space between words or a
    /// line break); it has no glyph in the content stream.
    pub const GENERATED: u8 = 1;
    /// The engine identified the character as a hyphen that breaks a word
    /// across lines.
    pub const HYPHEN: u8 = 2;
    /// The font's Unicode mapping for this glyph was unusable.
    pub const MAP_ERROR: u8 = 4;
}

/// One character exactly as an engine extracted it, in engine order.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawChar {
    /// The Unicode scalar the engine mapped the glyph to (0 when unmapped).
    pub unicode: u32,
    /// The engine's index of this character on the page.
    pub index: u32,
    /// Tight glyph bounds in display space; empty for generated characters.
    pub bbox: Rect,
    /// Loose bounds (advance width × font ascent/descent) in display space —
    /// the box selection highlights and hit-tests against.
    pub loose: Rect,
    pub font_size: f32,
    /// [`raw_flags`] bits.
    pub flags: u8,
}

/// Flag bits on a [`TextChar`].
pub mod char_flags {
    /// Not a glyph on the page: an inter-word space or a line break that the
    /// engine or the layout analysis inserted.
    pub const GENERATED: u8 = 1;
    /// A line (or block) separator; its text is `'\n'` and it has no box.
    pub const LINE_BREAK: u8 = 2;
    /// A hyphen that splits a word across the following line break. Copy and
    /// search drop it together with that break.
    pub const SOFT_HYPHEN: u8 = 4;
    /// One of several characters a single glyph expanded to (a ligature).
    pub const EXPANDED: u8 = 8;
}

/// One character of the layer, in reading order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextChar {
    pub ch: char,
    /// Box in display space; empty for line breaks.
    pub rect: Rect,
    /// The engine char index this character came from.
    pub source: u32,
    /// Index into [`TextLayer::lines`].
    pub line: u32,
    /// [`char_flags`] bits.
    pub flags: u8,
}

impl TextChar {
    #[must_use]
    pub fn is_line_break(&self) -> bool {
        self.flags & char_flags::LINE_BREAK != 0
    }

    #[must_use]
    pub fn is_generated(&self) -> bool {
        self.flags & char_flags::GENERATED != 0
    }
}

/// A visual line: a run of characters sharing a baseline region.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// Range of positions in [`TextLayer::chars`] (excluding the trailing
    /// line break).
    pub chars: Range<usize>,
    pub rect: Rect,
    /// Base direction from the line's strong characters.
    pub rtl: bool,
    /// Index into [`TextLayer::blocks`].
    pub block: u32,
}

/// A block of lines that reads as a unit (a column segment or paragraph
/// group) as found by the XY-cut.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    /// Range of indices into [`TextLayer::lines`].
    pub lines: Range<usize>,
    pub rect: Rect,
}

/// A set of reading-order character ranges: sorted, disjoint, non-empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<Range<usize>>,
}

impl Selection {
    /// A selection of one range (empty ranges yield an empty selection).
    #[must_use]
    pub fn from_range(range: Range<usize>) -> Self {
        Self::from_ranges(vec![range])
    }

    /// A selection from arbitrary ranges; they are sorted and merged.
    #[must_use]
    pub fn from_ranges(mut ranges: Vec<Range<usize>>) -> Self {
        ranges.retain(|r| r.start < r.end);
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for r in ranges {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        Self { ranges: merged }
    }

    #[must_use]
    pub fn ranges(&self) -> &[Range<usize>] {
        &self.ranges
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The covering range from the first to the last selected position.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        Some(self.ranges.first()?.start..self.ranges.last()?.end)
    }
}

/// Options for [`TextLayer::search`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Match `cafe` against `café`.
    pub ignore_diacritics: bool,
}

/// One search hit on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct TextMatch {
    /// Reading-order positions covered by the hit.
    pub range: Range<usize>,
    /// Highlight rectangles, one per line the hit spans.
    pub rects: Vec<Rect>,
}

/// The analysed text of one page. See the module documentation.
#[derive(Clone, Debug, Default)]
pub struct TextLayer {
    chars: Vec<TextChar>,
    lines: Vec<TextLine>,
    blocks: Vec<TextBlock>,
    text: String,
    byte_offsets: Vec<usize>,
    /// Character and line boxes in layout space: the page before its own
    /// rotation, where lines run horizontally. Analysis and hit-testing
    /// happen here; the public boxes are these mapped to display space.
    layout: Vec<Rect>,
    line_layout: Vec<Rect>,
    rotation: Rotation,
    unrotated: PageSize,
}

/// A character between extraction and layout.
#[derive(Clone, Copy, Debug)]
struct Unit {
    ch: char,
    rect: Rect,
    source: u32,
    flags: u8,
}

/// Upper bound on the characters one page may contribute. A pathological
/// page is refused as a whole rather than truncated into a layer whose
/// selection and search would silently lie.
pub const MAX_PAGE_CHARS: usize = 200_000;

impl TextLayer {
    /// Analyse an engine's characters (engine order) into a layer. The
    /// characters' boxes are in display space of an unrotated page.
    ///
    /// Returns `None` when the page exceeds [`MAX_PAGE_CHARS`].
    #[must_use]
    pub fn from_raw(raw: &[RawChar]) -> Option<Self> {
        Self::from_raw_rotated(raw, Rotation::None, PageSize::default())
    }

    /// As [`Self::from_raw`] for a page displayed with its own `rotation`:
    /// the characters' boxes are in the page's space before that rotation
    /// (of size `unrotated`), which is where its lines run horizontally.
    /// Every box the layer returns is in display space.
    #[must_use]
    pub fn from_raw_rotated(
        raw: &[RawChar],
        rotation: Rotation,
        unrotated: PageSize,
    ) -> Option<Self> {
        let mut layer = Self::analyse(raw)?;
        layer.layout = layer.chars.iter().map(|c| c.rect).collect();
        layer.line_layout = layer.lines.iter().map(|l| l.rect).collect();
        layer.rotation = rotation;
        layer.unrotated = unrotated;
        if rotation != Rotation::None {
            for c in &mut layer.chars {
                if !c.rect.is_empty() {
                    c.rect = rotation.apply_rect(unrotated, c.rect);
                }
            }
            for l in &mut layer.lines {
                l.rect = rotation.apply_rect(unrotated, l.rect);
            }
            for b in &mut layer.blocks {
                b.rect = rotation.apply_rect(unrotated, b.rect);
            }
        }
        Some(layer)
    }

    fn to_layout(&self, p: Point) -> Point {
        self.rotation.invert_point(self.unrotated, p)
    }

    fn to_display(&self, r: Rect) -> Rect {
        self.rotation.apply_rect(self.unrotated, r)
    }

    fn layout_rect(&self, pos: usize) -> Rect {
        self.layout.get(pos).copied().unwrap_or_default()
    }

    fn analyse(raw: &[RawChar]) -> Option<Self> {
        if raw.len() > MAX_PAGE_CHARS {
            return None;
        }
        let units = expand_units(raw);
        let mut lines = build_lines(&units);
        for line in &mut lines {
            to_logical_order(line, &units);
        }
        let line_rects: Vec<Rect> = lines.iter().map(|l| l.rect).collect();
        let page_rtl = lines.iter().filter(|l| l.rtl).count().saturating_mul(2) > lines.len();
        let blocks = merge_paragraphs(xy_cut(&line_rects, page_rtl), &line_rects);
        let layer = assemble(&units, &lines, &blocks);
        (layer.chars.len() <= MAX_PAGE_CHARS.saturating_mul(2)).then_some(layer)
    }

    /// Characters in reading order.
    #[must_use]
    pub fn chars(&self) -> &[TextChar] {
        &self.chars
    }

    #[must_use]
    pub fn lines(&self) -> &[TextLine] {
        &self.lines
    }

    #[must_use]
    pub fn blocks(&self) -> &[TextBlock] {
        &self.blocks
    }

    /// The whole page text in reading order, lines separated by `'\n'`.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Byte offset of position `pos` in [`Self::text`] (the text length for
    /// `pos == len`).
    #[must_use]
    pub fn byte_offset(&self, pos: usize) -> usize {
        self.byte_offsets
            .get(pos)
            .copied()
            .unwrap_or(self.text.len())
    }

    /// The character under `p`, or the nearest character of the line under
    /// `p` (within a small vertical tolerance).
    #[must_use]
    pub fn hit_test(&self, p: Point) -> Option<usize> {
        let p = self.to_layout(p);
        if let Some(pos) = self
            .layout
            .iter()
            .position(|r| !r.is_empty() && r.contains(p))
        {
            return Some(pos);
        }
        let index = self.line_layout.iter().position(|r| {
            let tol = r.height() * 0.25;
            p.y >= r.y0 - tol
                && p.y <= r.y1 + tol
                && p.x >= r.x0 - r.height()
                && p.x <= r.x1 + r.height()
        })?;
        self.nearest_in_line(self.lines.get(index)?, p)
    }

    /// The nearest character of `line` to `p` (layout space).
    fn nearest_in_line(&self, line: &TextLine, p: Point) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for pos in line.chars.clone() {
            let r = self.layout_rect(pos);
            if r.is_empty() {
                continue;
            }
            let d = r.distance_to(p);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((pos, d));
            }
        }
        best.map(|(pos, _)| pos)
    }

    /// The line a point (layout space) belongs to for caret placement: the
    /// line whose vertical band contains it (nearest horizontally among
    /// those), else the nearest line.
    fn line_for_caret(&self, p: Point) -> Option<usize> {
        let mut best: Option<(usize, f32, f32)> = None;
        for (i, r) in self.line_layout.iter().enumerate() {
            let inside = p.y >= r.y0 && p.y <= r.y1;
            let d = r.distance_to(p);
            // Rank: inside the band first, then distance.
            let rank = if inside { 0.0 } else { 1.0 };
            if best.is_none_or(|(_, br, bd)| rank < br || (rank <= br && d < bd)) {
                best = Some((i, rank, d));
            }
        }
        best.map(|(i, _, _)| i)
    }

    /// The caret position (0..=len) nearest `p`: before or after the nearest
    /// character depending on which half of it `p` falls in, respecting the
    /// line's direction.
    #[must_use]
    pub fn caret_at(&self, p: Point) -> usize {
        let p = self.to_layout(p);
        let Some(line) = self.line_for_caret(p).and_then(|i| self.lines.get(i)) else {
            return 0;
        };
        let Some(pos) = self.nearest_in_line(line, p) else {
            return line.chars.start;
        };
        let mid = self.layout_rect(pos).center().x;
        let after = if line.rtl { p.x < mid } else { p.x > mid };
        if after { pos.saturating_add(1) } else { pos }
    }

    /// Flow selection between two points, as a drag from `anchor` to `focus`
    /// selects.
    #[must_use]
    pub fn select_between(&self, anchor: Point, focus: Point) -> Selection {
        let a = self.caret_at(anchor);
        let b = self.caret_at(focus);
        Selection::from_range(a.min(b)..a.max(b))
    }

    /// Selection of a reading-order character range (clamped to the page).
    #[must_use]
    pub fn select_range(&self, range: Range<usize>) -> Selection {
        let end = range.end.min(self.chars.len());
        Selection::from_range(range.start.min(end)..end)
    }

    /// The word containing position `pos` (letters, digits and marks within
    /// one line), or just that character when it is not part of a word.
    #[must_use]
    pub fn word_range(&self, pos: usize) -> Range<usize> {
        let Some(c) = self.chars.get(pos) else {
            return pos..pos;
        };
        let is_word = |t: &TextChar| {
            t.line == c.line
                && !t.is_generated()
                && (t.ch.is_alphanumeric() || is_combining_mark(t.ch))
        };
        if !is_word(c) {
            return pos..pos.saturating_add(1);
        }
        let mut start = pos;
        while start > 0 && self.chars.get(start.saturating_sub(1)).is_some_and(is_word) {
            start = start.saturating_sub(1);
        }
        let mut end = pos.saturating_add(1);
        while self.chars.get(end).is_some_and(is_word) {
            end = end.saturating_add(1);
        }
        start..end
    }

    #[must_use]
    pub fn line_range(&self, pos: usize) -> Range<usize> {
        self.chars
            .get(pos)
            .and_then(|c| self.lines.get(c.line as usize))
            .map_or(pos..pos, |l| l.chars.clone())
    }

    /// The text a copy of `selection` yields: NFC, soft hyphens at line
    /// breaks removed, ranges separated by line breaks.
    #[must_use]
    pub fn text_for(&self, selection: &Selection) -> String {
        let mut out = String::new();
        for (i, range) in selection.ranges().iter().enumerate() {
            if i > 0 && !out.ends_with('\n') {
                out.push('\n');
            }
            let mut pos = range.start;
            while pos < range.end {
                let Some(c) = self.chars.get(pos) else { break };
                if c.flags & char_flags::SOFT_HYPHEN != 0 {
                    let next = pos.saturating_add(1);
                    if next < range.end && self.chars.get(next).is_some_and(TextChar::is_line_break)
                    {
                        pos = next.saturating_add(1);
                        continue;
                    }
                }
                out.push(c.ch);
                pos = pos.saturating_add(1);
            }
        }
        out.nfc().collect()
    }

    /// Highlight rectangles for `selection`: one band per line segment.
    #[must_use]
    pub fn rects_for(&self, selection: &Selection) -> Vec<Rect> {
        let mut bands = Vec::new();
        for range in selection.ranges() {
            let mut current: Option<(u32, Rect)> = None;
            for pos in range.clone() {
                let Some(c) = self.chars.get(pos) else { break };
                let r = self.layout_rect(pos);
                if r.is_empty() {
                    continue;
                }
                match &mut current {
                    Some((line, band)) if *line == c.line => *band = band.union(&r),
                    _ => {
                        if let Some((_, band)) = current.take() {
                            bands.push(band);
                        }
                        current = Some((c.line, r));
                    }
                }
            }
            if let Some((_, band)) = current {
                bands.push(band);
            }
        }
        bands.into_iter().map(|b| self.to_display(b)).collect()
    }

    /// Find `query` on the page. Matches may span line breaks (and
    /// hyphenated breaks); whitespace runs match any whitespace run.
    #[must_use]
    pub fn search(&self, query: &str, opts: SearchOptions) -> Vec<TextMatch> {
        let needle: Vec<char> = fold_query(query, opts);
        if needle.is_empty() {
            return Vec::new();
        }
        let hay = self.search_haystack(opts);
        let mut out = Vec::new();
        let mut start = 0usize;
        while let Some(window_end) = start.checked_add(needle.len()) {
            if window_end > hay.len() {
                break;
            }
            let matched = hay
                .get(start..window_end)
                .is_some_and(|w| w.iter().map(|(c, _)| *c).eq(needle.iter().copied()));
            if matched && (!opts.whole_word || Self::is_word_bounded(&hay, start, window_end)) {
                let first = hay.get(start).map_or(0, |(_, p)| *p);
                let last = hay
                    .get(window_end.saturating_sub(1))
                    .map_or(first, |(_, p)| *p);
                let range = first..last.saturating_add(1);
                let rects = self.rects_for(&Selection::from_range(range.clone()));
                out.push(TextMatch { range, rects });
                start = window_end;
            } else {
                start = start.saturating_add(1);
            }
        }
        out
    }

    fn is_word_bounded(hay: &[(char, usize)], start: usize, end: usize) -> bool {
        let before = start
            .checked_sub(1)
            .and_then(|i| hay.get(i))
            .is_some_and(|(c, _)| c.is_alphanumeric());
        let after = hay.get(end).is_some_and(|(c, _)| c.is_alphanumeric());
        !before && !after
    }

    /// Folded characters paired with the position each came from.
    fn search_haystack(&self, opts: SearchOptions) -> Vec<(char, usize)> {
        let mut hay: Vec<(char, usize)> = Vec::with_capacity(self.chars.len());
        let mut pos = 0usize;
        while let Some(c) = self.chars.get(pos) {
            if c.flags & char_flags::SOFT_HYPHEN != 0
                && self
                    .chars
                    .get(pos.saturating_add(1))
                    .is_some_and(TextChar::is_line_break)
            {
                pos = pos.saturating_add(2);
                continue;
            }
            if c.ch.is_whitespace() || c.is_line_break() {
                if hay.last().is_some_and(|(h, _)| *h != ' ') {
                    hay.push((' ', pos));
                }
            } else {
                for f in fold_char(c.ch, opts) {
                    hay.push((f, pos));
                }
            }
            pos = pos.saturating_add(1);
        }
        hay
    }
}

fn fold_char(ch: char, opts: SearchOptions) -> Vec<char> {
    let mut out: Vec<char> = if opts.ignore_diacritics {
        std::iter::once(ch)
            .nfd()
            .filter(|c| !is_combining_mark(*c))
            .collect()
    } else {
        vec![ch]
    };
    if !opts.case_sensitive {
        out = out.into_iter().flat_map(char::to_lowercase).collect();
    }
    out
}

fn fold_query(query: &str, opts: SearchOptions) -> Vec<char> {
    let mut out = Vec::new();
    for ch in query.trim().nfc() {
        if ch.is_whitespace() {
            if out.last().is_some_and(|c| *c != ' ') {
                out.push(' ');
            }
        } else {
            out.extend(fold_char(ch, opts));
        }
    }
    out
}

/// The characters a glyph's Unicode value stands for when copied.
///
/// Alphabetic presentation forms (Latin/Armenian/Hebrew ligatures, U+FB00–
/// U+FB4F) and Arabic presentation forms (U+FB50–U+FDFF, U+FE70–U+FEFF) are
/// compatibility-decomposed to their base letters; every other character
/// is kept as is (full NFKC would also rewrite superscripts, full-width
/// forms and similar characters the author chose deliberately).
#[must_use]
pub fn copy_expansion(ch: char) -> Vec<char> {
    let cp = u32::from(ch);
    let presentation = (0xFB00..=0xFB4F).contains(&cp)
        || (0xFB50..=0xFDFF).contains(&cp)
        || (0xFE70..=0xFEFF).contains(&cp);
    if presentation {
        let expanded: Vec<char> = std::iter::once(ch).nfkc().collect();
        if !expanded.is_empty() {
            return expanded;
        }
    }
    vec![ch]
}

fn is_rtl_char(ch: char) -> bool {
    matches!(
        unicode_bidi::bidi_class(ch),
        unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL
    )
}

fn is_ltr_char(ch: char) -> bool {
    matches!(unicode_bidi::bidi_class(ch), unicode_bidi::BidiClass::L)
}

/// Raw engine characters to layout units: control characters dropped, the
/// engine's own line breaks dropped (layout analysis inserts its own), and
/// ligatures expanded with their box split between the parts.
#[allow(
    clippy::cast_precision_loss,
    reason = "a ligature expands to a handful of characters"
)]
fn expand_units(raw: &[RawChar]) -> Vec<Unit> {
    let mut units = Vec::with_capacity(raw.len());
    for r in raw {
        let Some(ch) = char::from_u32(r.unicode) else {
            continue;
        };
        if ch == '\0' || ch == '\r' || ch == '\n' || ch == '\u{FFFE}' || ch == '\u{FFFF}' {
            continue;
        }
        let ch = if ch == '\t' {
            ' '
        } else if ch.is_control() {
            continue;
        } else {
            ch
        };
        let mut rect = if r.loose.is_empty() || !r.loose.is_finite() {
            r.bbox
        } else {
            r.loose
        };
        if !rect.is_finite() {
            rect = Rect::default();
        }
        let mut flags = 0u8;
        if r.flags & raw_flags::GENERATED != 0 {
            flags |= char_flags::GENERATED;
            // A generated character's geometry is unreliable; the line pass
            // fills it from its neighbours.
            rect = Rect::default();
        }
        if r.flags & raw_flags::HYPHEN != 0 || ch == '\u{00AD}' {
            flags |= char_flags::SOFT_HYPHEN;
        }
        let parts = copy_expansion(ch);
        if parts.len() <= 1 {
            units.push(Unit {
                ch,
                rect,
                source: r.index,
                flags,
            });
            continue;
        }
        let n = parts.len() as f32;
        let rtl = parts.iter().any(|c| is_rtl_char(*c));
        let w = rect.width() / n;
        for (i, part) in parts.into_iter().enumerate() {
            let k = if rtl { n - 1.0 - i as f32 } else { i as f32 };
            let sub = if rect.is_empty() {
                rect
            } else {
                Rect {
                    x0: rect.x0 + w * k,
                    x1: rect.x0 + w * (k + 1.0),
                    ..rect
                }
            };
            units.push(Unit {
                ch: part,
                rect: sub,
                source: r.index,
                flags: flags | char_flags::EXPANDED,
            });
        }
    }
    split_shared_boxes(&mut units);
    drop_mark_duplicates(&mut units);
    drop_redundant_spaces(&mut units);
    units
}

fn same_rect(a: &Rect, b: &Rect) -> bool {
    (a.x0 - b.x0).abs() < 0.01
        && (a.x1 - b.x1).abs() < 0.01
        && (a.y0 - b.y0).abs() < 0.01
        && (a.y1 - b.y1).abs() < 0.01
}

/// A glyph that maps to several characters (a ligature with a `ToUnicode`
/// entry like "ffi") is reported as consecutive characters sharing one
/// box. Give each its share of the box so they can be selected apart.
#[allow(clippy::cast_precision_loss, reason = "runs are a few characters long")]
fn split_shared_boxes(units: &mut [Unit]) {
    let mut start = 0usize;
    while start < units.len() {
        let Some(first) = units.get(start).copied() else {
            break;
        };
        let mut end = start.saturating_add(1);
        while units.get(end).is_some_and(|u| {
            !first.rect.is_empty()
                && u.flags & char_flags::GENERATED == 0
                && same_rect(&u.rect, &first.rect)
        }) {
            end = end.saturating_add(1);
        }
        let n = end.saturating_sub(start);
        if n > 1 && first.flags & char_flags::GENERATED == 0 {
            let rtl = units
                .get(start..end)
                .is_some_and(|run| run.iter().any(|u| is_rtl_char(u.ch)));
            let w = first.rect.width() / n as f32;
            if let Some(run) = units.get_mut(start..end) {
                for (i, u) in run.iter_mut().enumerate() {
                    let k = if rtl {
                        n.saturating_sub(1).saturating_sub(i) as f32
                    } else {
                        i as f32
                    };
                    u.rect.x0 = first.rect.x0 + w * k;
                    u.rect.x1 = first.rect.x0 + w * (k + 1.0);
                    u.flags |= char_flags::EXPANDED;
                }
            }
        }
        start = end;
    }
}

/// Some producers map every glyph of a multi-glyph cluster to the cluster's
/// text, so a separately drawn dot or mark (common in Arabic Naskh fonts)
/// copies as a second letter. Drop a character that repeats a nearby
/// character of the same value and lies inside its box while being clearly
/// narrower: it is part of that letter's drawing, not text of its own.
fn drop_mark_duplicates(units: &mut Vec<Unit>) {
    const WINDOW: usize = 12;
    let mut keep = vec![true; units.len()];
    for (i, u) in units.iter().enumerate() {
        if u.rect.is_empty() || u.flags & char_flags::GENERATED != 0 || u.ch.is_whitespace() {
            continue;
        }
        let lo = i.saturating_sub(WINDOW);
        let hi = i.saturating_add(WINDOW).min(units.len());
        let covered = (lo..hi).any(|j| {
            j != i
                && units.get(j).is_some_and(|v| {
                    v.ch == u.ch
                        && !v.rect.is_empty()
                        && u.rect.x0 >= v.rect.x0 - 0.5
                        && u.rect.x1 <= v.rect.x1 + 0.5
                        && u.rect.width() < v.rect.width() * 0.8
                        && u.rect.vertical_overlap(&v.rect)
                            > 0.5 * u.rect.height().min(v.rect.height())
                })
        });
        if covered && let Some(k) = keep.get_mut(i) {
            *k = false;
        }
    }
    let mut flags = keep.into_iter();
    units.retain(|_| flags.next().unwrap_or(true));
}

/// A space without geometry next to another space adds nothing but a
/// doubled space in copied text.
fn drop_redundant_spaces(units: &mut Vec<Unit>) {
    let is_space = |u: &Unit| u.ch.is_whitespace();
    let mut keep = vec![true; units.len()];
    for i in 0..units.len() {
        let Some(u) = units.get(i) else { continue };
        if !is_space(u) || !u.rect.is_empty() {
            continue;
        }
        let prev_space = i
            .checked_sub(1)
            .and_then(|j| units.get(j))
            .is_some_and(is_space);
        let next_space = units
            .get(i.saturating_add(1))
            .is_some_and(|v| is_space(v) && !v.rect.is_empty());
        if (prev_space || next_space)
            && let Some(k) = keep.get_mut(i)
        {
            *k = false;
        }
    }
    let mut flags = keep.into_iter();
    units.retain(|_| flags.next().unwrap_or(true));
}

/// A line under construction: unit indices in engine order.
#[derive(Clone, Debug)]
struct LineBuild {
    units: Vec<usize>,
    rect: Rect,
    rtl: bool,
}

fn build_lines(units: &[Unit]) -> Vec<LineBuild> {
    let mut lines: Vec<LineBuild> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut last: Option<Rect> = None;
    for (i, u) in units.iter().enumerate() {
        if u.rect.is_empty() {
            current.push(i);
            continue;
        }
        if let Some(prev) = last {
            let min_h = prev.height().min(u.rect.height()).max(0.01);
            let same_row = u.rect.vertical_overlap(&prev) >= 0.4 * min_h;
            let gap = if u.rect.x0 >= prev.x1 {
                u.rect.x0 - prev.x1
            } else if u.rect.x1 <= prev.x0 {
                prev.x0 - u.rect.x1
            } else {
                0.0
            };
            let line_h = prev.height().max(u.rect.height());
            if !same_row || gap > 1.8 * line_h {
                finish_line(&mut lines, std::mem::take(&mut current), units);
            }
        }
        current.push(i);
        last = Some(u.rect);
    }
    finish_line(&mut lines, current, units);
    lines
}

fn finish_line(lines: &mut Vec<LineBuild>, mut members: Vec<usize>, units: &[Unit]) {
    let has_rect = |i: &usize| units.get(*i).is_some_and(|u| !u.rect.is_empty());
    // Leading/trailing generated characters carry no information.
    while members.last().is_some_and(|i| !has_rect(i)) {
        members.pop();
    }
    let first_real = members.iter().position(has_rect);
    let Some(first_real) = first_real else {
        return;
    };
    members.drain(..first_real);
    let mut rect: Option<Rect> = None;
    let (mut rtl, mut ltr) = (0usize, 0usize);
    for i in &members {
        let Some(u) = units.get(*i) else { continue };
        if !u.rect.is_empty() {
            rect = Some(rect.map_or(u.rect, |r| r.union(&u.rect)));
        }
        if is_rtl_char(u.ch) {
            rtl = rtl.saturating_add(1);
        } else if is_ltr_char(u.ch) {
            ltr = ltr.saturating_add(1);
        }
    }
    let Some(rect) = rect else { return };
    lines.push(LineBuild {
        units: members,
        rect,
        rtl: rtl > ltr,
    });
}

/// Engines normally report logical order. When a line's characters instead
/// advance against its direction (visual order), reverse it back.
///
/// For a right-to-left line in visual order the whole line is reversed and
/// then every embedded left-to-right run (Latin words, digits) is reversed
/// again; a left-to-right line only has its right-to-left runs reversed.
/// This is the inverse of the Unicode bidi reordering for the two-level case
/// that covers real documents.
fn to_logical_order(line: &mut LineBuild, units: &[Unit]) {
    let positions: Vec<(usize, f32, char)> = line
        .units
        .iter()
        .filter_map(|&i| {
            let u = units.get(i)?;
            (!u.rect.is_empty()).then_some((i, u.rect.center().x, u.ch))
        })
        .collect();
    let rtl_pairs = |filter: &dyn Fn(char) -> bool| -> (usize, usize) {
        let mut inc = 0usize;
        let mut dec = 0usize;
        for pair in positions.windows(2) {
            let [(_, xa, ca), (_, xb, cb)] = pair else {
                continue;
            };
            if filter(*ca) && filter(*cb) {
                if xb > xa {
                    inc = inc.saturating_add(1);
                } else if xb < xa {
                    dec = dec.saturating_add(1);
                }
            }
        }
        (inc, dec)
    };
    let (inc, dec) = rtl_pairs(&is_rtl_char);
    if inc <= dec {
        return; // already logical (or no right-to-left pairs to judge by)
    }
    let is_run_char = |i: &usize, rtl_run: bool| {
        units.get(*i).is_some_and(|u| {
            if rtl_run {
                is_rtl_char(u.ch) || is_combining_mark(u.ch)
            } else {
                is_ltr_char(u.ch) || u.ch.is_ascii_digit()
            }
        })
    };
    if line.rtl {
        line.units.reverse();
        reverse_runs(&mut line.units, |i| is_run_char(i, false));
    } else {
        reverse_runs(&mut line.units, |i| is_run_char(i, true));
    }
}

/// Reverse every maximal run of elements satisfying `in_run` (runs may
/// contain single neutral characters between two run characters, such as
/// the space inside a Latin phrase).
fn reverse_runs(items: &mut [usize], in_run: impl Fn(&usize) -> bool) {
    let n = items.len();
    let mut i = 0usize;
    while i < n {
        if !items.get(i).is_some_and(&in_run) {
            i = i.saturating_add(1);
            continue;
        }
        let start = i;
        let mut end = i.saturating_add(1);
        loop {
            if items.get(end).is_some_and(&in_run) {
                end = end.saturating_add(1);
            } else if items.get(end.saturating_add(1)).is_some_and(&in_run)
                && end.saturating_add(1) < n
            {
                // A single neutral between two run characters stays inside.
                end = end.saturating_add(2);
            } else {
                break;
            }
        }
        if let Some(run) = items.get_mut(start..end) {
            run.reverse();
        }
        i = end;
    }
}

/// Recursive XY-cut over line rectangles: split at column gutters first,
/// then at the widest horizontal gap; leaves are blocks, emitted in reading
/// order (columns right-to-left on a right-to-left page).
fn xy_cut(rects: &[Rect], rtl_page: bool) -> Vec<Vec<usize>> {
    const MIN_GUTTER: f32 = 3.0;
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut stack: Vec<Vec<usize>> = vec![(0..rects.len()).collect()];
    while let Some(region) = stack.pop() {
        if region.len() <= 1 {
            if !region.is_empty() {
                out.push(region);
            }
            continue;
        }
        let rect_of = |i: &usize| rects.get(*i).copied().unwrap_or_default();
        // Column gutters: gaps in the union of x-intervals.
        let columns = split_by_gaps(&region, |i| (rect_of(i).x0, rect_of(i).x1), MIN_GUTTER);
        if columns.len() > 1 {
            let mut columns = columns;
            if !rtl_page {
                columns.reverse();
            }
            // Pushed in reverse so the first column pops first.
            stack.extend(columns);
            continue;
        }
        // Rows: split once at the widest gap in the y-intervals.
        let mut by_y = region.clone();
        by_y.sort_by(|a, b| rect_of(a).y0.total_cmp(&rect_of(b).y0));
        let mut widest: Option<(usize, f32)> = None;
        let mut reach = f32::NEG_INFINITY;
        for (k, i) in by_y.iter().enumerate() {
            let r = rect_of(i);
            if k > 0 {
                let gap = r.y0 - reach;
                if gap > 0.05 && widest.is_none_or(|(_, g)| gap > g + 0.01) {
                    widest = Some((k, gap));
                }
            }
            reach = reach.max(r.y1);
        }
        if let Some((k, _)) = widest {
            let below = by_y.split_off(k);
            stack.push(below);
            stack.push(by_y);
        } else {
            by_y.sort_by(|a, b| {
                let (ra, rb) = (rect_of(a), rect_of(b));
                ra.y0.total_cmp(&rb.y0).then(if rtl_page {
                    rb.x0.total_cmp(&ra.x0)
                } else {
                    ra.x0.total_cmp(&rb.x0)
                })
            });
            out.push(by_y);
        }
    }
    out
}

/// Join consecutive XY-cut leaves that continue one another — the next
/// leaf starts within a line's height below the previous one and overlaps it
/// horizontally — into one block, so blocks are paragraphs rather than lines.
fn merge_paragraphs(leaves: Vec<Vec<usize>>, rects: &[Rect]) -> Vec<Vec<usize>> {
    let bounds = |leaf: &[usize]| {
        leaf.iter()
            .filter_map(|i| rects.get(*i))
            .copied()
            .reduce(|a, b| a.union(&b))
            .unwrap_or_default()
    };
    let mut out: Vec<Vec<usize>> = Vec::with_capacity(leaves.len());
    for leaf in leaves {
        let next = bounds(&leaf);
        let joins = out.last().is_some_and(|prev| {
            let last_line = prev
                .last()
                .and_then(|i| rects.get(*i))
                .copied()
                .unwrap_or_default();
            let gap = next.y0 - last_line.y1;
            let overlap = (last_line.x1.min(next.x1) - last_line.x0.max(next.x0)).max(0.0);
            let narrower = last_line.width().min(next.width()).max(0.01);
            gap >= -0.5 && gap <= last_line.height() * 0.8 && overlap >= 0.5 * narrower
        });
        match out.last_mut() {
            Some(prev) if joins => prev.extend(leaf),
            _ => out.push(leaf),
        }
    }
    out
}

/// Partition `items` into groups separated by gaps of at least `min_gap` in
/// their projected intervals, ordered by position.
fn split_by_gaps(
    items: &[usize],
    interval: impl Fn(&usize) -> (f32, f32),
    min_gap: f32,
) -> Vec<Vec<usize>> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| interval(a).0.total_cmp(&interval(b).0));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut reach = f32::NEG_INFINITY;
    for i in sorted {
        let (lo, hi) = interval(&i);
        match groups.last_mut() {
            Some(group) if lo - reach < min_gap => group.push(i),
            _ => groups.push(vec![i]),
        }
        reach = reach.max(hi);
    }
    groups
}

fn assemble(units: &[Unit], lines: &[LineBuild], blocks: &[Vec<usize>]) -> TextLayer {
    let mut layer = TextLayer::default();
    for block in blocks {
        let first_line = layer.lines.len();
        let mut block_rect: Option<Rect> = None;
        let block_index = u32::try_from(layer.blocks.len()).unwrap_or(u32::MAX);
        for &li in block {
            let Some(line) = lines.get(li) else { continue };
            if !layer.chars.is_empty() {
                let prev_line = u32::try_from(layer.lines.len().saturating_sub(1)).unwrap_or(0);
                push_char(
                    &mut layer,
                    TextChar {
                        ch: '\n',
                        rect: Rect::default(),
                        source: u32::MAX,
                        line: prev_line,
                        flags: char_flags::GENERATED | char_flags::LINE_BREAK,
                    },
                );
            }
            let line_index = u32::try_from(layer.lines.len()).unwrap_or(u32::MAX);
            let start = layer.chars.len();
            let resolved = resolve_generated_rects(&line.units, units, line);
            for (ui, rect) in resolved {
                let Some(u) = units.get(ui) else { continue };
                push_char(
                    &mut layer,
                    TextChar {
                        ch: u.ch,
                        rect,
                        source: u.source,
                        line: line_index,
                        flags: u.flags,
                    },
                );
            }
            // Only a hyphen that ends the line splits a word.
            let end = layer.chars.len();
            if let Some(line_chars) = layer.chars.get_mut(start..end.saturating_sub(1)) {
                for c in line_chars {
                    c.flags &= !char_flags::SOFT_HYPHEN;
                }
            }
            block_rect = Some(block_rect.map_or(line.rect, |r| r.union(&line.rect)));
            layer.lines.push(TextLine {
                chars: start..layer.chars.len(),
                rect: line.rect,
                rtl: line.rtl,
                block: block_index,
            });
        }
        if let Some(rect) = block_rect {
            layer.blocks.push(TextBlock {
                lines: first_line..layer.lines.len(),
                rect,
            });
        }
    }
    layer
}

fn push_char(layer: &mut TextLayer, c: TextChar) {
    layer.byte_offsets.push(layer.text.len());
    layer.text.push(c.ch);
    layer.chars.push(c);
}

/// Give generated characters inside a line (inter-word spaces) the box
/// between their neighbours so they can be hit and highlighted.
fn resolve_generated_rects(
    members: &[usize],
    units: &[Unit],
    line: &LineBuild,
) -> Vec<(usize, Rect)> {
    let rect_at = |k: usize| members.get(k).and_then(|i| units.get(*i)).map(|u| u.rect);
    let mut out = Vec::with_capacity(members.len());
    for (k, &ui) in members.iter().enumerate() {
        let rect = rect_at(k).unwrap_or_default();
        if !rect.is_empty() {
            out.push((ui, rect));
            continue;
        }
        let prev = (0..k)
            .rev()
            .find_map(|j| rect_at(j).filter(|r| !r.is_empty()));
        let next =
            (k.saturating_add(1)..members.len()).find_map(|j| rect_at(j).filter(|r| !r.is_empty()));
        let filled = match (prev, next) {
            (Some(p), Some(n)) => {
                let (x0, x1) = if line.rtl { (n.x1, p.x0) } else { (p.x1, n.x0) };
                let (x0, x1) = (x0.min(x1), x0.max(x1));
                Rect {
                    x0,
                    x1: if x1 - x0 < 0.5 { x0 + 0.5 } else { x1 },
                    y0: line.rect.y0,
                    y1: line.rect.y1,
                }
            }
            _ => Rect::default(),
        };
        out.push((ui, filled));
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "small synthetic test geometry"
)]
mod tests {
    use super::*;

    fn glyph(ch: char, index: u32, x: f32, y: f32, w: f32) -> RawChar {
        let r = Rect::from_xywh(x, y, w, 10.0);
        RawChar {
            unicode: u32::from(ch),
            index,
            bbox: r,
            loose: r,
            font_size: 10.0,
            flags: 0,
        }
    }

    fn space(index: u32) -> RawChar {
        RawChar {
            unicode: u32::from(' '),
            index,
            bbox: Rect::default(),
            loose: Rect::default(),
            font_size: 10.0,
            flags: raw_flags::GENERATED,
        }
    }

    fn run(text: &str, first: u32, x: f32, y: f32) -> Vec<RawChar> {
        let mut out = Vec::new();
        let mut cx = x;
        for (i, ch) in text.chars().enumerate() {
            let index = first + i as u32;
            if ch == ' ' {
                out.push(space(index));
                cx += 4.0;
            } else {
                out.push(glyph(ch, index, cx, y, 6.0));
                cx += 6.0;
            }
        }
        out
    }

    #[test]
    fn copied_text_expands_ligatures_and_presentation_forms_and_drops_soft_hyphens() {
        // Phase `ligature_glyph_copies_as_its_letters_and_splits_its_box`.
        {
            let raw = vec![
                glyph('\u{FB01}', 0, 0.0, 0.0, 10.0),
                glyph('n', 1, 10.0, 0.0, 5.0),
                glyph('d', 2, 15.0, 0.0, 5.0),
            ];
            let layer = TextLayer::from_raw(&raw).unwrap();
            assert_eq!(layer.text(), "find");
            assert_eq!(layer.chars()[0].rect.x1, 5.0);
            assert_eq!(layer.chars()[1].rect.x0, 5.0);
            // Selecting only the first half of the ligature copies "f".
            let sel = layer.select_between(Point::new(0.5, 5.0), Point::new(4.0, 5.0));
            assert_eq!(layer.text_for(&sel), "f");
            assert_eq!(layer.search("fi", SearchOptions::default()).len(), 1);
        }
        // Phase `soft_hyphen_at_line_end_is_dropped_on_copy_and_search`.
        {
            let mut raw = run("exam", 0, 0.0, 0.0);
            let mut hyphen = glyph('-', 4, 24.0, 0.0, 4.0);
            hyphen.flags = raw_flags::HYPHEN;
            raw.push(hyphen);
            raw.extend(run("ple text", 5, 0.0, 14.0));
            let layer = TextLayer::from_raw(&raw).unwrap();
            let all = layer.select_range(0..layer.len());
            assert_eq!(layer.text_for(&all), "example text");
            assert_eq!(layer.search("example", SearchOptions::default()).len(), 1);
            let hit = &layer.search("example", SearchOptions::default())[0];
            assert_eq!(hit.rects.len(), 2, "hit spans two lines");
        }
        // Phase `arabic_presentation_forms_copy_as_base_letters`.
        {
            // U+FEDF U+FEE0 are initial/medial LAM presentation forms.
            assert_eq!(copy_expansion('\u{FEDF}'), vec!['\u{0644}']);
            assert_eq!(copy_expansion('\u{FB00}'), vec!['f', 'f']);
            assert_eq!(copy_expansion('²'), vec!['²'], "superscripts are kept");
        }
    }

    #[test]
    fn text_layers_restore_reading_order_across_columns_and_scripts() {
        // Phase `two_columns_read_column_by_column`.
        {
            let mut raw = Vec::new();
            // Engine order interleaves the columns row by row, as some producers do.
            raw.extend(run("left one", 0, 0.0, 0.0));
            raw.extend(run("right one", 100, 200.0, 0.0));
            raw.extend(run("left two", 200, 0.0, 14.0));
            raw.extend(run("right two", 300, 200.0, 14.0));
            let layer = TextLayer::from_raw(&raw).unwrap();
            assert_eq!(layer.text(), "left one\nleft two\nright one\nright two");
            assert_eq!(layer.blocks().len(), 2);
            // A drag within the left column selects only left-column text.
            let sel = layer.select_between(Point::new(0.5, 5.0), Point::new(47.0, 19.0));
            assert_eq!(layer.text_for(&sel), "left one\nleft two");
        }
        // Phase `title_above_columns_reads_first`.
        {
            let mut raw = Vec::new();
            raw.extend(run("Title spanning both", 0, 0.0, 0.0));
            raw.extend(run("a1", 50, 0.0, 30.0));
            raw.extend(run("a2", 60, 0.0, 44.0));
            raw.extend(run("b1", 70, 200.0, 30.0));
            raw.extend(run("b2", 80, 200.0, 44.0));
            let layer = TextLayer::from_raw(&raw).unwrap();
            assert_eq!(layer.text(), "Title spanning both\na1\na2\nb1\nb2");
        }
        // Phase `visual_order_rtl_line_is_restored_to_logical_order`.
        {
            // "سلام" drawn right-to-left but reported left-to-right (visual).
            let logical: Vec<char> = "سلام".chars().collect();
            let raw: Vec<RawChar> = logical
                .iter()
                .rev()
                .enumerate()
                .map(|(i, ch)| glyph(*ch, i as u32, i as f32 * 6.0, 0.0, 6.0))
                .collect();
            let layer = TextLayer::from_raw(&raw).unwrap();
            assert_eq!(layer.text(), "سلام");
            assert!(layer.lines()[0].rtl);
        }
        // Phase `logical_order_rtl_line_is_kept`.
        {
            let raw: Vec<RawChar> = "שלום"
                .chars()
                .enumerate()
                .map(|(i, ch)| glyph(ch, i as u32, 100.0 - i as f32 * 6.0, 0.0, 6.0))
                .collect();
            let layer = TextLayer::from_raw(&raw).unwrap();
            assert_eq!(layer.text(), "שלום");
            // In a right-to-left line the caret right of the first glyph is 0.
            assert_eq!(layer.caret_at(Point::new(105.5, 5.0)), 0);
        }
    }

    #[test]
    fn search_and_ranges_pick_words_lines_and_folded_matches() {
        // Phase `search_is_case_and_optionally_diacritic_insensitive`.
        {
            let raw = run("Café CAFE", 0, 0.0, 0.0);
            let layer = TextLayer::from_raw(&raw).unwrap();
            assert_eq!(layer.search("cafe", SearchOptions::default()).len(), 1);
            let opts = SearchOptions {
                ignore_diacritics: true,
                ..SearchOptions::default()
            };
            assert_eq!(layer.search("cafe", opts).len(), 2);
            let ww = SearchOptions {
                whole_word: true,
                ..SearchOptions::default()
            };
            assert_eq!(layer.search("caf", ww).len(), 0);
        }
        // Phase `word_and_line_ranges`.
        {
            let raw = run("hello world", 0, 0.0, 0.0);
            let layer = TextLayer::from_raw(&raw).unwrap();
            let w = layer.word_range(7);
            assert_eq!(layer.text_for(&layer.select_range(w)), "world");
            let l = layer.line_range(0);
            assert_eq!(layer.text_for(&layer.select_range(l)), "hello world");
        }
    }
}
