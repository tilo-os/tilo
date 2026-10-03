//! Avatar widget — a circular profile chip that renders either a
//! caller-supplied image clipped to a circle, or a coloured circle with
//! the user's initials when no image is available.
//!
//! Initials-avatar colour is sampled deterministically from the theme's
//! accent ramp by hashing the initials ([`Avatar::background_color`]), so
//! the same person's chip is the same colour everywhere it's drawn,
//! without the OS shipping per-user theming.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Vec2};
use vello::peniko::{self, BlendMode, Color, Fill};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// The separator ring painted behind an avatar chip in an overlapping
/// cluster: `width` px of `color` around the chip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AvatarRing {
    pub color: Color,
    pub width: f64,
}

/// Circular profile chip — see module docs.
pub struct Avatar {
    image: Option<peniko::ImageData>,
    /// Uppercased, ≤ 2-char initials, normalised at construction so
    /// [`Avatar::display_initials`] is a cheap getter tests can pin.
    initials: String,
    size: f32,
    rect: Rect,
    /// Scene cache. Avatars have no hover/press/focus visuals, so the
    /// cache invalidates only when the rect size changes (`apply_layout`).
    cached: vello::Scene,
    dirty: bool,
    /// Rect the cache was emitted at — compared by size; pure
    /// translation handled by the append transform in `draw`.
    cache_rect: Rect,
}

impl Avatar {
    /// Construct an avatar that paints a circle-clipped image, sized to
    /// fill the `size × size` square before the circular clip is applied.
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

    /// Construct an avatar that paints a deterministically-coloured circle
    /// with the supplied initials, uppercased and truncated to 2 chars.
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

    /// The exact string `draw` hands to the text renderer for the
    /// initials variant; empty for the image variant.
    pub fn display_initials(&self) -> &str {
        &self.initials
    }

    /// Hashes the (uppercased, truncated) initials with `DefaultHasher`
    /// and picks that slot of the theme's presence palette: an avatar is a
    /// person, so it wears a presence colour. `pub` so tests can call it
    /// without rendering into a scene.
    pub fn background_color(&self, theme: &Theme) -> Color {
        let presence = &theme.tokens.colors.presence;
        let mut hasher = DefaultHasher::new();
        self.initials.hash(&mut hasher);
        presence.get(ramp_index(
            hasher.finish(),
            crate::tokens::colors::PRESENCE_LEN,
        ))
    }

    /// Circle-clip, fill the image scaled to the avatar square (so the
    /// circular crop catches the image's centre regardless of source
    /// aspect), pop the clip.
    fn paint_image(&self, scene: &mut Scene, transform: Affine, img: &peniko::ImageData) {
        let centre = self.rect.center();
        let radius = self.rect.width().min(self.rect.height()) * 0.5;
        let clip = Circle::new(centre, radius);

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);

        let iw = f64::from(img.width.max(1));
        let ih = f64::from(img.height.max(1));
        let sx = self.rect.width() / iw;
        let sy = self.rect.height() / ih;
        let brush_transform =
            Affine::translate((self.rect.x0, self.rect.y0)).pre_scale_non_uniform(sx, sy);

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

    /// Immediate-mode: paints directly into `scene` at `rect` with no
    /// layout node, caching, or widget instance — for callers that lay out
    /// their own rows (peer lists, mention chips, comment gutters).
    /// `initials` is normalised exactly like [`Avatar::from_initials`]; an
    /// empty `initials` paints just the circle.
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        initials: &str,
        color: Color,
        theme: &Theme,
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
        let style = initials_style(theme, px(rect.width().min(rect.height())) * 0.4);
        let (tw, th) = text_renderer.measure_style(&initials, None, &style);
        let tx = centre.x - f64::from(tw) / 2.0;
        let ty = centre.y - f64::from(th) / 2.0;
        text_renderer.draw_style(
            scene,
            &initials,
            Self::initial_ink(color),
            transform.pre_translate(Vec2::new(tx, ty)),
            None,
            &style,
        );
    }

    /// Identity colours are picked to tell people apart, not to sit under
    /// white text, and the light half of any palette is bright enough that
    /// a white initial is unreadable on it — so relative luminance decides:
    /// dark chips keep white, light ones take near-black ink. Uses sRGB
    /// relative luminance (Rec. 709) on the raw channels rather than the
    /// gamma-correct form, which costs three `powf` calls per glyph for a
    /// threshold shift of a couple of palette entries.
    pub fn initial_ink(fill: Color) -> Color {
        let c = fill.to_rgba8();
        let l = 0.2126 * f32::from(c.r) + 0.7152 * f32::from(c.g) + 0.0722 * f32::from(c.b);
        if l > 140.0 {
            Color::from_rgb8(0x14, 0x16, 0x1a)
        } else {
            Color::WHITE
        }
    }

    /// [`Avatar::draw_at`] preceded by a separator ring, for overlapping
    /// avatars in a cluster.
    pub fn draw_ringed_at(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        initials: &str,
        color: Color,
        ring: AvatarRing,
    ) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        Self::draw_ring(ctx.scene, ctx.transform, rect, ring);
        Self::draw_at(
            ctx.scene,
            ctx.transform,
            rect,
            initials,
            color,
            ctx.theme,
            ctx.text,
        );
    }

    /// Trailing "+N overflow" chip — same separator ring as
    /// [`Avatar::draw_ringed_at`], but the theme's neutral raised fill and a
    /// dim, non-truncated `"+{count}"` label, since it reads as "N more
    /// people" rather than another identity. `count == 0` paints nothing.
    pub fn draw_overflow_at(ctx: &mut PaintCtx<'_>, rect: Rect, count: usize, ring: AvatarRing) {
        if count == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let colors = &ctx.theme.tokens.colors;
        Self::draw_ring(ctx.scene, ctx.transform, rect, ring);
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.5;
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            colors.surfaces.raised,
            None,
            &Circle::new(centre, radius),
        );
        // "+N" can be 2–3 glyphs, so key the size a touch below the 0.4×
        // initials ratio to keep "+12" inside the chip.
        let label = format!("+{count}");
        let style = initials_style(ctx.theme, px(rect.width().min(rect.height())) * 0.36);
        let (tw, th) = ctx.text.measure_style(&label, None, &style);
        let tx = centre.x - f64::from(tw) / 2.0;
        let ty = centre.y - f64::from(th) / 2.0;
        ctx.text(&label, &style, colors.text_dim, Point::new(tx, ty), None);
    }

    /// A filled `ring.color` disc `ring.width` px larger than `rect`,
    /// painted before the chip fill so the ring shows through the gap
    /// between neighbouring chips.
    fn draw_ring(scene: &mut Scene, transform: Affine, rect: Rect, ring: AvatarRing) {
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.5;
        scene.fill(
            Fill::NonZero,
            transform,
            ring.color,
            None,
            &Circle::new(centre, radius + ring.width),
        );
    }

    /// First character of each of the first two whitespace-separated
    /// words, uppercased — `"Ada Lovelace"` → `"AL"`. Distinct from
    /// [`Avatar::from_initials`], which only truncates and never splits on
    /// words. Callers supply their own fallback for the empty-name case.
    pub fn initials_for_name(name: &str) -> String {
        name.split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .collect::<String>()
            .to_uppercase()
    }

    /// Same hash as [`Avatar::from_initials`], exposed standalone so
    /// immediate-mode callers of [`Avatar::draw_at`] can derive it without
    /// constructing an `Avatar`.
    pub fn color_for_initials(initials: &str, theme: &Theme) -> Color {
        // The normalised copy is read via `Hash::hash` below; clippy's
        // collection_is_never_read doesn't recognize hashing as a read.
        #[allow(
            clippy::collection_is_never_read,
            reason = "read by Hash::hash two lines down, which this lint doesn't track as a use"
        )]
        let initials: String = initials.to_uppercase().chars().take(2).collect();
        let presence = &theme.tokens.colors.presence;
        let mut hasher = DefaultHasher::new();
        initials.hash(&mut hasher);
        presence.get(ramp_index(
            hasher.finish(),
            crate::tokens::colors::PRESENCE_LEN,
        ))
    }

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
        // and a 64px header avatar both look balanced.
        let style = initials_style(theme, self.size * 0.4);
        let (tw, th) = text_renderer.measure_style(&self.initials, None, &style);
        let tx = centre.x - f64::from(tw) / 2.0;
        let ty = centre.y - f64::from(th) / 2.0;
        text_renderer.draw_style(
            scene,
            &self.initials,
            Self::initial_ink(bg),
            transform.pre_translate(Vec2::new(tx, ty)),
            None,
            &style,
        );
    }
}

/// Initials are sized by the disc they sit in, not by the reader's text
/// scale: the theme's emphasised label face at `px`, as content.
fn initials_style(theme: &Theme, px: f32) -> TextStyle {
    theme.text(TextRole::Label).strong().with_content_size(px)
}

/// Index into a colour ramp of length `len` from a 64-bit hash. `len` is
/// always a small, fixed ramp size, so the truncation the lint warns
/// about can't actually occur; `checked_rem` handles `len == 0` safely.
fn ramp_index(hash: u64, len: usize) -> usize {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the low bits of a 64-bit hash modulo a small ramp length always fit in usize"
    )]
    let hash = hash as usize;
    hash.checked_rem(len).unwrap_or(0)
}

/// Narrows a small, on-screen chip dimension to `f32` for text sizing.
/// Avatar chips are always a handful to a few hundred logical pixels,
/// nowhere near `f32::MAX`.
fn px(v: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "v is an avatar chip dimension, never approaching f32::MAX"
    )]
    let out = v as f32;
    out
}

impl Widget for Avatar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(self.size),
                height: Dimension::length(self.size),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Exact comparison is intentional: any width/height delta, however
        // small, must invalidate the render cache — an epsilon would let a
        // genuine sub-pixel size change go undetected.
        #[allow(
            clippy::float_cmp,
            reason = "cache-dirtying must catch any delta, not just deltas past an epsilon"
        )]
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
            out_scene.append(
                &self.cached,
                Some(out_transform.pre_translate(Vec2::new(dx, dy))),
            );
            return;
        }
        // paint_* helpers borrow `&self`, so move the cache out, mutate
        // via them, move it back; `Scene` is a `Vec<…>` underneath, so
        // the move is cheap.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        // paint_image / paint_initials anchor on `self.rect`, so this
        // emits at current absolute coords.
        if let Some(img) = self.image.clone() {
            self.paint_image(&mut cached, Affine::IDENTITY, &img);
        } else {
            self.paint_initials(&mut cached, Affine::IDENTITY, text_renderer, theme);
        }
        out_scene.append(&cached, Some(out_transform));
        self.cached = cached;
        self.cache_rect = self.rect;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    /// `Rect::ZERO` sentinel until the first `apply_layout`; stable
    /// after that since avatars are fixed-size leaves.
    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Avatar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The image variant carries empty initials, so collapse that to no name.
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

    fn paint(avatar: &mut Avatar) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        avatar.rect = Rect::new(0.0, 0.0, f64::from(avatar.size), f64::from(avatar.size));
        avatar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    #[test]
    fn avatar_widget_paints_initials_or_image_with_deterministic_colour() {
        fn avatar_with_initials_paints_initials_text() {
            let mut a = Avatar::from_initials("QD", 40.0);
            assert_eq!(
                a.display_initials(),
                "QD",
                "from_initials must keep already-uppercase 2-char input verbatim",
            );

            let lower = Avatar::from_initials("qd", 40.0);
            assert_eq!(
                lower.display_initials(),
                "QD",
                "must uppercase lowercase input"
            );
            let long = Avatar::from_initials("Tilo", 40.0);
            assert_eq!(
                long.display_initials(),
                "TI",
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

        fn avatar_with_initials_background_is_deterministic_for_same_initials() {
            let theme = Theme::dark();
            let a = Avatar::from_initials("QD", 40.0);
            let b = Avatar::from_initials("QD", 40.0);
            assert_eq!(
                a.background_color(&theme),
                b.background_color(&theme),
                "background_color must be deterministic for the same initials",
            );

            // The modulo can collide on any specific pair, so scan a small
            // alphabet and require at least one difference.
            let candidates = ["AA", "BB", "CC", "DD", "EE", "FF", "GG"];
            let qd_color = a.background_color(&theme);
            let any_different = candidates
                .iter()
                .any(|s| Avatar::from_initials(s, 40.0).background_color(&theme) != qd_color);
            assert!(
                any_different,
                "background_color collapsed all initials to one ramp stop — \
             hash distribution is broken or the modulo is wrong",
            );
        }

        fn initials_for_name_takes_first_char_of_first_two_words() {
            assert_eq!(Avatar::initials_for_name("Ada Lovelace"), "AL");
            assert_eq!(Avatar::initials_for_name("ada lovelace"), "AL");
            assert_eq!(Avatar::initials_for_name("mononym"), "M");
            assert_eq!(
                Avatar::initials_for_name("Grace Brewster Murray Hopper"),
                "GB"
            );
            assert_eq!(Avatar::initials_for_name("   spaced   out  "), "SO");
            assert_eq!(
                Avatar::initials_for_name(""),
                "",
                "empty name yields empty — caller supplies its own fallback glyph"
            );
        }

        /// Identity palettes span both halves of the value range, and a white
        /// letter on the light half is the "illegible label on a coloured
        /// fill" the HIG names.
        fn a_monogram_takes_readable_ink_for_its_fill() {
            let white = Color::WHITE;
            let black = Color::from_rgb8(0x14, 0x16, 0x1a);
            // Dark identity colours keep the white monogram.
            for dark in [
                Color::from_rgb8(0x3b, 0x82, 0xf6), // blue
                Color::from_rgb8(0xa8, 0x55, 0xf7), // purple
                Color::from_rgb8(0x00, 0x00, 0x00),
            ] {
                assert_eq!(
                    Avatar::initial_ink(dark),
                    white,
                    "a dark chip keeps white initials: {dark:?}"
                );
            }
            // Light ones take the dark ink instead of vanishing.
            for light in [
                Color::from_rgb8(0x6e, 0xe7, 0xb7), // mint
                Color::from_rgb8(0xfd, 0xe0, 0x47), // yellow
                Color::WHITE,
            ] {
                assert_eq!(
                    Avatar::initial_ink(light),
                    black,
                    "a light chip must not keep white initials: {light:?}"
                );
            }
        }

        /// The draw stream must contain `BEGIN_CLIP` → `IMAGE` → `END_CLIP` in
        /// that order; losing the clip would render the image as a square. A 1×1
        /// transparent image keeps the test independent of any on-disk
        /// fixture — only tag order is inspected, not pixel content.
        fn avatar_with_image_paints_circle_clipped_image() {
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

        avatar_with_initials_paints_initials_text();
        accessibility_reports_image_role_and_initials_name();
        avatar_with_initials_background_is_deterministic_for_same_initials();
        initials_for_name_takes_first_char_of_first_two_words();
        a_monogram_takes_readable_ink_for_its_fill();
        avatar_with_image_paints_circle_clipped_image();
    }

    #[test]
    fn immediate_avatar_painters_draw_chips_rings_and_overflow() {
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

        fn draw_at_large_text_scales_once_inside_scaled_geometry() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            Avatar::draw_at(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 80.0, 80.0),
                "QD",
                Avatar::color_for_initials("QD", &theme),
                &theme,
                &mut tr,
            );
            let run = scene
                .encoding()
                .resources
                .glyph_runs
                .first()
                .expect("monogram glyph run");
            assert_eq!(run.font_size, 32.0, "80px geometry keeps the 0.4 ratio");
        }

        fn draw_at_keeps_initials_when_motion_is_reduced() {
            let theme = Theme::dark().with_reduce_motion(true);
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            Avatar::draw_at(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 72.0, 72.0),
                "AL",
                Avatar::color_for_initials("AL", &theme),
                &theme,
                &mut tr,
            );
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "reduced motion must freeze animation only, never omit static identity text"
            );
        }

        fn draw_at_color_matches_widget_background() {
            let theme = Theme::dark();
            let a = Avatar::from_initials("QD", 40.0);
            assert_eq!(
                Avatar::color_for_initials("qd", &theme),
                a.background_color(&theme),
                "immediate-mode colour must equal the widget's background colour",
            );
            assert_eq!(
                Avatar::color_for_initials("AB", &theme),
                Avatar::color_for_initials("ab", &theme),
            );
        }

        fn draw_ringed_at_paints_ring_then_chip() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            Avatar::draw_ringed_at(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 40.0, 40.0),
                "qd",
                Avatar::color_for_initials("qd", &theme),
                AvatarRing {
                    color: Color::from_rgb8(0, 0, 0),
                    width: 2.0,
                },
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

        fn draw_overflow_at_paints_chip_and_label() {
            let theme = Theme::dark();
            let colors = &theme.tokens.colors;
            let mut tr = TextRenderer::new();

            let mut scene = Scene::new();
            Avatar::draw_overflow_at(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 40.0, 40.0),
                5,
                AvatarRing {
                    color: colors.surfaces.overlay,
                    width: 2.0,
                },
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
                &mut PaintCtx::new(&mut empty, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 40.0, 40.0),
                0,
                AvatarRing {
                    color: colors.surfaces.overlay,
                    width: 2.0,
                },
            );
            assert_eq!(
                empty.encoding().draw_tags.len(),
                0,
                "draw_overflow_at with count 0 must paint nothing",
            );
        }

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

        draw_at_paints_circle_and_initials();
        draw_at_large_text_scales_once_inside_scaled_geometry();
        draw_at_keeps_initials_when_motion_is_reduced();
        draw_at_color_matches_widget_background();
        draw_ringed_at_paints_ring_then_chip();
        draw_overflow_at_paints_chip_and_label();
        draw_at_zero_rect_is_noop();
    }
}
