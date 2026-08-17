use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use cosmic_text::{FontSystem, SwashCache, Buffer, Family, Metrics, Shaping, Style as FontStyle, Wrap};
use cosmic_text::fontdb::Weight;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, FontData as Font, StyleRef};

/// Canonical family name of the toolkit's **embedded** serif face.
///
/// The toolkit bundles a real, well-hinted libre serif (Source Serif 4, SIL
/// OFL 1.1 — see `ui/assets/fonts/`) and registers it into **every**
/// [`FontSystem`] it builds, then points cosmic-text's generic
/// [`Family::Serif`] at it via `set_serif_family`. That makes serif body text
/// resolve **deterministically to a genuine serif**, with no dependency on
/// whatever the host happens to have installed.
///
/// This closes a real trap: a named-but-absent font silently substitutes a
/// proportional/wrong face (cosmic-text falls through to its default sans). In
/// the OS image and the build container — neither of which ships a serif —
/// `Family::Serif` would otherwise render as sans, so a "serif" reading view
/// would look identical to the sans UI. Embedding the face is what guarantees
/// the reader (RSS article body, ebook body, office document body) actually
/// gets serifs.
pub const SERIF_FAMILY: &str = "Source Serif 4";

/// Canonical family name of the toolkit's **embedded** monospace face.
///
/// The toolkit bundles a real, fixed-width libre mono (Source Code Pro, SIL OFL
/// 1.1 — the coding sibling of the embedded serif; see `ui/assets/fonts/`) and
/// registers it into **every** [`FontSystem`] it builds, then points
/// cosmic-text's generic [`Family::Monospace`] at it via
/// `set_monospace_family`. That makes code / fixed-width text resolve
/// **deterministically to a genuine monospace**, with no dependency on whatever
/// the host happens to have installed.
///
/// This closes the same trap the serif embed did, on the axis that matters most
/// for a code tool: a named-but-absent mono (e.g. "JetBrains Mono") silently
/// substitutes a **proportional** face — cosmic-text falls through to its
/// default sans — so a "monospace" grid smears and columns stop lining up. The
/// OS image and the build container ship **no** mono, so `Family::Monospace`
/// would otherwise render as the sans UI face. Embedding a real fixed-width
/// face is what guarantees the terminal, code editor, hex editor, snippet
/// bodies, Typst source, and diff/merge panels actually get aligned columns.
pub const MONO_FAMILY: &str = "Source Code Pro";

/// The four embedded Source Serif 4 faces (regular / italic / bold / bold-
/// italic) so a serif body run picks the right face for emphasis and headings.
/// Baked into the binary — no filesystem or host-font dependency at runtime.
const EMBEDDED_SERIF_FACES: &[&[u8]] = &[
    include_bytes!("../assets/fonts/SourceSerif4-Regular.ttf"),
    include_bytes!("../assets/fonts/SourceSerif4-It.ttf"),
    include_bytes!("../assets/fonts/SourceSerif4-Bold.ttf"),
    include_bytes!("../assets/fonts/SourceSerif4-BoldIt.ttf"),
];

/// The four embedded Source Code Pro faces (regular / italic / bold / bold-
/// italic) so a monospace run picks the right face for bold keywords and
/// emphasised code. Baked into the binary — no filesystem or host-font
/// dependency at runtime.
const EMBEDDED_MONO_FACES: &[&[u8]] = &[
    include_bytes!("../assets/fonts/SourceCodePro-Regular.ttf"),
    include_bytes!("../assets/fonts/SourceCodePro-It.ttf"),
    include_bytes!("../assets/fonts/SourceCodePro-Bold.ttf"),
    include_bytes!("../assets/fonts/SourceCodePro-BoldIt.ttf"),
];

/// Build a [`FontSystem`] with the toolkit's embedded faces registered.
///
/// Loads the host font set (so named installed families still resolve for the
/// font picker), then loads the bundled serif faces and makes the generic
/// serif family point at them. Every [`TextRenderer`] is constructed through
/// here, so serif shaping is identical and host-independent across the whole
/// toolkit — the compositor, every system app, and the offscreen `--shots`
/// renderer alike.
fn build_font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    {
        let db = fs.db_mut();
        for face in EMBEDDED_SERIF_FACES {
            db.load_font_data(face.to_vec());
        }
        for face in EMBEDDED_MONO_FACES {
            db.load_font_data(face.to_vec());
        }
        // Point the generic serif family at the embedded face. Even on a host
        // that ships its own serif, ours wins deterministically — the same
        // glyphs render everywhere qOS runs.
        db.set_serif_family(SERIF_FAMILY);
        // Likewise pin the generic monospace family at the embedded fixed-width
        // face, so `Family::Monospace` / `FontChoice::Monospace` / `FontRole::Mono`
        // resolve to a genuine mono with no host dependency (rather than falling
        // through to a proportional face and smearing every code grid).
        db.set_monospace_family(MONO_FAMILY);
    }
    fs
}

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

/// Fraction of the font size a single-line text run must be lifted so its
/// *optical* centre — roughly the middle of the cap height — lands on the
/// geometric centre of the box it is centred in, level with an icon, avatar,
/// or dot centred by its box in the same row.
///
/// The renderer draws a `size`-px line from the top of a `size * 1.25` line
/// box (see [`TextRenderer::measure_weighted`]), and the baseline sits well
/// below that box's geometric middle. Centring a run by its nominal `size`
/// alone — `y0 + (h - size) / 2` — therefore leaves the text sitting visibly
/// LOW next to a box-centred glyph: measured against rendered output, the
/// text's optical centre lands ~0.2 em below it. This constant is that
/// measured offset, calibrated against the alignment shots.
pub const OPTICAL_LIFT: f64 = 0.2;

/// Top-left `y` for a `size`-px single-line text run so its optical centre
/// lands on the middle of a box of height `h` starting at `y0` — level with an
/// icon, avatar, or dot centred in the same box.
///
/// This is the one canonical vertical-centring helper for the toolkit. Shared
/// widgets (button label, badge / pill / chip text, avatar initials, list-row
/// content) and app draw helpers route their centred-text `y` through it so
/// every surface lines up identically. Prefer this over an inline
/// `y0 + (h - size) / 2`, which reads ~0.2 em low (see [`OPTICAL_LIFT`]).
pub fn centered_text_y(y0: f64, h: f64, size: f32) -> f64 {
    y0 + (h - size as f64) / 2.0 - size as f64 * OPTICAL_LIFT
}

/// Optically-centred top `y` for the two lines of a name-over-subtitle block —
/// a list row's bold name over a dim subtitle — within `[y0, y0 + h]`.
/// `line_advance` is the distance between the two runs' top edges. Both tops
/// are lifted together so the block's optical centre lands on the box middle,
/// level with a box-centred avatar, icon, or swatch in the same row. This is
/// the two-line extension of [`centered_text_y`]'s single-line optical model,
/// and the one canonical helper for list-row content across the toolkit and
/// apps (prefer it over hand-placed `y0 + k` / `y1 - k` line tops, which read
/// the block ~0.2 em low against a box-centred leading glyph).
///
/// Returns `(name_top, subtitle_top)`; the advance between them is preserved
/// exactly, so the pair keeps its intended line spacing while the block as a
/// whole is centred.
pub fn centered_two_line_tops(
    y0: f64,
    h: f64,
    size_top: f32,
    size_bottom: f32,
    line_advance: f64,
) -> (f64, f64) {
    // A run drawn at top `y` has its optical centre at `y + size·(0.5 + LIFT)`.
    let k = 0.5 + OPTICAL_LIFT;
    // Span between the two runs' optical centres = advance + the lift-weighted
    // run heights; centre that span's midpoint on the box middle.
    let optical_extent = line_advance + k * (size_top as f64 + size_bottom as f64);
    let y_top = y0 + h / 2.0 - optical_extent / 2.0;
    (y_top, y_top + line_advance)
}

/// A font family to shape with. The three generic classes resolve to the
/// system's default face for that class (via cosmic-text / fontconfig); `Named`
/// targets a specific installed family by name (e.g. a serif book face the user
/// picked from the font list). This is what lets apps render body text in a
/// chosen serif or custom font rather than only sans/mono.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FontChoice {
    SansSerif,
    Serif,
    Monospace,
    Named(String),
}

impl Default for FontChoice {
    fn default() -> Self {
        FontChoice::SansSerif
    }
}

/// A **semantic** font role, resolved by the OS [`Theme`](crate::theme::Theme)
/// to a concrete [`FontChoice`]. Widgets and apps ask for a role rather than a
/// literal family so the OS controls the mapping in one place — the same way
/// colours and spacing are theme-driven, not hard-coded.
///
/// - [`FontRole::Ui`] — chrome / labels / controls. The default; sans.
/// - [`FontRole::Reading`] — long-form **body copy** in reading views (RSS
///   article body, ebook page, office document body). Maps to the theme's
///   reading face, which defaults to the toolkit's embedded serif
///   ([`SERIF_FAMILY`]). Serif stays **opt-in** per run/view — UI chrome keeps
///   the sans default.
/// - [`FontRole::Mono`] — code / fixed-width numeric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum FontRole {
    #[default]
    Ui,
    Reading,
    Mono,
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

/// Hashable cache key for a fully shaped + positioned run of text.
/// Mirrors [`MeasureKey`] but adds the two style bits (`italic`,
/// `monospace`) because they change glyph selection and therefore the
/// positions. Colour and transform are deliberately NOT part of the key
/// — they're applied at paint time, so a theme recolour or a widget
/// moving on screen reuses the same cached glyph geometry.
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
/// positioned in the text block's local coordinate space (x = advance,
/// y = baseline). Painting is then just `scene.draw_glyphs` with the
/// caller's colour + transform — no reshaping.
#[derive(Clone)]
struct GlyphRun {
    face_id: cosmic_text::fontdb::ID,
    glyphs: Vec<vello::Glyph>,
}

/// Upper bound on distinct shaped runs retained across frames. Each
/// entry is a `Vec<GlyphRun>` holding `vello::Glyph` (12 bytes each);
/// even a tree with a few hundred distinct labels stays well under a
/// megabyte. Static chrome + button labels warm this once and every
/// subsequent frame is a pure `Arc` clone instead of a cosmic-text
/// shape — the dominant per-frame draw cost before this cache existed.
const SHAPE_CACHE_LIMIT: usize = 4096;

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
    /// `(text, size, max_width, weight, style)` → shaped + positioned
    /// glyph runs. This is the payload of the DRAW path: before it
    /// existed, every `draw_text*` call reshaped its string through
    /// cosmic-text on every single frame even when nothing changed. An
    /// `Arc` value lets a lookup hand the runs back to the paint loop
    /// without cloning the glyph vecs and without holding a borrow on
    /// the cache while `font_cache` is touched.
    shape_cache: HashMap<ShapeKey, Arc<Vec<GlyphRun>>>,
    /// Shape-cache hit / miss counters + accumulated shaping time.
    /// Surfaced via [`Self::take_shape_stats`] so the frame profiler
    /// (`QUICK_UI_PERF`) can attribute how much of the draw phase was
    /// spent inside cosmic-text vs. Vello encoding.
    shape_hits: u64,
    shape_misses: u64,
    shape_ns: u128,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            // Build the font system with the embedded serif registered so
            // `FontChoice::Serif` resolves to a real serif everywhere — the
            // compositor, every app, and the headless renderer share one path.
            font_system: build_font_system(),
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
    /// `(hits, misses, nanoseconds_spent_shaping)`. The frame profiler
    /// calls this once per frame so each line reports only that frame's
    /// shaping work. Returns zeros on an all-cached frame — the goal
    /// state for static content.
    pub fn take_shape_stats(&mut self) -> (u64, u64, u128) {
        let out = (self.shape_hits, self.shape_misses, self.shape_ns);
        self.shape_hits = 0;
        self.shape_misses = 0;
        self.shape_ns = 0;
        out
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
        // Shaped glyph geometry depends on the same font set + locale,
        // so it must die alongside the measurements. Entries key on
        // `generation` too, so stale ones would never match anyway; the
        // explicit clear reclaims their memory immediately.
        self.shape_cache.clear();
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
        let family = if monospace { FontChoice::Monospace } else { FontChoice::SansSerif };
        let runs = self.shaped_runs(text, size, max_width, weight, italic, &family);
        self.paint_runs(scene, &runs, size, color, transform);
    }

    /// Draw `text` shaped in an explicit [`FontChoice`] family (serif / a named
    /// installed family / …) with a weight and italic flag. This is how an app
    /// renders body text in a chosen book face rather than only sans/mono.
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
        if text.is_empty() { return; }
        let runs = self.shaped_runs(text, size, max_width, weight, italic, family);
        self.paint_runs(scene, &runs, size, color, transform);
    }

    /// Measure `text` shaped in an explicit [`FontChoice`]. Uncached (the family
    /// varies enough that it isn't worth a key); callers measure during a
    /// cached layout pass, so this runs only when a layout actually recomputes.
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

    /// Distinct family names of installed faces, sorted — the pool an app's
    /// font picker offers so a reader can choose any system family (including
    /// serif book faces). De-duplicated across weights/styles of the same
    /// family.
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

    /// The font-family name(s) each shaped run of `text` actually resolves to
    /// under the given [`FontChoice`], shaping through the **real** draw path
    /// ([`Self::shaped_runs`]). This is how a test proves that
    /// `FontChoice::Serif` lands on the embedded serif face
    /// ([`SERIF_FAMILY`]) — a genuine serif — rather than silently falling
    /// through to the default sans. De-duplicated, in first-seen order.
    pub fn resolved_run_families(
        &mut self,
        text: &str,
        size: f32,
        weight: TextWeight,
        italic: bool,
        family: &FontChoice,
    ) -> Vec<String> {
        let runs = self.shaped_runs(text, size, None, weight, italic, family);
        let mut out: Vec<String> = Vec::new();
        for run in runs.iter() {
            if let Some(face) = self.font_system.db().face(run.face_id) {
                if let Some((name, _)) = face.families.first() {
                    if !out.iter().any(|n| n == name) {
                        out.push(name.clone());
                    }
                }
            }
        }
        out
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
        let runs = self.shaped_runs(text, size, max_width, weight, false, &FontChoice::SansSerif);
        self.paint_runs(scene, &runs, size, color, transform);
    }

    /// Shape `text` under the given size / wrap-width / weight / style
    /// into positioned [`GlyphRun`]s, caching the result. On a hit this
    /// is an `Arc` clone (refcount bump); on a miss it runs cosmic-text
    /// once and stores the geometry. The dominant win of the draw path:
    /// static labels shape once and stay shaped until their text, size,
    /// width, weight, style, or the font generation changes.
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
            max_width_bits: max_width.map(|w| w.to_bits()).unwrap_or(u32::MAX),
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

        // Flatten the layout into per-face contiguous runs, baking the
        // baseline (`line_y + glyph.y`) into each glyph so paint is a
        // pure transform + colour with no per-glyph arithmetic.
        let mut runs: Vec<GlyphRun> = Vec::new();
        for run in buffer.layout_runs() {
            let mut current_face: Option<cosmic_text::fontdb::ID> = None;
            let mut glyphs: Vec<vello::Glyph> = Vec::new();
            for g in run.glyphs.iter() {
                if current_face != Some(g.font_id) && !glyphs.is_empty() {
                    runs.push(GlyphRun {
                        face_id: current_face.unwrap(),
                        glyphs: std::mem::take(&mut glyphs),
                    });
                }
                current_face = Some(g.font_id);
                glyphs.push(vello::Glyph {
                    id: g.glyph_id as u32,
                    x: g.x,
                    y: run.line_y + g.y,
                });
            }
            if let Some(face_id) = current_face {
                if !glyphs.is_empty() {
                    runs.push(GlyphRun { face_id, glyphs });
                }
            }
        }
        self.shape_ns += t0.elapsed().as_nanos();

        let arc = Arc::new(runs);
        if self.shape_cache.len() >= SHAPE_CACHE_LIMIT {
            if let Some(victim) = self.shape_cache.keys().next().copied() {
                self.shape_cache.remove(&victim);
            }
        }
        self.shape_cache.insert(key, arc.clone());
        arc
    }

    /// Paint pre-shaped [`GlyphRun`]s with a single `color` at
    /// `transform`. The per-face Vello font handle is resolved from
    /// `font_cache` (loaded lazily, once per face for the process).
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

    /// Resolve (loading + caching once per face) the Vello font handle
    /// for a cosmic-text face id. Returns a clone — `FontData` is a
    /// cheap `Arc`-backed handle, so this doesn't copy glyph outlines.
    fn vello_font(&mut self, face_id: cosmic_text::fontdb::ID) -> Font {
        self.font_cache
            .entry(face_id)
            .or_insert_with(|| {
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
            })
            .clone()
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
        use crate::richtext::BaselineShift;
        let resolve = |c: Option<cosmic_text::Color>| {
            c.map(|c| Color::from_rgb8(c.r(), c.g(), c.b())).unwrap_or(color)
        };

        // 1. Highlight bands — paint behind glyphs so the ink reads on top.
        //    One fill per (contiguous) run-segment on each visual line.
        for &(s, e, [br, bg, bb]) in &layout.backgrounds {
            let band = Color::from_rgb8(br, bg, bb);
            for (x0, y0, x1, y1) in layout.selection_rects(s, e) {
                let rect = vello::kurbo::Rect::new(x0 as f64, y0 as f64, x1 as f64, y1 as f64);
                scene.fill(vello::peniko::Fill::NonZero, transform, band, None, &rect);
            }
        }

        // 2. Glyphs. We batch by (face, colour, size, baseline-shift) so a
        //    super/subscript run — shaped smaller and drawn with a vertical
        //    offset — flushes as its own batch with the right size & y.
        for run in layout.buffer.layout_runs() {
            let mut face: Option<cosmic_text::fontdb::ID> = None;
            let mut cur_color: Option<cosmic_text::Color> = None;
            let mut cur_shift = BaselineShift::None;
            let mut batch: Vec<cosmic_text::LayoutGlyph> = Vec::new();
            let mut size = 16.0f32;
            let mut batch_size = 16.0f32;
            for g in run.glyphs.iter() {
                let g_shift = layout.baseline_at(g.start);
                // Flush when face, colour, glyph size, or baseline-shift change.
                let changed = face != Some(g.font_id)
                    || cur_color != g.color_opt
                    || (g.font_size - batch_size).abs() > f32::EPSILON
                    || cur_shift != g_shift;
                if changed && !batch.is_empty() {
                    let dy = cur_shift.offset(batch_size);
                    self.flush_glyphs(scene, face.unwrap(), &batch, batch_size, resolve(cur_color), transform, run.line_y + dy);
                    batch.clear();
                }
                face = Some(g.font_id);
                cur_color = g.color_opt;
                cur_shift = g_shift;
                size = g.font_size;
                batch_size = g.font_size;
                batch.push(g.clone());
            }
            if let Some(f) = face {
                if !batch.is_empty() {
                    let dy = cur_shift.offset(batch_size);
                    self.flush_glyphs(scene, f, &batch, size, resolve(cur_color), transform, run.line_y + dy);
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
        let vello_font = self.vello_font(face_id);

        let vello_glyphs: Vec<vello::Glyph> = glyphs.iter().map(|g| {
            vello::Glyph {
                id: g.glyph_id as u32,
                x: g.x,
                y: line_y + g.y,
            }
        }).collect();

        scene.draw_glyphs(&vello_font)
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

    /// A highlighted run must add a real fill to the draw output. We render
    /// the same text twice — once with a background span, once without — and
    /// assert the highlighted scene encodes strictly more paths (the extra
    /// path is the highlight band `draw_rich` fills before the glyphs).
    #[test]
    fn background_span_emits_an_extra_fill_in_the_scene() {
        use crate::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
        let mut tr = TextRenderer::new();

        let plain_spans = [RichSpan::new("highlighted")];
        let plain = RichTextLayout::shape(
            &mut tr.font_system, &plain_spans, false, 16.0, 400.0,
            TextAlign::Left, FontFamily::Sans, vec![], vec![], 1.0,
        );
        let hl_spans = [RichSpan::new("highlighted").background(Some([255, 235, 59]))];
        let highlighted = RichTextLayout::shape(
            &mut tr.font_system, &hl_spans, false, 16.0, 400.0,
            TextAlign::Left, FontFamily::Sans, vec![], vec![], 1.0,
        );

        let mut plain_scene = Scene::new();
        tr.draw_rich(&mut plain_scene, &plain, Affine::IDENTITY, Color::from_rgb8(0, 0, 0));
        let plain_paths = plain_scene.encoding().n_paths;

        let mut hl_scene = Scene::new();
        tr.draw_rich(&mut hl_scene, &highlighted, Affine::IDENTITY, Color::from_rgb8(0, 0, 0));
        let hl_paths = hl_scene.encoding().n_paths;

        assert!(
            hl_paths > plain_paths,
            "highlight must add a fill path: plain={plain_paths} highlighted={hl_paths}",
        );
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

    /// The DRAW path caches shaped glyph geometry: drawing the same
    /// string twice shapes it once. This is the fix for the per-frame
    /// re-shape that dominated draw time.
    #[test]
    fn shape_cache_reshapes_once_for_repeated_draw() {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let t = Affine::IDENTITY;
        let c = Color::WHITE;
        tr.draw_text_weighted(&mut scene, "Equals =", 20.0, c, t, None, TextWeight::Bold);
        let (_h, m1, _ns1) = tr.take_shape_stats();
        assert_eq!(m1, 1, "first draw must shape once (miss)");

        // Redraw the same label 5 more times — as a widget tree does
        // every frame. Every one must be a cache hit; zero reshapes.
        for _ in 0..5 {
            tr.draw_text_weighted(&mut scene, "Equals =", 20.0, c, t, None, TextWeight::Bold);
        }
        let (h, m, ns) = tr.take_shape_stats();
        assert_eq!(m, 0, "repeated identical draws must not reshape");
        assert_eq!(h, 5, "all five redraws must hit the shape cache");
        assert_eq!(ns, 0, "no shaping time on an all-hit frame");
    }

    /// Colour is NOT part of the shape key — a theme recolour of the
    /// same label reuses the cached glyph geometry. Guards the design
    /// choice that makes theme switches free of reshaping.
    #[test]
    fn shape_cache_reuses_geometry_across_recolor() {
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let t = Affine::IDENTITY;
        tr.draw_text_weighted(&mut scene, "Clear", 16.0, Color::WHITE, t, None, TextWeight::Regular);
        let _ = tr.take_shape_stats();
        // Same string + size + weight, different colour (dark→light theme).
        tr.draw_text_weighted(&mut scene, "Clear", 16.0, Color::BLACK, t, None, TextWeight::Regular);
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
        tr.draw_text_weighted(&mut scene, s, 16.0, Color::WHITE, t, Some(200.0), TextWeight::Regular);
        tr.draw_text_weighted(&mut scene, s, 16.0, Color::WHITE, t, Some(60.0), TextWeight::Regular);
        tr.draw_text_weighted(&mut scene, s, 16.0, Color::WHITE, t, Some(200.0), TextWeight::Regular); // hit
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
        tr.draw_text_weighted(&mut scene, "Send", 16.0, Color::WHITE, t, None, TextWeight::Regular);
        let _ = tr.take_shape_stats();
        tr.draw_text_weighted(&mut scene, "Send", 16.0, Color::WHITE, t, None, TextWeight::Regular);
        let (h, _m, _ns) = tr.take_shape_stats();
        assert_eq!(h, 1, "second draw hits before bump");
        tr.bump_generation();
        tr.draw_text_weighted(&mut scene, "Send", 16.0, Color::WHITE, t, None, TextWeight::Regular);
        let (_h, m, _ns) = tr.take_shape_stats();
        assert_eq!(m, 1, "draw after bump must reshape");
    }

    /// Real font-family support: the generic families resolve to installed
    /// faces and shape to positive advances, a named installed family shapes,
    /// and two different families are distinct shape-cache entries (so a serif
    /// body render doesn't collide with the sans measurement).
    #[test]
    fn font_choice_families_resolve_and_shape() {
        let mut tr = TextRenderer::new();
        let fams = tr.system_families();
        assert!(!fams.is_empty(), "the system font db exposes some families");

        // Both generic families measure to a positive advance for real text.
        let (ws, _) =
            tr.measure_font("Reading", 20.0, None, TextWeight::Regular, false, &FontChoice::Serif);
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

    /// The embedded serif is registered into the font system and the generic
    /// [`FontChoice::Serif`] resolves to it **through the real shaping path** —
    /// not to a silent sans substitute. This is the guarantee the reading views
    /// (RSS body, ebook body, office documents) depend on: a serif request must
    /// yield genuine serif glyphs on any host, including the OS image and build
    /// container that ship no serif of their own.
    #[test]
    fn embedded_serif_resolves_through_the_shaping_path() {
        let mut tr = TextRenderer::new();

        // The face is present in the db under its canonical family name.
        assert!(
            tr.system_families().iter().any(|f| f == SERIF_FAMILY),
            "the embedded serif family {SERIF_FAMILY:?} must be registered; \
             families = {:?}",
            tr.system_families(),
        );

        // Shaping real text as Serif lands on the embedded face.
        let serif_fams = tr.resolved_run_families(
            "Reading fjord",
            20.0,
            TextWeight::Regular,
            false,
            &FontChoice::Serif,
        );
        assert!(
            serif_fams.iter().any(|f| f == SERIF_FAMILY),
            "FontChoice::Serif must shape with the embedded serif {SERIF_FAMILY:?}, \
             got {serif_fams:?}",
        );

        // And it is a *distinct* choice from sans — proving we didn't just make
        // everything serif. The sans run must NOT resolve to the serif face.
        let sans_fams = tr.resolved_run_families(
            "Reading fjord",
            20.0,
            TextWeight::Regular,
            false,
            &FontChoice::SansSerif,
        );
        assert!(
            !sans_fams.iter().any(|f| f == SERIF_FAMILY),
            "sans must not resolve to the serif face; sans = {sans_fams:?}",
        );
    }

    /// The bold and italic serif faces are registered too, so an emphasised or
    /// bold run inside serif body copy stays on the same embedded family rather
    /// than faux-styling a single weight. Guards that all four faces load.
    #[test]
    fn embedded_serif_bold_and_italic_stay_on_the_family() {
        let mut tr = TextRenderer::new();
        for (weight, italic, label) in [
            (TextWeight::Bold, false, "bold"),
            (TextWeight::Regular, true, "italic"),
            (TextWeight::Bold, true, "bold-italic"),
        ] {
            let fams =
                tr.resolved_run_families("Emphasis", 20.0, weight, italic, &FontChoice::Serif);
            assert!(
                fams.iter().any(|f| f == SERIF_FAMILY),
                "serif {label} run must stay on {SERIF_FAMILY:?}, got {fams:?}",
            );
        }
    }

    /// The canonical optical-centring helper places a single-line run so its
    /// optical centre (≈ mid cap-height, `top + size·(0.5 + OPTICAL_LIFT)`)
    /// lands on the box centre within a sub-pixel tolerance, and sits strictly
    /// higher than the naive `(h - size)/2` box-centre that read visibly low.
    #[test]
    fn centered_text_y_optically_centers_within_tolerance() {
        // Sweep a range of box heights and font sizes (button, badge, avatar,
        // list-row content all pass through this one helper).
        for (h, size) in [(40.0, 14.0f32), (16.0, 11.0), (28.0, 13.0), (58.0, 14.0), (40.0, 16.0)] {
            let y0 = 7.0;
            let top = centered_text_y(y0, h, size);
            let optical_center = top + size as f64 * (0.5 + OPTICAL_LIFT);
            let box_center = y0 + h / 2.0;
            assert!(
                (optical_center - box_center).abs() < 1e-6,
                "h={h} size={size}: optical centre {optical_center} must equal box centre {box_center}",
            );
            let naive = y0 + (h - size as f64) / 2.0;
            assert!(top < naive, "h={h} size={size}: optical top {top} must lift above naive {naive}");
        }
        // The lift grows with the font size — a larger run needs a larger lift.
        let small = (0.0 + (34.0 - 12.0) / 2.0) - centered_text_y(0.0, 34.0, 12.0);
        let large = (0.0 + (34.0 - 24.0) / 2.0) - centered_text_y(0.0, 34.0, 24.0);
        assert!(large > small, "lift scales with font size: {large} > {small}");
    }

    /// A two-line name-over-subtitle block (a list row) is optically centred as
    /// a unit: the midpoint of the two runs' optical centres lands on the box
    /// middle — level with a box-centred avatar / icon in the same row — while
    /// the advance between the two run tops is preserved exactly.
    #[test]
    fn centered_two_line_tops_centers_the_block() {
        let k = 0.5 + OPTICAL_LIFT;
        // Sweep list-row shapes: a 50 px results row (16/13), a 38 px roster row
        // (14/11), a tall 58 px settings row (14/11).
        for (h, st, sb, adv) in [(50.0, 16.0f32, 13.0f32, 20.8), (38.0, 14.0, 11.0, 18.0), (58.0, 14.0, 11.0, 22.0)] {
            let y0 = 100.0;
            let (yt, yb) = centered_two_line_tops(y0, h, st, sb, adv);
            let ct = yt + st as f64 * k; // top-line optical centre
            let cb = yb + sb as f64 * k; // bottom-line optical centre
            let block_center = (ct + cb) / 2.0;
            let box_center = y0 + h / 2.0;
            assert!(
                (block_center - box_center).abs() < 1e-6,
                "h={h}: block centre {block_center} must equal box centre {box_center}",
            );
            assert!((yb - yt - adv).abs() < 1e-9, "the run advance is preserved");
        }
    }

    /// The embedded monospace face is registered and cosmic-text's generic
    /// [`Family::Monospace`] resolves **to it** — not to a proportional host
    /// fallback. Proven two ways: the fontdb query for the generic monospace
    /// family returns a face whose family name is the embedded [`MONO_FAMILY`],
    /// and that face is genuinely fixed-width (a narrow `i` and a wide `W`
    /// advance identically) where the proportional sans face is not — so the
    /// silent named-but-absent → proportional substitution can't recur.
    #[test]
    fn monospace_resolves_to_embedded_fixed_width_face() {
        use cosmic_text::fontdb::{Family as DbFamily, Query};
        let mut tr = TextRenderer::new();

        // The embedded family is registered under its canonical name.
        assert!(
            tr.system_families().iter().any(|f| f == MONO_FAMILY),
            "the embedded monospace family is present in the font db"
        );

        // The generic monospace family resolves *to the embedded face*.
        let id = tr
            .font_system
            .db()
            .query(&Query { families: &[DbFamily::Monospace], ..Default::default() })
            .expect("generic monospace resolves to a face");
        let resolved = tr
            .font_system
            .db()
            .face(id)
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_default();
        assert_eq!(
            resolved, MONO_FAMILY,
            "generic monospace points at the embedded face, got {resolved:?}"
        );

        // Genuinely fixed-width: a narrow `i` and a wide `W` advance identically
        // in mono, but differ in the proportional sans face — so this proves the
        // mono face is not the silent proportional substitute.
        let (wi, _) =
            tr.measure_font("i", 20.0, None, TextWeight::Regular, false, &FontChoice::Monospace);
        let (ww, _) =
            tr.measure_font("W", 20.0, None, TextWeight::Regular, false, &FontChoice::Monospace);
        assert!((wi - ww).abs() < 0.5, "monospace advances are equal: i={wi} W={ww}");

        let (si, _) =
            tr.measure_font("i", 20.0, None, TextWeight::Regular, false, &FontChoice::SansSerif);
        let (sw, _) =
            tr.measure_font("W", 20.0, None, TextWeight::Regular, false, &FontChoice::SansSerif);
        assert!((sw - si).abs() > 1.0, "sans is proportional (control): i={si} W={sw}");

        // A column of equal-length mono runs lines up to the pixel, whatever the
        // glyphs — the property a diff / code grid depends on.
        let (row_a, _) = tr.measure_font(
            "iiiiiiii", 16.0, None, TextWeight::Regular, false, &FontChoice::Monospace,
        );
        let (row_b, _) = tr.measure_font(
            "WWWWWWWW", 16.0, None, TextWeight::Regular, false, &FontChoice::Monospace,
        );
        assert!((row_a - row_b).abs() < 0.5, "eight-char mono rows share a width: {row_a} vs {row_b}");
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
