use cosmic_text::fontdb::Weight;
use cosmic_text::{
    Buffer, Family, FontSystem, Metrics, Shaping, Style as FontStyle, SwashCache, Wrap,
};
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, FontData as Font, StyleRef};

/// Named weight tiers mapping 1:1 to [`cosmic_text::fontdb::Weight`]'s
/// numeric constants (Tailwind's font-medium/semibold/bold), so call sites
/// read declaratively instead of passing raw u16s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TextWeight {
    /// 400 — body / subtitle / explanatory text. Default.
    #[default]
    Regular,
    /// 500 — most chrome labels (buttons, secondary titles).
    Medium,
    /// 600 — active chrome titles (tab titles, workspace-name pill).
    Semibold,
    /// 700 — h1/h2, clock, numeric monospace values.
    Bold,
}

impl TextWeight {
    pub const fn to_u16(self) -> u16 {
        match self {
            TextWeight::Regular => 400,
            TextWeight::Medium => 500,
            TextWeight::Semibold => 600,
            TextWeight::Bold => 700,
        }
    }

    pub const fn to_cosmic_weight(self) -> Weight {
        Weight(self.to_u16())
    }
}

/// A font family to shape with. The three generic classes resolve to the
/// system's default face for that class; `Named` targets a specific
/// installed family by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum FontChoice {
    #[default]
    SansSerif,
    Serif,
    Monospace,
    Named(String),
}

impl FontChoice {
    fn to_cosmic(&self) -> Family<'_> {
        match self {
            FontChoice::SansSerif => Family::SansSerif,
            FontChoice::Serif => Family::Serif,
            FontChoice::Monospace => Family::Monospace,
            FontChoice::Named(n) => Family::Name(n),
        }
    }

    /// A stable hash of the choice, used as part of the shape-cache key so two
    /// families of the same text/size/weight are cached separately.
    fn cache_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        match self {
            FontChoice::SansSerif => 0u8.hash(&mut h),
            FontChoice::Serif => 1u8.hash(&mut h),
            FontChoice::Monospace => 2u8.hash(&mut h),
            FontChoice::Named(n) => {
                3u8.hash(&mut h);
                n.hash(&mut h);
            }
        }
        h.finish()
    }
}

/// Hashable cache key for measured-text results. `max_width` is encoded as a
/// `u32` bit-pattern so `f32` (not `Hash`) can participate; callers pass a
/// sane finite width or no max, so the bit-pattern is unique-per-input.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MeasureKey {
    text_hash: u64,
    size_bits: u32,
    max_width_bits: u32,
    weight: u16,
    generation: u64,
}

/// Entries before the cache starts evicting on insertion. 4096 x ~64 bytes ~=
/// 256 KB — enough for every static label in a several-hundred-widget tree.
const MEASURE_CACHE_LIMIT: usize = 4096;

/// Hashable cache key for a fully shaped + positioned run of text. Mirrors
/// [`MeasureKey`] but adds `italic`/`monospace` since they change glyph
/// selection. Colour and transform are deliberately NOT part of the key —
/// they're applied at paint time, so a theme recolour or a moving widget
/// reuses the same cached glyph geometry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ShapeKey {
    text_hash: u64,
    size_bits: u32,
    max_width_bits: u32,
    weight: u16,
    italic: bool,
    family: u64,
    generation: u64,
}

/// One contiguous span of glyphs sharing a single font face, already
/// positioned in the text block's local coordinate space. Painting is then
/// just `scene.draw_glyphs` with the caller's colour + transform.
#[derive(Clone)]
struct GlyphRun {
    face_id: cosmic_text::fontdb::ID,
    glyphs: Vec<vello::Glyph>,
}

/// Upper bound on distinct shaped runs retained across frames. Static
/// chrome labels warm this once; every subsequent frame is a pure `Arc`
/// clone instead of a cosmic-text shape.
const SHAPE_CACHE_LIMIT: usize = 4096;

pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    font_cache: HashMap<cosmic_text::fontdb::ID, Font>,
    /// `(text, size, max_width, weight)` → measured `(width, height)`.
    /// Uncached cosmic-text re-shape per frame measured ~7.5ms in debug
    /// across a 50-widget tree; most widgets re-measure the same static
    /// strings every frame, so caching drops that to a hash lookup.
    measure_cache: HashMap<MeasureKey, (f32, f32)>,
    /// Bumped by [`Self::bump_generation`] when locale or fonts change.
    /// Entries from prior generations never match a lookup (generation is
    /// part of the key); they age out when the cache hits its limit.
    generation: u64,
    hits: u64,
    misses: u64,
    /// `(text, size, max_width, weight, style)` → shaped + positioned glyph
    /// runs. An `Arc` value lets a lookup hand the runs back to the paint
    /// loop without cloning the glyph vecs or holding a borrow on the cache
    /// while `font_cache` is touched.
    shape_cache: HashMap<ShapeKey, Arc<Vec<GlyphRun>>>,
    /// Surfaced via [`Self::take_shape_stats`] so the frame profiler
    /// (`QUICK_UI_PERF`) can attribute draw-phase time between cosmic-text
    /// and Vello encoding.
    shape_hits: u64,
    shape_misses: u64,
    shape_ns: u128,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
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
            shape_cache: HashMap::with_capacity(256),
            shape_hits: 0,
            shape_misses: 0,
            shape_ns: 0,
        }
    }

    /// Take-and-reset the per-frame shape-cache stats:
    /// `(hits, misses, nanoseconds_spent_shaping)`, so each call reports only
    /// that frame's shaping work.
    pub fn take_shape_stats(&mut self) -> (u64, u64, u128) {
        let out = (self.shape_hits, self.shape_misses, self.shape_ns);
        self.shape_hits = 0;
        self.shape_misses = 0;
        self.shape_ns = 0;
        out
    }

    /// Useful for benchmarks asserting "the static-label widgets warmed the
    /// cache and subsequent frames are pure hits".
    pub fn cache_stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Call this when the font set or active locale changes; cosmic-text
    /// will shape glyphs differently after either swap, so stale numbers
    /// would let widgets compute wrong layouts.
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.measure_cache.clear();
        // Entries key on `generation` so stale ones would never match
        // anyway; the explicit clear reclaims their memory immediately.
        self.shape_cache.clear();
    }

    fn measure_lookup(&mut self, key: MeasureKey) -> Option<(f32, f32)> {
        if let Some(v) = self.measure_cache.get(&key).copied() {
            self.hits += 1;
            return Some(v);
        }
        self.misses += 1;
        None
    }

    /// Evicts at random when the table is full — cheap and good enough since
    /// most missed entries are one-off animated values (Spinner dots,
    /// scrubbing slider value) that aren't worth keeping anyway.
    fn measure_insert(&mut self, key: MeasureKey, value: (f32, f32)) {
        if self.measure_cache.len() >= MEASURE_CACHE_LIMIT
            && let Some(victim) = self.measure_cache.keys().next().copied()
        {
            self.measure_cache.remove(&victim);
        }
        self.measure_cache.insert(key, value);
    }

    fn make_measure_key(
        &self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
    ) -> MeasureKey {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        MeasureKey {
            text_hash: hasher.finish(),
            size_bits: size.to_bits(),
            max_width_bits: max_width.map_or(u32::MAX, f32::to_bits),
            weight: weight.to_u16(),
            generation: self.generation,
        }
    }

    pub fn measure(&mut self, text: &str, size: f32, max_width: Option<f32>) -> (f32, f32) {
        self.measure_weighted(text, size, max_width, TextWeight::Regular)
    }

    /// Same as [`TextRenderer::measure`] but respects a typographic weight.
    /// Heavier weights generally advance wider, so measurements must use
    /// the same weight the draw path will render with.
    pub fn measure_weighted(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }

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

    /// Truncate `text` to fit within `max_width` on a single line, appending
    /// an ellipsis when anything was dropped; returns just the ellipsis when
    /// even that doesn't fit. Measures with [`TextWeight::Regular`]; pair
    /// with [`Self::elide_to_width_weighted`] when the draw weight differs.
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
        // A single glyph reads better than an empty slot when even the
        // ellipsis doesn't fit.
        let (ell_w, _) = self.measure_weighted("\u{2026}", size, None, weight);
        if ell_w > max_width {
            return ELLIPSIS.to_string();
        }
        let budget = max_width - ell_w;
        // Linear in char count — fine for the short labels this targets.
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

    /// Word-wraps `text` to `max_width` and clamps to at most `max_lines`
    /// lines, eliding the last kept line when content overflows. Draw each
    /// returned line on its own baseline at `line_height = size * 1.25`.
    /// `max_lines == 1` degenerates to [`Self::elide_to_width`].
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
        // Keep the first `max_lines - 1` whole lines, then merge the rest
        // onto the final line and elide it as the truncation marker.
        let mut kept: Vec<String> = wrapped[..max_lines - 1].to_vec();
        let remainder: String = wrapped[max_lines - 1..].join(" ");
        kept.push(self.elide_to_width(&remainder, size, max_width));
        kept
    }

    /// Greedy word-wrap of `text` at `max_width`. A word longer than the
    /// budget is placed on its own line (no mid-word breaking — the per-line
    /// elision in [`Self::clamp_to_lines`] handles overlong single words).
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
        self.draw_text_weighted(
            scene,
            text,
            size,
            color,
            transform,
            max_width,
            TextWeight::Regular,
        );
    }

    /// Draw `text` with explicit weight + italic + optional monospace family
    /// override, used by [`crate::widget::RichTextLine`] to paint inline-mark
    /// spans. Pair with [`Self::measure_styled`] for accurate measurement.
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
        if text.is_empty() {
            return;
        }
        let family = if monospace {
            FontChoice::Monospace
        } else {
            FontChoice::SansSerif
        };
        let runs = self.shaped_runs(text, size, max_width, weight, italic, &family);
        self.paint_runs(scene, &runs, size, color, transform);
    }

    /// Draw `text` shaped in an explicit [`FontChoice`] family (serif / a
    /// named installed family / …) with a weight and italic flag.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_font(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        family: &FontChoice,
    ) {
        if text.is_empty() {
            return;
        }
        let runs = self.shaped_runs(text, size, max_width, weight, italic, family);
        self.paint_runs(scene, &runs, size, color, transform);
    }

    /// Measure `text` shaped in an explicit [`FontChoice`]. Uncached (the
    /// family varies enough that it isn't worth a key); callers measure
    /// during a cached layout pass, so this runs only when it recomputes.
    #[allow(clippy::too_many_arguments)]
    pub fn measure_font(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        family: &FontChoice,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let mut attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        if italic {
            attrs = attrs.style(FontStyle::Italic);
        }
        attrs = attrs.family(family.to_cosmic());
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

    /// Distinct family names of installed faces, sorted and de-duplicated
    /// across weights/styles — the pool an app's font picker offers.
    pub fn system_families(&self) -> Vec<String> {
        use std::collections::BTreeSet;
        let mut set: BTreeSet<String> = BTreeSet::new();
        for face in self.font_system.db().faces() {
            if let Some((name, _)) = face.families.first() {
                // Skip internal/placeholder families with no real name.
                if !name.trim().is_empty() {
                    set.insert(name.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    /// Measure `text` with style attributes. Mirrors [`Self::draw_text_styled`].
    /// NOT cached — these calls are rare (one per segment per frame).
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
    /// [`TextWeight`], for chrome labels that need to lift off the body.
    // Standard immediate-mode paint-helper signature for this vello
    // renderer (scene + transform + text_renderer + theme, plus this
    // draw's own content); every parameter is independently load-bearing.
    #[allow(clippy::too_many_arguments)]
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
        if text.is_empty() {
            return;
        }
        let runs = self.shaped_runs(text, size, max_width, weight, false, &FontChoice::SansSerif);
        self.paint_runs(scene, &runs, size, color, transform);
    }

    /// Shape `text` into positioned [`GlyphRun`]s, caching the result. On a
    /// hit this is an `Arc` clone; on a miss it runs cosmic-text once.
    fn shaped_runs(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
        italic: bool,
        family: &FontChoice,
    ) -> Arc<Vec<GlyphRun>> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let key = ShapeKey {
            text_hash: hasher.finish(),
            size_bits: size.to_bits(),
            max_width_bits: max_width.map_or(u32::MAX, f32::to_bits),
            weight: weight.to_u16(),
            italic,
            family: family.cache_hash(),
            generation: self.generation,
        };
        if let Some(runs) = self.shape_cache.get(&key) {
            self.shape_hits += 1;
            return runs.clone();
        }
        self.shape_misses += 1;

        let t0 = Instant::now();
        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let mut attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        if italic {
            attrs = attrs.style(FontStyle::Italic);
        }
        attrs = attrs.family(family.to_cosmic());
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.set_size(max_width, None);
        buffer.set_wrap(Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Flatten into per-face contiguous runs, baking the baseline into
        // each glyph so paint is a pure transform + colour, no arithmetic.
        let mut runs: Vec<GlyphRun> = Vec::new();
        for run in buffer.layout_runs() {
            let mut current_face: Option<cosmic_text::fontdb::ID> = None;
            let mut glyphs: Vec<vello::Glyph> = Vec::new();
            for g in run.glyphs {
                if current_face != Some(g.font_id) && !glyphs.is_empty() {
                    runs.push(GlyphRun {
                        face_id: current_face.unwrap(),
                        glyphs: std::mem::take(&mut glyphs),
                    });
                }
                current_face = Some(g.font_id);
                glyphs.push(vello::Glyph {
                    id: u32::from(g.glyph_id),
                    x: g.x,
                    y: run.line_y + g.y,
                });
            }
            if let Some(face_id) = current_face
                && !glyphs.is_empty()
            {
                runs.push(GlyphRun { face_id, glyphs });
            }
        }
        self.shape_ns += t0.elapsed().as_nanos();

        let arc = Arc::new(runs);
        if self.shape_cache.len() >= SHAPE_CACHE_LIMIT
            && let Some(victim) = self.shape_cache.keys().next().copied()
        {
            self.shape_cache.remove(&victim);
        }
        self.shape_cache.insert(key, arc.clone());
        arc
    }

    /// The per-face Vello font handle is resolved from `font_cache`
    /// (loaded lazily, once per face for the process).
    fn paint_runs(
        &mut self,
        scene: &mut Scene,
        runs: &[GlyphRun],
        size: f32,
        color: Color,
        transform: Affine,
    ) {
        for run in runs {
            let font = self.vello_font(run.face_id);
            scene
                .draw_glyphs(&font)
                .font_size(size)
                .brush(color)
                .transform(transform)
                .draw(
                    StyleRef::Fill(vello::peniko::Fill::NonZero),
                    run.glyphs.iter().copied(),
                );
        }
    }

    /// Returns a clone — `FontData` is a cheap `Arc`-backed handle, so this
    /// doesn't copy glyph outlines.
    fn vello_font(&mut self, face_id: cosmic_text::fontdb::ID) -> Font {
        self.font_cache
            .entry(face_id)
            .or_insert_with(|| {
                let font_data = self
                    .font_system
                    .db()
                    .face(face_id)
                    .and_then(|f| match &f.source {
                        cosmic_text::fontdb::Source::Binary(data) => {
                            Some(data.as_ref().as_ref().to_vec())
                        }
                        cosmic_text::fontdb::Source::File(path) => fs::read(path).ok(),
                        cosmic_text::fontdb::Source::SharedFile(_, data) => {
                            Some(data.as_ref().as_ref().to_vec())
                        }
                    });
                if let Some(data) = font_data {
                    Font::new(data.into(), 0)
                } else {
                    Font::new(vec![].into(), 0)
                }
            })
            .clone()
    }

    /// Draw a [`crate::richtext::RichTextLayout`] (attributed, wrapped text)
    /// in a single `color`, including its underline decorations.
    pub fn draw_rich(
        &mut self,
        scene: &mut Scene,
        layout: &crate::richtext::RichTextLayout,
        transform: Affine,
        color: Color,
    ) {
        use crate::richtext::BaselineShift;
        let resolve = |c: Option<cosmic_text::Color>| {
            c.map_or(color, |c| Color::from_rgb8(c.r(), c.g(), c.b()))
        };

        // Highlight bands paint behind glyphs so the ink reads on top.
        for &(s, e, [br, bg, bb]) in &layout.backgrounds {
            let band = Color::from_rgb8(br, bg, bb);
            for (x0, y0, x1, y1) in layout.selection_rects(s, e) {
                let rect = vello::kurbo::Rect::new(
                    f64::from(x0),
                    f64::from(y0),
                    f64::from(x1),
                    f64::from(y1),
                );
                scene.fill(vello::peniko::Fill::NonZero, transform, band, None, &rect);
            }
        }

        // Glyphs batch by (face, colour, size, baseline-shift) so a
        // super/subscript run flushes as its own batch with the right size & y.
        for run in layout.buffer.layout_runs() {
            let mut face: Option<cosmic_text::fontdb::ID> = None;
            let mut cur_color: Option<cosmic_text::Color> = None;
            let mut cur_shift = BaselineShift::None;
            let mut batch: Vec<cosmic_text::LayoutGlyph> = Vec::new();
            let mut size = 16.0f32;
            let mut batch_size = 16.0f32;
            for g in run.glyphs {
                let g_shift = layout.baseline_at(g.start);
                // Flush when face, colour, glyph size, or baseline-shift change.
                let changed = face != Some(g.font_id)
                    || cur_color != g.color_opt
                    || (g.font_size - batch_size).abs() > f32::EPSILON
                    || cur_shift != g_shift;
                if changed && !batch.is_empty() {
                    let dy = cur_shift.offset(batch_size);
                    self.flush_glyphs(
                        scene,
                        face.unwrap(),
                        &batch,
                        batch_size,
                        resolve(cur_color),
                        transform,
                        run.line_y + dy,
                    );
                    batch.clear();
                }
                face = Some(g.font_id);
                cur_color = g.color_opt;
                cur_shift = g_shift;
                size = g.font_size;
                batch_size = g.font_size;
                batch.push(g.clone());
            }
            if let Some(f) = face
                && !batch.is_empty()
            {
                let dy = cur_shift.offset(batch_size);
                self.flush_glyphs(
                    scene,
                    f,
                    &batch,
                    size,
                    resolve(cur_color),
                    transform,
                    run.line_y + dy,
                );
            }
        }
        for &(s, e) in &layout.underlines {
            for (x0, _y0, x1, y1) in layout.selection_rects(s, e) {
                let uy = f64::from(y1 - 2.0);
                let rect = vello::kurbo::Rect::new(f64::from(x0), uy, f64::from(x1), uy + 1.5);
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
            }
        }
        for &(s, e) in &layout.strikes {
            for (x0, y0, x1, y1) in layout.selection_rects(s, e) {
                let my = f64::from(f32::midpoint(y0, y1));
                let rect = vello::kurbo::Rect::new(f64::from(x0), my, f64::from(x1), my + 1.5);
                scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &rect);
            }
        }
    }

    // Standard immediate-mode paint-helper signature for this vello
    // renderer (scene + transform + text_renderer + theme, plus this
    // draw's own content); every parameter is independently load-bearing.
    #[allow(clippy::too_many_arguments)]
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
        let vello_font = self.vello_font(face_id);

        let vello_glyphs: Vec<vello::Glyph> = glyphs
            .iter()
            .map(|g| vello::Glyph {
                id: u32::from(g.glyph_id),
                x: g.x,
                y: line_y + g.y,
            })
            .collect();

        scene
            .draw_glyphs(&vello_font)
            .font_size(size)
            .brush(color)
            .transform(transform)
            .draw(
                StyleRef::Fill(vello::peniko::Fill::NonZero),
                vello_glyphs.into_iter(),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drift here would silently collapse every chrome label onto the
    /// fallback weight.
    #[test]
    fn named_weights_resolve_to_tailwind_numeric_values() {
        assert_eq!(TextWeight::Regular.to_u16(), 400);
        assert_eq!(TextWeight::Medium.to_u16(), 500);
        assert_eq!(TextWeight::Semibold.to_u16(), 600);
        assert_eq!(TextWeight::Bold.to_u16(), 700);
        assert_eq!(TextWeight::Bold.to_cosmic_weight(), Weight(700));
        assert_eq!(TextWeight::Regular.to_cosmic_weight(), Weight::NORMAL);
        // Default must be Regular so call sites through `draw_text` keep
        // their 400-weight behaviour.
        assert_eq!(TextWeight::default(), TextWeight::Regular);
    }

    /// Measuring the same string twice should hit the cache the second time
    /// — without this, every widget's `draw` pays a full cosmic-text shape.
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

    /// A highlighted run must add a real fill: the highlighted scene must
    /// encode strictly more paths than the plain one.
    #[test]
    fn background_span_emits_an_extra_fill_in_the_scene() {
        use crate::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
        let mut tr = TextRenderer::new();

        let plain_spans = [RichSpan::new("highlighted")];
        let plain = RichTextLayout::shape(
            &mut tr.font_system,
            &plain_spans,
            false,
            16.0,
            400.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        let hl_spans = [RichSpan::new("highlighted").background(Some([255, 235, 59]))];
        let highlighted = RichTextLayout::shape(
            &mut tr.font_system,
            &hl_spans,
            false,
            16.0,
            400.0,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );

        let mut plain_scene = Scene::new();
        tr.draw_rich(
            &mut plain_scene,
            &plain,
            Affine::IDENTITY,
            Color::from_rgb8(0, 0, 0),
        );
        let plain_paths = plain_scene.encoding().n_paths;

        let mut hl_scene = Scene::new();
        tr.draw_rich(
            &mut hl_scene,
            &highlighted,
            Affine::IDENTITY,
            Color::from_rgb8(0, 0, 0),
        );
        let hl_paths = hl_scene.encoding().n_paths;

        assert!(
            hl_paths > plain_paths,
            "highlight must add a fill path: plain={plain_paths} highlighted={hl_paths}",
        );
    }

    #[test]
    fn elide_returns_text_unchanged_when_it_fits() {
        let mut tr = TextRenderer::new();
        let s = "short";
        let (w, _) = tr.measure(s, 14.0, None);
        let out = tr.elide_to_width(s, 14.0, w + 50.0);
        assert_eq!(out, s, "fitting text must be returned unchanged");
        assert!(!out.ends_with('\u{2026}'), "no ellipsis when it fits");
    }

    #[test]
    fn elide_truncates_and_appends_ellipsis() {
        let mut tr = TextRenderer::new();
        let s = "the quick brown fox jumps over the lazy dog";
        let (full_w, _) = tr.measure(s, 14.0, None);
        let budget = full_w / 3.0; // force truncation
        let out = tr.elide_to_width(s, 14.0, budget);
        assert!(
            out.ends_with('\u{2026}'),
            "truncated text must end with …, got {out:?}"
        );
        assert!(
            out.chars().count() < s.chars().count(),
            "must drop characters"
        );
        let (out_w, _) = tr.measure(&out, 14.0, None);
        assert!(
            out_w <= budget + 0.5,
            "elided string ({out_w}) must fit the budget ({budget})",
        );
    }

    /// A zero/empty budget yields empty output; so does empty input.
    #[test]
    fn elide_edge_cases() {
        let mut tr = TextRenderer::new();
        assert_eq!(tr.elide_to_width("", 14.0, 100.0), "");
        assert_eq!(tr.elide_to_width("abc", 14.0, 0.0), "");
    }

    #[test]
    fn clamp_to_lines_caps_line_count_and_elides_last() {
        let mut tr = TextRenderer::new();
        let paragraph = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
             incididunt ut labore et dolore magna aliqua ut enim ad minim veniam quis";
        // Narrow width forces several wrapped lines; cap to 2.
        let lines = tr.clamp_to_lines(paragraph, 14.0, 120.0, 2);
        assert!(
            lines.len() <= 2,
            "must not exceed max_lines, got {}",
            lines.len()
        );
        assert_eq!(lines.len(), 2, "this paragraph wraps to more than 2 lines");
        assert!(
            lines.last().unwrap().ends_with('\u{2026}'),
            "overflowing clamp must elide the final line: {:?}",
            lines.last(),
        );
    }

    #[test]
    fn clamp_to_lines_degenerate_cases() {
        let mut tr = TextRenderer::new();
        assert!(tr.clamp_to_lines("anything", 14.0, 80.0, 0).is_empty());
        let one = tr.clamp_to_lines("a fairly long single line of text here", 14.0, 60.0, 1);
        assert_eq!(one.len(), 1);
        assert!(one[0].ends_with('\u{2026}'), "single-line clamp elides");
    }

    #[test]
    fn clamp_to_lines_no_overflow_keeps_all() {
        let mut tr = TextRenderer::new();
        let lines = tr.clamp_to_lines("two words", 14.0, 1000.0, 3);
        assert_eq!(lines, vec!["two words".to_string()]);
        assert!(!lines[0].ends_with('\u{2026}'));
    }

    /// The DRAW path caches shaped glyph geometry: drawing the same string
    /// twice shapes it once.
    #[test]
    fn shape_cache_reshapes_once_for_repeated_draw() {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let t = Affine::IDENTITY;
        let c = Color::WHITE;
        tr.draw_text_weighted(&mut scene, "Equals =", 20.0, c, t, None, TextWeight::Bold);
        let (_h, m1, _ns1) = tr.take_shape_stats();
        assert_eq!(m1, 1, "first draw must shape once (miss)");

        // Every redraw must be a cache hit; zero reshapes.
        for _ in 0..5 {
            tr.draw_text_weighted(&mut scene, "Equals =", 20.0, c, t, None, TextWeight::Bold);
        }
        let (h, m, ns) = tr.take_shape_stats();
        assert_eq!(m, 0, "repeated identical draws must not reshape");
        assert_eq!(h, 5, "all five redraws must hit the shape cache");
        assert_eq!(ns, 0, "no shaping time on an all-hit frame");
    }

    /// Colour is NOT part of the shape key — a theme recolour of the same
    /// label reuses the cached glyph geometry, making theme switches free.
    #[test]
    fn shape_cache_reuses_geometry_across_recolor() {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let t = Affine::IDENTITY;
        tr.draw_text_weighted(
            &mut scene,
            "Clear",
            16.0,
            Color::WHITE,
            t,
            None,
            TextWeight::Regular,
        );
        let _ = tr.take_shape_stats();
        // Same string + size + weight, different colour (dark→light theme).
        tr.draw_text_weighted(
            &mut scene,
            "Clear",
            16.0,
            Color::BLACK,
            t,
            None,
            TextWeight::Regular,
        );
        let (h, m, _ns) = tr.take_shape_stats();
        assert_eq!(m, 0, "recolour must not reshape");
        assert_eq!(h, 1, "recolour must hit the cached geometry");
    }

    /// A different wrap width is a different shape — wrapping changes
    /// glyph positions, so it must miss and reshape.
    #[test]
    fn shape_cache_keys_on_width() {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let t = Affine::IDENTITY;
        let s = "the quick brown fox";
        tr.draw_text_weighted(
            &mut scene,
            s,
            16.0,
            Color::WHITE,
            t,
            Some(200.0),
            TextWeight::Regular,
        );
        tr.draw_text_weighted(
            &mut scene,
            s,
            16.0,
            Color::WHITE,
            t,
            Some(60.0),
            TextWeight::Regular,
        );
        tr.draw_text_weighted(
            &mut scene,
            s,
            16.0,
            Color::WHITE,
            t,
            Some(200.0),
            TextWeight::Regular,
        ); // hit
        let (h, m, _ns) = tr.take_shape_stats();
        assert_eq!(m, 2, "each distinct width shapes once");
        assert_eq!(h, 1, "the repeated width hits");
    }

    /// `bump_generation` (locale / font hot-swap) invalidates shaped
    /// geometry too, so the next draw reshapes.
    #[test]
    fn bump_generation_invalidates_shape_cache() {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let t = Affine::IDENTITY;
        tr.draw_text_weighted(
            &mut scene,
            "Send",
            16.0,
            Color::WHITE,
            t,
            None,
            TextWeight::Regular,
        );
        let _ = tr.take_shape_stats();
        tr.draw_text_weighted(
            &mut scene,
            "Send",
            16.0,
            Color::WHITE,
            t,
            None,
            TextWeight::Regular,
        );
        let (h, _m, _ns) = tr.take_shape_stats();
        assert_eq!(h, 1, "second draw hits before bump");
        tr.bump_generation();
        tr.draw_text_weighted(
            &mut scene,
            "Send",
            16.0,
            Color::WHITE,
            t,
            None,
            TextWeight::Regular,
        );
        let (_h, m, _ns) = tr.take_shape_stats();
        assert_eq!(m, 1, "draw after bump must reshape");
    }

    /// Two different families must be distinct shape-cache entries (so
    /// serif doesn't collide with sans).
    #[test]
    fn font_choice_families_resolve_and_shape() {
        let mut tr = TextRenderer::new();
        let fams = tr.system_families();
        assert!(!fams.is_empty(), "the system font db exposes some families");

        // Both generic families measure to a positive advance for real text.
        let (ws, _) = tr.measure_font(
            "Reading",
            20.0,
            None,
            TextWeight::Regular,
            false,
            &FontChoice::Serif,
        );
        let (wn, _) = tr.measure_font(
            "Reading",
            20.0,
            None,
            TextWeight::Regular,
            false,
            &FontChoice::SansSerif,
        );
        assert!(ws > 0.0, "serif shapes to a real width");
        assert!(wn > 0.0, "sans shapes to a real width");

        // A specific installed family shapes into glyph runs without panicking.
        let mut scene = Scene::new();
        let named = FontChoice::Named(fams[0].clone());
        tr.draw_text_font(
            &mut scene,
            "Reading",
            20.0,
            Color::WHITE,
            Affine::IDENTITY,
            None,
            TextWeight::Regular,
            false,
            &named,
        );

        // Serif and Sans of the same string are distinct shape-cache entries.
        let _ = tr.take_shape_stats();
        tr.draw_text_font(
            &mut scene,
            "Reading",
            20.0,
            Color::WHITE,
            Affine::IDENTITY,
            None,
            TextWeight::Regular,
            false,
            &FontChoice::Serif,
        );
        tr.draw_text_font(
            &mut scene,
            "Reading",
            20.0,
            Color::WHITE,
            Affine::IDENTITY,
            None,
            TextWeight::Regular,
            false,
            &FontChoice::SansSerif,
        );
        let (_h, m, _ns) = tr.take_shape_stats();
        assert_eq!(m, 2, "serif and sans shape as separate families");
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
