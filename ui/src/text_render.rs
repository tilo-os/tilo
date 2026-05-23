use std::collections::HashMap;
use std::fs;
use cosmic_text::{FontSystem, SwashCache, Buffer, Metrics, Shaping, Wrap};
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
