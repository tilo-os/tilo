//! Avatar widget — a circular profile chip that renders either a
//! caller-supplied image clipped to a circle, or a coloured circle with
//! the user's initials when no image is available.
//!
//! Two construction paths:
//!
//! - [`Avatar::new`] — image variant. The supplied `peniko::Image` is
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
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Circular profile chip — see module docs.
pub struct Avatar {
    image: Option<peniko::Image>,
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
    pub fn new(image: peniko::Image, size: f32) -> Self {
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
    fn paint_image(&self, scene: &mut Scene, transform: Affine, img: &peniko::Image) {
        let centre = self.rect.center();
        let radius = self.rect.width().min(self.rect.height()) * 0.5;
        let clip = Circle::new(centre, radius);

        scene.push_layer(BlendMode::default(), 1.0, transform, &clip);

        let iw = img.width.max(1) as f64;
        let ih = img.height.max(1) as f64;
        let sx = self.rect.width() / iw;
        let sy = self.rect.height() / ih;
        let brush_transform =
            Affine::translate((self.rect.x0, self.rect.y0)) * Affine::scale_non_uniform(sx, sy);

        scene.fill(
            Fill::NonZero,
            transform,
            img,
            Some(brush_transform),
            &self.rect,
        );

        scene.pop_layer();
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
        let (tw, th) = text_renderer.measure(&self.initials, text_size, None);
        let tx = centre.x - tw as f64 / 2.0;
        let ty = centre.y - th as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            &self.initials,
            text_size,
            Color::WHITE,
            transform * Affine::translate((tx, ty)),
            None,
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
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, Format, Image as PenikoImage};
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
        let img = PenikoImage::new(blob, Format::Rgba8, 1, 1);

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
