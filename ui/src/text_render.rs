use std::collections::HashMap;
use std::fs;
use cosmic_text::{FontSystem, SwashCache, Buffer, Family, Metrics, Shaping, Style as FontStyle, Wrap};
use cosmic_text::fontdb::Weight;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, FontData as Font, StyleRef};

/// Concept-aligned named weight tiers for shell chrome.
///
/// Tailwind's `font-medium` / `font-semibold` / `font-bold` map 1:1 to
/// [`cosmic_text::fontdb::Weight`]'s numeric constants. We expose them as an
/// enum so widget call sites read declaratively (e.g.
/// `draw_text_weighted(..., TextWeight::Bold)`) rather than passing raw u16s
/// around. See visual-gap #7 in `docs/nirios/visual-gaps.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextWeight {
    /// 400 — body / subtitle / explanatory text. Default.
    Regular,
    /// 500 — most chrome labels (buttons, secondary titles).
    Medium,
    /// 600 — active chrome titles (tab titles, workspace-name pill).
    Semibold,
    /// 700 — h1/h2, clock, numeric monospace values.
    Bold,
}

impl TextWeight {
    /// Numeric OpenType weight (400 / 500 / 600 / 700).
    pub const fn to_u16(self) -> u16 {
        match self {
            TextWeight::Regular => 400,
            TextWeight::Medium => 500,
            TextWeight::Semibold => 600,
            TextWeight::Bold => 700,
        }
    }

    /// Matching [`cosmic_text::fontdb::Weight`] constant to feed to
    /// [`cosmic_text::Attrs::weight`].
    pub const fn to_cosmic_weight(self) -> Weight {
        Weight(self.to_u16())
    }
}

impl Default for TextWeight {
    fn default() -> Self {
        TextWeight::Regular
    }
}

/// Hashable cache key for measured-text results. `max_width` is
/// encoded as a `u32` bit-pattern so `f32` (which isn't `Hash`)
/// participates in the key without surprise NaN collisions —
/// callers either pass a sane finite max width or no max at all,
/// so the bit-pattern is unique-per-input.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MeasureKey {
    text_hash: u64,
    size_bits: u32,
    max_width_bits: u32,
    weight: u16,
    generation: u64,
}

/// Maximum entries the text-measure cache holds before it starts
/// evicting on insertion. 4096 entries × ~64 bytes ≈ 256 KB —
/// enough for every static label in a several-hundred-widget tree
/// to survive across frames, small enough that we don't bloat
/// process RSS when the user types many distinct strings.
const MEASURE_CACHE_LIMIT: usize = 4096;

pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    font_cache: HashMap<cosmic_text::fontdb::ID, Font>,
    /// `(text, size, max_width, weight)` → measured `(width, height)`.
    /// The same `measure_weighted` call is the dominant cost in
    /// `widget::*::draw` (see the 2026-05-23 perf plan: the cosmic-
    /// text re-shape per frame is ~7.5 ms in debug across a 50-
    /// widget tree). Most widgets re-measure the same static
    /// strings every frame; caching the result drops that to a
    /// hash lookup.
    measure_cache: HashMap<MeasureKey, (f32, f32)>,
    /// Generation counter — bumped by [`Self::bump_generation`]
    /// when the user changes locale or hot-swaps a font. Cache
    /// entries from prior generations stay in the map but never
    /// match a lookup (since the generation is part of the key);
    /// they're aged out when the cache hits its limit.
    generation: u64,
    /// Hits / misses counter — for the `cache_stats` accessor.
    /// Useful in tests + benches; doesn't affect runtime behaviour.
    hits: u64,
    misses: u64,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_cache: HashMap::new(),
            measure_cache: HashMap::with_capacity(256),
            generation: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// Hit / miss snapshot of the measure cache. Useful for
    /// benchmarks asserting "the static-label widgets warmed the
    /// cache and subsequent frames are pure hits".
    pub fn cache_stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Invalidate every cached measurement. Call this when the
    /// font set or active locale changes; cosmic-text will shape
    /// glyphs differently after either swap, so stale numbers
    /// would let widgets compute wrong layouts.
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.measure_cache.clear();
    }

    /// Internal: hash-based key lookup → cached measurement.
    /// Returns `None` on miss. Mutates hit / miss counters.
    fn measure_lookup(&mut self, key: MeasureKey) -> Option<(f32, f32)> {
        if let Some(v) = self.measure_cache.get(&key).copied() {
            self.hits += 1;
            return Some(v);
        }
        self.misses += 1;
        None
    }

    /// Internal: write a cache entry, evicting at random when the
    /// table is full. Random eviction is cheap and good enough —
    /// most missed entries are one-off animated values (Spinner
    /// dots, scrubbing slider value) that we don't care about
    /// keeping anyway.
    fn measure_insert(&mut self, key: MeasureKey, value: (f32, f32)) {
        if self.measure_cache.len() >= MEASURE_CACHE_LIMIT {
            if let Some(victim) = self.measure_cache.keys().next().copied() {
                self.measure_cache.remove(&victim);
            }
        }
        self.measure_cache.insert(key, value);
    }

    fn make_measure_key(&self, text: &str, size: f32, max_width: Option<f32>, weight: TextWeight) -> MeasureKey {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        MeasureKey {
            text_hash: hasher.finish(),
            size_bits: size.to_bits(),
            max_width_bits: max_width.map(|w| w.to_bits()).unwrap_or(u32::MAX),
            weight: weight.to_u16(),
            generation: self.generation,
        }
    }

    pub fn measure(&mut self, text: &str, size: f32, max_width: Option<f32>) -> (f32, f32) {
        self.measure_weighted(text, size, max_width, TextWeight::Regular)
    }

    /// Same as [`TextRenderer::measure`] but respects a typographic weight.
    /// Heavier weights generally advance wider, so measurements must use
    /// the same weight the draw path will render with (visual-gap #7).
    pub fn measure_weighted(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
    ) -> (f32, f32) {
        if text.is_empty() { return (0.0, 0.0); }

        let key = self.make_measure_key(text, size, max_width, weight);
        if let Some(cached) = self.measure_lookup(key) {
            return cached;
        }

        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        // cosmic-text 0.19: setters are lazy and no longer take FontSystem;
        // set_text gained a trailing `Option<Align>` arg.
        buffer.set_text(text, &attrs, Shaping::Advanced, None);

        buffer.set_size(max_width, None);
        // Explicitly set wrap mode. Word wrap is standard for UI labels.
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width = 0.0f32;
        let mut lines = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        let height = lines as f32 * line_height;
        let result = (width, height);
        self.measure_insert(key, result);
        result
    }

    /// Truncate `text` to fit within `max_width` logical pixels on a
    /// single line, appending an ellipsis (`…`) when anything was dropped.
    ///
    /// Returns `text` unchanged when it already fits. When even the
    /// ellipsis alone doesn't fit, returns the ellipsis (callers that
    /// reserve at least a few px get a sensible glyph rather than empty).
    /// Measurement uses [`TextWeight::Regular`]; pair with
    /// [`Self::elide_to_width_weighted`] when the draw weight differs so the
    /// fit calculation matches the rendered advance.
    pub fn elide_to_width(&mut self, text: &str, size: f32, max_width: f32) -> String {
        self.elide_to_width_weighted(text, size, max_width, TextWeight::Regular)
    }

    /// Weighted variant of [`Self::elide_to_width`] — measures with the
    /// supplied [`TextWeight`] so the fit matches a bold/medium draw call.
    pub fn elide_to_width_weighted(
        &mut self,
        text: &str,
        size: f32,
        max_width: f32,
        weight: TextWeight,
    ) -> String {
        if text.is_empty() || max_width <= 0.0 {
            return String::new();
        }
        let (full_w, _) = self.measure_weighted(text, size, None, weight);
        if full_w <= max_width {
            return text.to_string();
        }
        const ELLIPSIS: char = '\u{2026}';
        // If the ellipsis alone is wider than the budget, we still return
        // it — a single glyph reads better than an empty slot, and callers
        // that truly have no room won't have called us with text anyway.
        let (ell_w, _) = self.measure_weighted("\u{2026}", size, None, weight);
        if ell_w > max_width {
            return ELLIPSIS.to_string();
        }
        let budget = max_width - ell_w;
        // Grow a prefix char-by-char while it fits the remaining budget.
        // Linear in char count — fine for the short labels (filenames,
        // titles, legend entries) elision targets. Each step re-measures
        // the candidate prefix, which hits the measure cache after warmup.
        let mut best = String::new();
        let mut candidate = String::new();
        for ch in text.chars() {
            candidate.push(ch);
            let (w, _) = self.measure_weighted(&candidate, size, None, weight);
            if w <= budget {
                best.push(ch);
            } else {
                break;
            }
        }
        // Drop a trailing space so the ellipsis doesn't read as " …".
        while best.ends_with(' ') {
            best.pop();
        }
        best.push(ELLIPSIS);
        best
    }

    /// Word-wrap `text` to `max_width` and clamp the result to at most
    /// `max_lines` lines, eliding the last kept line with an ellipsis when
    /// content overflows. Returns the kept lines as a `Vec<String>`,
    /// already wrapped — draw each entry on its own baseline at
    /// `line_height = size * 1.25`.
    ///
    /// `max_lines == 0` returns an empty vec. A single-line clamp
    /// (`max_lines == 1`) degenerates to [`Self::elide_to_width`].
    pub fn clamp_to_lines(
        &mut self,
        text: &str,
        size: f32,
        max_width: f32,
        max_lines: usize,
    ) -> Vec<String> {
        if max_lines == 0 || text.is_empty() || max_width <= 0.0 {
            return Vec::new();
        }
        if max_lines == 1 {
            return vec![self.elide_to_width(text, size, max_width)];
        }
        let wrapped = self.wrap_lines(text, size, max_width);
        if wrapped.len() <= max_lines {
            return wrapped;
        }
        // Overflow: keep the first `max_lines - 1` whole lines, then merge
        // the remaining text onto the final line and elide it so the reader
        // sees the truncation marker.
        let mut kept: Vec<String> = wrapped[..max_lines - 1].to_vec();
        let remainder: String = wrapped[max_lines - 1..].join(" ");
        kept.push(self.elide_to_width(&remainder, size, max_width));
        kept
    }

    /// Greedy word-wrap of `text` at `max_width`. A word longer than the
    /// budget is placed on its own line (no mid-word breaking — the
    /// per-line elision in [`Self::clamp_to_lines`] handles overlong
    /// single words). Internal helper for the clamp; exposed for tests.
    pub(crate) fn wrap_lines(&mut self, text: &str, size: f32, max_width: f32) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for word in text.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
                continue;
            }
            let candidate = format!("{current} {word}");
            let (w, _) = self.measure(&candidate, size, None);
            if w <= max_width {
                current = candidate;
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
    }

    pub fn draw_text(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
    ) {
        self.draw_text_weighted(scene, text, size, color, transform, max_width, TextWeight::Regular)
    }

    /// Draw `text` with explicit weight + italic + optional monospace
    /// family override. Used by [`crate::widget::RichTextLine`] to
    /// paint inline-mark spans (bold / italic / code) with the right
    /// glyph styling.
    ///
    /// `monospace = true` switches the font family to the system's
    /// `Family::Monospace` so inline code reads as code without the
    /// caller needing to manage a custom font db. Width is consistent
    /// because cosmic-text picks one face for the whole call —
    /// callers wanting accurate measurement should pair with
    /// [`Self::measure_styled`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_styled(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        monospace: bool,
    ) {
        if text.is_empty() { return; }

        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let mut attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        if italic {
            attrs = attrs.style(FontStyle::Italic);
        }
        if monospace {
            attrs = attrs.family(Family::Monospace);
        }
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.set_size(max_width, None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);

        for run in buffer.layout_runs() {
            let mut current_face_id: Option<cosmic_text::fontdb::ID> = None;
            let mut glyphs_to_draw = Vec::new();

            for glyph in run.glyphs.iter() {
                let face_id = glyph.font_id;

                if current_face_id != Some(face_id) && !glyphs_to_draw.is_empty() {
                    self.flush_glyphs(scene, current_face_id.unwrap(), &glyphs_to_draw, size, color, transform, run.line_y);
                    glyphs_to_draw.clear();
                }

                current_face_id = Some(face_id);
                glyphs_to_draw.push(glyph.clone());
            }

            if let Some(face_id) = current_face_id {
                if !glyphs_to_draw.is_empty() {
                    self.flush_glyphs(scene, face_id, &glyphs_to_draw, size, color, transform, run.line_y);
                }
            }
        }
    }

    /// Measure `text` with style attributes. Mirrors
    /// [`Self::draw_text_styled`]; used by [`crate::widget::RichTextLine`]
    /// to compute per-segment advances when painting inline-mark spans.
    /// NOT cached — these calls are rare (one per segment per frame)
    /// and the cache key would need to include the italic/mono flags
    /// too, which isn't worth the complication right now.
    pub fn measure_styled(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        monospace: bool,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        // Fast path: regular non-italic non-mono → reuse the cached
        // measure_weighted, which is the hottest path.
        if !italic && !monospace {
            return self.measure_weighted(text, size, max_width, weight);
        }
        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let mut attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        if italic {
            attrs = attrs.style(FontStyle::Italic);
        }
        if monospace {
            attrs = attrs.family(Family::Monospace);
        }
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.set_size(max_width, None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);
        let mut width = 0.0f32;
        let mut lines = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        (width, lines as f32 * line_height)
    }

    /// Same as [`TextRenderer::draw_text`] but takes an explicit typographic
    /// [`TextWeight`]. Use this for chrome labels that need to lift off the
    /// body (e.g. top-bar clock at [`TextWeight::Bold`], tab titles at
    /// [`TextWeight::Semibold`]). See visual-gap #7.
    pub fn draw_text_weighted(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
        weight: TextWeight,
    ) {
        if text.is_empty() { return; }

        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        // cosmic-text 0.19: setters are lazy and no longer take FontSystem;
        // set_text gained a trailing `Option<Align>` arg.
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.set_size(max_width, None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);

        for run in buffer.layout_runs() {
            let mut current_face_id: Option<cosmic_text::fontdb::ID> = None;
            let mut glyphs_to_draw = Vec::new();

            for glyph in run.glyphs.iter() {
                let face_id = glyph.font_id;

                if current_face_id != Some(face_id) && !glyphs_to_draw.is_empty() {
                    self.flush_glyphs(scene, current_face_id.unwrap(), &glyphs_to_draw, size, color, transform, run.line_y);
                    glyphs_to_draw.clear();
                }
                
                current_face_id = Some(face_id);
                glyphs_to_draw.push(glyph.clone());
            }

            if let Some(face_id) = current_face_id {
                if !glyphs_to_draw.is_empty() {
                    self.flush_glyphs(scene, face_id, &glyphs_to_draw, size, color, transform, run.line_y);
                }
            }
        }
    }

    /// Draw a [`crate::richtext::RichTextLayout`] (attributed, wrapped text) in a
    /// single `color`, including its underline decorations. The companion of
    /// [`Self::draw_text`] for rich, hit-testable text.
    pub fn draw_rich(
        &mut self,
        scene: &mut Scene,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
    ) {
        let resolve = |c: Option<cosmic_text::Color>| {
            c.map(|c| Color::from_rgb8(c.r(), c.g(), c.b())).unwrap_or(color)
        };
        for run in layout.buffer.layout_runs() {
            let mut face: Option<cosmic_text::fontdb::ID> = None;
            let mut cur_color: Option<cosmic_text::Color> = None;
            let mut batch: Vec<cosmic_text::LayoutGlyph> = Vec::new();
            let mut size = 16.0f32;
            for g in run.glyphs.iter() {
                size = g.font_size;
                // Flush when either the face or the per-run colour changes.
                if (face != Some(g.font_id) || cur_color != g.color_opt) && !batch.is_empty() {
                    self.flush_glyphs(scene, face.unwrap(), &batch, size, resolve(cur_color), transform, run.line_y);
                    batch.clear();
                }
                face = Some(g.font_id);
                cur_color = g.color_opt;
                batch.push(g.clone());
            }
            if let Some(f) = face {
                if !batch.is_empty() {
                    self.flush_glyphs(scene, f, &batch, size, resolve(cur_color), transform, run.line_y);
                }
            }
        }
        for &(s, e) in &layout.underlines {
            for (x0, _y0, x1, y1) in layout.selection_rects(s, e) {
                let uy = (y1 - 2.0) as f64;
                let rect = vello::kurbo::Rect::new(x0 as f64, uy, x1 as f64, uy + 1.5);
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
            }
        }
        for &(s, e) in &layout.strikes {
            for (x0, y0, x1, y1) in layout.selection_rects(s, e) {
                let my = ((y0 + y1) / 2.0) as f64;
                let rect = vello::kurbo::Rect::new(x0 as f64, my, x1 as f64, my + 1.5);
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
            }
        }
    }

    fn flush_glyphs(
        &mut self,
        scene: &mut Scene,
        face_id: cosmic_text::fontdb::ID,
        glyphs: &[cosmic_text::LayoutGlyph],
        size: f32,
        color: Color,
        transform: Affine,
        line_y: f32,
    ) {
        let vello_font = self.font_cache.entry(face_id).or_insert_with(|| {
            let font_data = self.font_system.db().face(face_id).and_then(|f| {
                match &f.source {
                    cosmic_text::fontdb::Source::Binary(data) => Some(data.as_ref().as_ref().to_vec()),
                    cosmic_text::fontdb::Source::File(path) => fs::read(path).ok(),
                    cosmic_text::fontdb::Source::SharedFile(_, data) => Some(data.as_ref().as_ref().to_vec()),
                }
            });
            
            if let Some(data) = font_data {
                Font::new(data.into(), 0)
            } else {
                Font::new(vec![].into(), 0)
            }
        });

        let vello_glyphs: Vec<vello::Glyph> = glyphs.iter().map(|g| {
            vello::Glyph {
                id: g.glyph_id as u32,
                x: g.x,
                y: line_y + g.y,
            }
        }).collect();

        scene.draw_glyphs(vello_font)
            .font_size(size)
            .brush(color)
            .transform(transform)
            .draw(
                StyleRef::Fill(vello::peniko::Fill::NonZero),
                vello_glyphs.into_iter()
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Visual-gap #7: the named weight tiers must resolve to Tailwind's
    /// numeric OpenType weights so chrome call sites can ask for
    /// `TextWeight::Bold` and trust they will get a `Weight(700)` handoff
    /// to cosmic-text. Any drift here would silently collapse every
    /// chrome label back onto the fallback font's single weight.
    #[test]
    fn named_weights_resolve_to_tailwind_numeric_values() {
        assert_eq!(TextWeight::Regular.to_u16(), 400);
        assert_eq!(TextWeight::Medium.to_u16(), 500);
        assert_eq!(TextWeight::Semibold.to_u16(), 600);
        assert_eq!(TextWeight::Bold.to_u16(), 700);
        assert_eq!(TextWeight::Bold.to_cosmic_weight(), Weight(700));
        assert_eq!(TextWeight::Regular.to_cosmic_weight(), Weight::NORMAL);
        // Default must be Regular so existing call sites that go through
        // the legacy `draw_text` wrapper keep their 400-weight behaviour.
        assert_eq!(TextWeight::default(), TextWeight::Regular);
    }

    /// Measuring the same string twice should hit the cache the
    /// second time — the dominant payoff of Unit 4. Without this,
    /// every widget's `draw` pays a full cosmic-text shape per
    /// frame.
    #[test]
    fn measure_cache_returns_hit_on_second_call() {
        let mut tr = TextRenderer::new();
        let label = "Hello, gallery!";
        let (w1, h1) = tr.measure(label, 16.0, None);
        let (hits1, misses1) = tr.cache_stats();
        assert_eq!(misses1, 1);
        assert_eq!(hits1, 0);

        let (w2, h2) = tr.measure(label, 16.0, None);
        let (hits2, misses2) = tr.cache_stats();
        assert_eq!(hits2, 1, "second measure of same string must hit");
        assert_eq!(misses2, 1, "miss count unchanged after a hit");
        assert_eq!((w1, h1), (w2, h2), "cached value must equal recomputed");
    }

    /// A different `size` is a different cache entry — the cached
    /// (width, height) at one size is no good for another.
    #[test]
    fn measure_cache_keys_on_size() {
        let mut tr = TextRenderer::new();
        let _ = tr.measure("abc", 12.0, None);
        let _ = tr.measure("abc", 16.0, None);
        let _ = tr.measure("abc", 12.0, None); // hit
        let (hits, misses) = tr.cache_stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 2);
    }

    /// Text that already fits is returned verbatim — no ellipsis is
    /// appended and the budget check uses the real measured width.
    #[test]
    fn elide_returns_text_unchanged_when_it_fits() {
        let mut tr = TextRenderer::new();
        let s = "short";
        let (w, _) = tr.measure(s, 14.0, None);
        // Give a budget comfortably above the measured width.
        let out = tr.elide_to_width(s, 14.0, w + 50.0);
        assert_eq!(out, s, "fitting text must be returned unchanged");
        assert!(!out.ends_with('\u{2026}'), "no ellipsis when it fits");
    }

    /// Text wider than the budget is truncated and ends with the ellipsis
    /// glyph, and the truncated string measures within the budget.
    #[test]
    fn elide_truncates_and_appends_ellipsis() {
        let mut tr = TextRenderer::new();
        let s = "the quick brown fox jumps over the lazy dog";
        let (full_w, _) = tr.measure(s, 14.0, None);
        let budget = full_w / 3.0; // force truncation
        let out = tr.elide_to_width(s, 14.0, budget);
        assert!(out.ends_with('\u{2026}'), "truncated text must end with …, got {out:?}");
        assert!(out.chars().count() < s.chars().count(), "must drop characters");
        let (out_w, _) = tr.measure(&out, 14.0, None);
        assert!(
            out_w <= budget + 0.5,
            "elided string ({out_w}) must fit the budget ({budget})",
        );
    }

    /// A zero/empty budget yields an empty string; empty input yields
    /// empty output. Defensive edge cases callers rely on.
    #[test]
    fn elide_edge_cases() {
        let mut tr = TextRenderer::new();
        assert_eq!(tr.elide_to_width("", 14.0, 100.0), "");
        assert_eq!(tr.elide_to_width("abc", 14.0, 0.0), "");
    }

    /// A multi-line clamp keeps at most `max_lines` lines, and when the
    /// content overflows the final kept line carries the ellipsis.
    #[test]
    fn clamp_to_lines_caps_line_count_and_elides_last() {
        let mut tr = TextRenderer::new();
        let paragraph =
            "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
             incididunt ut labore et dolore magna aliqua ut enim ad minim veniam quis";
        // Narrow width forces several wrapped lines; cap to 2.
        let lines = tr.clamp_to_lines(paragraph, 14.0, 120.0, 2);
        assert!(lines.len() <= 2, "must not exceed max_lines, got {}", lines.len());
        assert_eq!(lines.len(), 2, "this paragraph wraps to more than 2 lines");
        assert!(
            lines.last().unwrap().ends_with('\u{2026}'),
            "overflowing clamp must elide the final line: {:?}",
            lines.last(),
        );
    }

    /// `max_lines == 1` degenerates to single-line elision; `max_lines == 0`
    /// returns nothing.
    #[test]
    fn clamp_to_lines_degenerate_cases() {
        let mut tr = TextRenderer::new();
        assert!(tr.clamp_to_lines("anything", 14.0, 80.0, 0).is_empty());
        let one = tr.clamp_to_lines("a fairly long single line of text here", 14.0, 60.0, 1);
        assert_eq!(one.len(), 1);
        assert!(one[0].ends_with('\u{2026}'), "single-line clamp elides");
    }

    /// Short content that fits within `max_lines` is returned wrapped but
    /// un-elided.
    #[test]
    fn clamp_to_lines_no_overflow_keeps_all() {
        let mut tr = TextRenderer::new();
        let lines = tr.clamp_to_lines("two words", 14.0, 1000.0, 3);
        assert_eq!(lines, vec!["two words".to_string()]);
        assert!(!lines[0].ends_with('\u{2026}'));
    }

    /// `bump_generation` blows away every cached entry — the next
    /// measure of a previously-seen string registers as a miss.
    #[test]
    fn bump_generation_invalidates_cache() {
        let mut tr = TextRenderer::new();
        let _ = tr.measure("abc", 12.0, None);
        let _ = tr.measure("abc", 12.0, None); // hit
        tr.bump_generation();
        let _ = tr.measure("abc", 12.0, None); // miss after bump
        let (hits, misses) = tr.cache_stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 2);
    }
}
