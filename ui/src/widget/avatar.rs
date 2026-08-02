//! Avatar widget — a circular profile chip that renders either a
//! caller-supplied image clipped to a circle, or a coloured circle with
//! the user's initials when no image is available.
//!
//! Two construction paths:
//!
//! - [`Avatar::new`] — image variant. The supplied `peniko::ImageData` is
//!   painted inside a circular clip (`push_layer` with a circle path,
//!   then `fill` the image, then `pop_layer`). This is the "real photo"
//!   path used when the user has an avatar uploaded.
//! - [`Avatar::from_initials`] — initials variant. The string is
//!   uppercased and truncated to the first 2 chars (see
//!   [`Avatar::display_initials`]). A filled circle is drawn behind the
//!   text; its colour is sampled deterministically from the theme's
//!   11-stop accent ramp using [`Avatar::background_color`], so the
//!   "QD" avatar is always the same indigo whether painted in the
//!   peer list, a mention chip, or the settings header. Initials are
//!   rendered in `Color::WHITE` and centred inside the circle.
//!
//! The colour-from-hash trick means a roster of users gets a stable,
//! visually-distinct palette without the OS shipping per-user theming.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{self, BlendMode, Color, Fill};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::Widget;

/// Circular profile chip — see module docs.
pub struct Avatar {
    image: Option<peniko::ImageData>,
    /// Uppercased, ≤ 2-char initials. Always pre-normalised by the
    /// constructor so [`Avatar::display_initials`] is a cheap getter
    /// the tests can pin without re-running the truncation logic.
    initials: String,
    size: f32,
    rect: Rect,
    /// Scene cache. Avatars are stateless after construction (no
    /// hover / press / focus visuals), so the cache invalidates
    /// only when the rect moves (apply_layout). Once a tree settles
    /// in steady state, every Avatar contributes one `Scene::append`
    /// per frame instead of repeating the circle / image fill +
    /// glyph emission.
    cached: vello::Scene,
    dirty: bool,
    /// Rect the cache was emitted at — compared by size; pure
    /// translation handled by the append transform in `draw`.
    cache_rect: Rect,
}

impl Avatar {
    /// Construct an avatar that paints a circle-clipped image. The
    /// `image` is sized to fit the avatar's `size × size` square; the
    /// circular clip happens in [`Self::paint_image`].
    pub fn new(image: peniko::ImageData, size: f32) -> Self {
        Self {
            image: Some(image),
            initials: String::new(),
            size,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
        }
    }

    /// Construct an avatar that paints a deterministically-coloured
    /// circle with the supplied initials. The string is uppercased and
    /// truncated to the first 2 chars right here at construction time
    /// so callers can pass any length and the test contract for
    /// [`Self::display_initials`] is stable.
    pub fn from_initials(initials: &str, size: f32) -> Self {
        let initials: String = initials.to_uppercase().chars().take(2).collect();
        Self {
            image: None,
            initials,
            size,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
        }
    }

    /// The exact string that `draw` will hand to the text renderer for
    /// the initials variant. Returns the empty string for the image
    /// variant (no initials are painted in that case).
    ///
    /// Pinning this accessor in tests means a future refactor that
    /// reroutes truncation through `draw` (and so changes when the
    /// uppercase / 2-char cap is applied) breaks here loudly rather
    /// than silently producing 3-letter avatars at runtime.
    pub fn display_initials(&self) -> &str {
        &self.initials
    }

    /// Deterministic background colour for the initials variant. Hashes
    /// the (already-uppercased, already-truncated) initials with
    /// `DefaultHasher` and indexes into the theme's 11-stop accent
    /// ramp. Same string in → same colour out, across processes and
    /// across runs — that's the property the test pins.
    ///
    /// Exposed as `pub` (rather than computed in-line inside `draw`)
    /// because the test for "two avatars with the same initials get
    /// the same colour" needs to call it without rendering into a
    /// scene.
    pub fn background_color(&self, theme: &Theme) -> Color {
        let ramp = &theme.tokens.colors.accent;
        let mut hasher = DefaultHasher::new();
        self.initials.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % ramp.len();
        ramp[idx]
    }

    /// Paint the image variant: circle-clip → fill the image → pop the
    /// clip. The brush_transform is set so the image's `(0, 0)` corner
    /// lands on the avatar's top-left and the image scales to exactly
    /// fill the avatar square (so the circular crop catches the centre
    /// of the image regardless of its source aspect).
    fn paint_image(&self, scene: &mut Scene, transform: Affine, img: &peniko::ImageData) {
        let centre = self.rect.center();
        let radius = self.rect.width().min(self.rect.height()) * 0.5;
        let clip = Circle::new(centre, radius);

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);

        let iw = img.width.max(1) as f64;
        let ih = img.height.max(1) as f64;
        let sx = self.rect.width() / iw;
        let sy = self.rect.height() / ih;
        let brush_transform =
            Affine::translate((self.rect.x0, self.rect.y0)) * Affine::scale_non_uniform(sx, sy);

        let image_brush = peniko::ImageBrush::new(img.clone());
        scene.fill(
            Fill::NonZero,
            transform,
            &image_brush,
            Some(brush_transform),
            &self.rect,
        );

        scene.pop_layer();
    }

    /// Immediate-mode initials avatar — paint a coloured circle with
    /// centred initials directly into `scene` at `rect`, with no layout
    /// node, caching, or widget instance.
    ///
    /// This is the helper a list/roster reaches for when it lays out its
    /// own rows and just needs a chip painted at a known rect (peer lists,
    /// mention chips, comment gutters). `initials` is uppercased and
    /// clamped to two chars exactly like [`Avatar::from_initials`]; `color`
    /// is the circle fill (callers typically pass a per-user accent from
    /// the theme ramp, e.g. via a hash). Initials render in `Color::WHITE`.
    /// An empty `initials` paints just the circle.
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        initials: &str,
        color: Color,
        _theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.5;
        let circle = Circle::new(centre, radius);
        scene.fill(Fill::NonZero, transform, color, None, &circle);

        let initials: String = initials.to_uppercase().chars().take(2).collect();
        if initials.is_empty() {
            return;
        }
        // Same 0.4× ratio the widget uses, but keyed off the rect rather
        // than a stored `size` field so callers don't have to thread one.
        // Semibold so the monogram reads crisply on a small saturated chip —
        // the weight the roster apps hand-rolled before adopting this helper.
        let text_size = (rect.width().min(rect.height()) as f32) * 0.4;
        let (tw, th) = text_renderer.measure_weighted(&initials, text_size, None, TextWeight::Semibold);
        let tx = centre.x - tw as f64 / 2.0;
        let ty = centre.y - th as f64 / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &initials,
            text_size,
            Color::WHITE,
            transform * Affine::translate((tx, ty)),
            None,
            TextWeight::Semibold,
        );
    }

    /// Immediate-mode avatar with a separator ring — [`Avatar::draw_at`]
    /// preceded by a filled `ring_color` disc `ring_width` px larger than the
    /// chip. Overlapping avatars in a cluster (the OS collaborator stack, an
    /// app roster row) paint this so each chip stays distinct against its
    /// neighbour and the surface behind: the visible annulus between the chip
    /// edge and the ring edge is the separator. The fill + initials render
    /// inside `rect` exactly as [`Avatar::draw_at`] (white, semibold, 0.4×).
    pub fn draw_ringed_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        initials: &str,
        color: Color,
        ring_color: Color,
        ring_width: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        Self::draw_ring(scene, transform, rect, ring_color, ring_width);
        Self::draw_at(scene, transform, rect, initials, color, theme, text_renderer);
    }

    /// Trailing "+N overflow" chip of an avatar cluster — an avatar-slot-shaped
    /// chip (with the same separator ring as [`Avatar::draw_ringed_at`]) whose
    /// label is the `count` of peers beyond the ones shown. It reads as "N more
    /// people", not another identity, so it takes a neutral `fill` + dim
    /// `label_color` and paints a non-truncated `"+{count}"` label rather than
    /// a per-identity accent circle with two white initials. `count == 0`
    /// paints nothing — an empty cluster has no overflow.
    pub fn draw_overflow_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        count: usize,
        fill: Color,
        ring_color: Color,
        ring_width: f64,
        label_color: Color,
        text_renderer: &mut TextRenderer,
    ) {
        if count == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        Self::draw_ring(scene, transform, rect, ring_color, ring_width);
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.5;
        scene.fill(
            Fill::NonZero,
            transform,
            fill,
            None,
            &Circle::new(centre, radius),
        );
        // "+N" can be 2–3 glyphs, so key the size a touch below the 0.4×
        // initials ratio to keep "+12" inside the chip.
        let label = format!("+{count}");
        let text_size = (rect.width().min(rect.height()) as f32) * 0.36;
        let (tw, th) =
            text_renderer.measure_weighted(&label, text_size, None, TextWeight::Semibold);
        let tx = centre.x - tw as f64 / 2.0;
        let ty = centre.y - th as f64 / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &label,
            text_size,
            label_color,
            transform * Affine::translate((tx, ty)),
            None,
            TextWeight::Semibold,
        );
    }

    /// Shared separator-ring painter for the cluster helpers: a filled
    /// `ring_color` disc `ring_width` px larger than the chip, centred on
    /// `rect`. Painted *before* the chip fill so the annulus that peeks out
    /// between neighbouring chips is the ring colour.
    fn draw_ring(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        ring_color: Color,
        ring_width: f64,
    ) {
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.5;
        scene.fill(
            Fill::NonZero,
            transform,
            ring_color,
            None,
            &Circle::new(centre, radius + ring_width),
        );
    }

    /// Word-initials for a display name: the first character of each of the
    /// first two whitespace-separated words, uppercased. `"Ada Lovelace"` →
    /// `"AL"`, `"mononym"` → `"M"`, `""` → `""`. This is the name→monogram
    /// step every roster app does before handing the (≤ 2-char) result to
    /// [`Avatar::draw_at`] / [`Avatar::from_initials`], which only truncate
    /// and never split on words — so passing a full name straight to them
    /// would yield the first two *letters* (`"Ad"`) rather than the
    /// per-word monogram (`"AL"`). Callers supply their own fallback glyph
    /// for the empty case (a nameless peer), which is app-specific.
    pub fn initials_for_name(name: &str) -> String {
        name.split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .collect::<String>()
            .to_uppercase()
    }

    /// Deterministic accent colour for `initials` against `theme`'s accent
    /// ramp — the same hash [`Avatar::from_initials`] uses, exposed as an
    /// associated function so immediate-mode callers of [`Avatar::draw_at`]
    /// can derive a stable per-identity colour without constructing an
    /// `Avatar`.
    pub fn color_for_initials(initials: &str, theme: &Theme) -> Color {
        let initials: String = initials.to_uppercase().chars().take(2).collect();
        let ramp = &theme.tokens.colors.accent;
        let mut hasher = DefaultHasher::new();
        initials.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % ramp.len();
        ramp[idx]
    }

    /// Paint the initials variant: filled circle in the deterministic
    /// accent colour, then the (already-normalised) initials in
    /// `Color::WHITE`, centred.
    fn paint_initials(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let centre = self.rect.center();
        let radius = self.rect.width().min(self.rect.height()) * 0.5;
        let circle = Circle::new(centre, radius);
        let bg = self.background_color(theme);
        scene.fill(Fill::NonZero, transform, bg, None, &circle);

        if self.initials.is_empty() {
            return;
        }

        // Text size is proportional to the avatar size so a 24px chip
        // and a 64px header avatar both look balanced. 0.4× matches the
        // ratio used in the concept mocks for chat avatars.
        let text_size = self.size * 0.4;
        let (tw, th) = text_renderer.measure_weighted(&self.initials, text_size, None, TextWeight::Semibold);
        let tx = centre.x - tw as f64 / 2.0;
        let ty = centre.y - th as f64 / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &self.initials,
            text_size,
            Color::WHITE,
            transform * Affine::translate((tx, ty)),
            None,
            TextWeight::Semibold,
        );
    }
}

impl Widget for Avatar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.size),
                    height: Dimension::length(self.size),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            // Pure translation since the cache was emitted.
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            out_scene.append(&self.cached, Some(out_transform * Affine::translate((dx, dy))));
            return;
        }
        // mem::take dance — paint_* helpers borrow `&self`, so
        // we move the cache out, mutate via them, move it back.
        // `Scene` is a `Vec<…>` underneath; the move is cheap.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        // Emit at current absolute coords — paint_image /
        // paint_initials anchor themselves on `self.rect`, so the
        // cache holds primitives at the current position.
        if let Some(img) = self.image.clone() {
            self.paint_image(&mut cached, Affine::IDENTITY, &img);
        } else {
            self.paint_initials(&mut cached, Affine::IDENTITY, text_renderer, theme);
        }
        out_scene.append(&cached, Some(out_transform));
        self.cached = cached;
        self.cache_rect = self.rect;
    }

    fn needs_redraw(&self) -> bool { self.dirty }
    fn mark_drawn(&mut self) { self.dirty = false; }
    /// `Rect::ZERO` sentinel until the first `apply_layout`; stable
    /// after that since avatars are fixed-size leaves.
    fn is_layout_dirty(&self) -> bool { self.cache_rect == Rect::ZERO }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str {
        "Avatar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // An avatar is a non-focusable image. The initials identify the person;
        // the image variant carries empty initials, so collapse that to no name
        // (a caller-supplied alt would supply one in a future enhancement).
        let initials = self.display_initials();
        let name = (!initials.is_empty()).then(|| initials.to_string());
        Some(AccessNode::new(Role::Image).with_optional_name(name))
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
    use vello_encoding::DrawTag;

    /// Helper: paint `avatar` into a fresh scene at a known rect and
    /// return the scene so the test can inspect its encoding. Mirrors
    /// the helper shape used in `badge::tests::color_fill_count` so
    /// future readers can match the two side-by-side.
    fn paint(avatar: &mut Avatar) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        avatar.rect = Rect::new(0.0, 0.0, avatar.size as f64, avatar.size as f64);
        avatar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    /// `from_initials` uppercases the input and clamps to the first two
    /// chars, so `"QD"` round-trips, `"qd"` lifts to `"QD"`, and
    /// `"Quickdesktop"` truncates to `"QU"`. Asserts via the
    /// `display_initials()` accessor — this is the contract the draw
    /// path relies on, so pinning the accessor pins the rendered text.
    ///
    /// Also confirms via the Vello encoding that the draw pass emits
    /// both a COLOR fill (the background circle) *and* a glyph run
    /// (the initials text). A regression that drops the background or
    /// the text would flip exactly one of those bits.
    #[test]
    fn avatar_with_initials_paints_initials_text() {
        let mut a = Avatar::from_initials("QD", 40.0);
        assert_eq!(
            a.display_initials(),
            "QD",
            "from_initials must keep already-uppercase 2-char input verbatim",
        );

        // Round-trip the casing + truncation rules so a future tweak to
        // the constructor (e.g. moving the uppercase into `draw`) trips
        // this test loudly.
        let lower = Avatar::from_initials("qd", 40.0);
        assert_eq!(lower.display_initials(), "QD", "must uppercase lowercase input");
        let long = Avatar::from_initials("Quickdesktop", 40.0);
        assert_eq!(
            long.display_initials(),
            "QU",
            "must truncate to first 2 chars after uppercasing",
        );

        let scene = paint(&mut a);
        let enc = scene.encoding();
        let color_fills = enc
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            color_fills >= 1,
            "Avatar::from_initials must emit at least one COLOR fill (the \
             background circle); got {color_fills}",
        );
        assert!(
            !enc.resources.glyph_runs.is_empty(),
            "Avatar::from_initials must push a glyph run for the initials text \
             into the scene; resources.glyph_runs is empty",
        );
    }

    /// The initials variant exposes the `Image` role with the (uppercased,
    /// truncated) initials as its accessible name; the image variant has empty
    /// initials and so reports no name.
    #[test]
    fn accessibility_reports_image_role_and_initials_name() {
        use crate::a11y::Role;
        let a = Avatar::from_initials("qd", 40.0);
        let node = Widget::accessibility(&a).expect("avatar is not presentational");
        assert_eq!(node.role, Role::Image);
        assert_eq!(node.name.as_deref(), Some("QD"));
        assert!(!node.state.focusable, "an avatar is not focusable");

        // Image variant: no initials → no accessible name.
        let bytes: Vec<u8> = vec![0u8; 4];
        let blob = Blob::new(Arc::new(bytes));
        let img = PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        };
        let photo = Avatar::new(img, 40.0);
        assert_eq!(Widget::accessibility(&photo).unwrap().name, None);
    }

    /// Two avatars constructed with the same initials must resolve to
    /// the same background colour. The `background_color` helper is
    /// the documented hook for this — a regression that swaps in a
    /// non-deterministic hash (e.g. `RandomState`-based) would flip
    /// this test red even though the visual difference is subtle.
    #[test]
    fn avatar_with_initials_background_is_deterministic_for_same_initials() {
        let theme = Theme::dark();
        let a = Avatar::from_initials("QD", 40.0);
        let b = Avatar::from_initials("QD", 40.0);
        assert_eq!(
            a.background_color(&theme),
            b.background_color(&theme),
            "background_color must be deterministic for the same initials",
        );

        // Sanity: a different string maps to a different ramp slot at
        // least *some* of the time — without this we couldn't tell a
        // deterministic-but-constant function from a real hash. We
        // can't assert inequality for any specific pair (the modulo
        // collapses 256-bit hashes into 11 buckets so collisions are
        // possible) but we can scan a small alphabet and require at
        // least one non-"QD" sample to differ.
        let candidates = ["AA", "BB", "CC", "DD", "EE", "FF", "GG"];
        let qd_color = a.background_color(&theme);
        let any_different = candidates.iter().any(|s| {
            Avatar::from_initials(s, 40.0).background_color(&theme) != qd_color
        });
        assert!(
            any_different,
            "background_color collapsed all initials to one ramp stop — \
             hash distribution is broken or the modulo is wrong",
        );
    }

    /// `Avatar::draw_at` paints a circle + initials glyph run into a
    /// scene immediately, with no widget instance. The encoding must show
    /// at least one COLOR fill (circle) and a glyph run (initials).
    #[test]
    fn draw_at_paints_circle_and_initials() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Avatar::draw_at(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 40.0, 40.0),
            "qd",
            Avatar::color_for_initials("qd", &theme),
            &theme,
            &mut tr,
        );
        let enc = scene.encoding();
        let color_fills = enc
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(color_fills >= 1, "draw_at must fill the background circle");
        assert!(
            !enc.resources.glyph_runs.is_empty(),
            "draw_at must paint the initials glyph run",
        );
    }

    /// `color_for_initials` is deterministic and agrees with a constructed
    /// `Avatar`'s `background_color` for the same (normalised) initials —
    /// so an immediate-mode chip and a widget chip for the same user match.
    #[test]
    fn draw_at_color_matches_widget_background() {
        let theme = Theme::dark();
        let a = Avatar::from_initials("QD", 40.0);
        assert_eq!(
            Avatar::color_for_initials("qd", &theme),
            a.background_color(&theme),
            "immediate-mode colour must equal the widget's background colour",
        );
        // Deterministic across calls.
        assert_eq!(
            Avatar::color_for_initials("AB", &theme),
            Avatar::color_for_initials("ab", &theme),
        );
    }

    /// `initials_for_name` is the name→monogram step: first char of each of
    /// the first two words, uppercased. This is what separates a per-word
    /// monogram (`"AL"`) from `from_initials`'s first-two-letters truncation
    /// (`"AD"`), so a roster app gets `"AL"` for "Ada Lovelace" only by
    /// routing through here first.
    #[test]
    fn initials_for_name_takes_first_char_of_first_two_words() {
        assert_eq!(Avatar::initials_for_name("Ada Lovelace"), "AL");
        assert_eq!(Avatar::initials_for_name("ada lovelace"), "AL");
        assert_eq!(Avatar::initials_for_name("mononym"), "M");
        assert_eq!(Avatar::initials_for_name("Grace Brewster Murray Hopper"), "GB");
        assert_eq!(Avatar::initials_for_name("   spaced   out  "), "SO");
        assert_eq!(Avatar::initials_for_name(""), "", "empty name yields empty — caller supplies its own fallback glyph");
    }

    /// `draw_ringed_at` paints the separator ring *and* the chip: at least
    /// two COLOR fills (ring disc + chip circle) plus the initials glyph run.
    /// A regression that dropped the ring would fall to one fill; one that
    /// dropped the chip would lose the glyph run.
    #[test]
    fn draw_ringed_at_paints_ring_then_chip() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Avatar::draw_ringed_at(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 40.0, 40.0),
            "qd",
            Avatar::color_for_initials("qd", &theme),
            Color::from_rgb8(0, 0, 0),
            2.0,
            &theme,
            &mut tr,
        );
        let enc = scene.encoding();
        let color_fills = enc
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            color_fills >= 2,
            "draw_ringed_at must fill the ring disc AND the chip circle; got \
             {color_fills} COLOR fills",
        );
        assert!(
            !enc.resources.glyph_runs.is_empty(),
            "draw_ringed_at must still paint the initials glyph run",
        );
    }

    /// `draw_overflow_at` paints the neutral "+N" chip: ring + chip fill (two
    /// COLOR fills) plus the `"+{count}"` glyph run. `count == 0` is a no-op —
    /// an empty cluster shows no overflow.
    #[test]
    fn draw_overflow_at_paints_chip_and_label() {
        let theme = Theme::dark();
        let colors = &theme.tokens.colors;
        let mut tr = TextRenderer::new();

        let mut scene = Scene::new();
        Avatar::draw_overflow_at(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 40.0, 40.0),
            5,
            colors.surfaces.raised,
            colors.surfaces.overlay,
            2.0,
            colors.text_dim,
            &mut tr,
        );
        let enc = scene.encoding();
        let color_fills = enc
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            color_fills >= 2,
            "draw_overflow_at must fill the ring AND the chip; got {color_fills}",
        );
        assert!(
            !enc.resources.glyph_runs.is_empty(),
            "draw_overflow_at must paint the \"+N\" label glyph run",
        );

        // count == 0 → nothing at all.
        let mut empty = Scene::new();
        Avatar::draw_overflow_at(
            &mut empty,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 40.0, 40.0),
            0,
            colors.surfaces.raised,
            colors.surfaces.overlay,
            2.0,
            colors.text_dim,
            &mut tr,
        );
        assert_eq!(
            empty.encoding().draw_tags.len(),
            0,
            "draw_overflow_at with count 0 must paint nothing",
        );
    }

    /// A zero-size rect is a no-op for `draw_at` (no panic, no draw).
    #[test]
    fn draw_at_zero_rect_is_noop() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Avatar::draw_at(
            &mut scene,
            Affine::IDENTITY,
            Rect::ZERO,
            "QD",
            Color::WHITE,
            &theme,
            &mut tr,
        );
        assert_eq!(scene.encoding().draw_tags.len(), 0);
    }

    /// Image variant: the draw stream must contain a BEGIN_CLIP →
    /// IMAGE → END_CLIP triple in that order. The clip is the
    /// circular crop, the image is the actual pixel blit. A refactor
    /// that ever forgets the clip would render the image as a square,
    /// breaking the avatar's defining visual.
    ///
    /// We construct a 1×1 transparent image so the test is independent
    /// of any on-disk fixture. The encoding inspection doesn't care
    /// about pixel content — only that the tags landed in the right
    /// order.
    #[test]
    fn avatar_with_image_paints_circle_clipped_image() {
        // 1×1 transparent RGBA blob. peniko::Blob wraps an Arc and the
        // image references those bytes without copying.
        let bytes: Vec<u8> = vec![0u8; 4];
        let blob = Blob::new(Arc::new(bytes));
        let img = PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        };

        let mut a = Avatar::new(img, 40.0);
        let scene = paint(&mut a);
        let tags: Vec<DrawTag> = scene.encoding().draw_tags.clone();

        let begin = tags
            .iter()
            .position(|t| *t == DrawTag::BEGIN_CLIP)
            .expect("Avatar::new must push a clip before the image draw");
        let image = tags
            .iter()
            .position(|t| *t == DrawTag::IMAGE)
            .expect("Avatar::new must emit an IMAGE draw tag");
        let end = tags
            .iter()
            .rposition(|t| *t == DrawTag::END_CLIP)
            .expect("Avatar::new must pop the clip after the image draw");

        // `DrawTag` doesn't implement Debug, so render the raw u32s for
        // the diagnostic if the ordering ever drifts.
        let raw: Vec<u32> = tags.iter().map(|t| t.0).collect();
        assert!(
            begin < image && image < end,
            "draw tags must be ordered BEGIN_CLIP < IMAGE < END_CLIP; got \
             positions begin={begin}, image={image}, end={end} in {raw:?}",
        );
    }
}
