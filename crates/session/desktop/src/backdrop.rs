//! Desktop backdrop widget — paints the desktop background behind all
//! shell content: the wallpaper the host supplies, cover-fitted to each
//! display on its own, or, with no wallpaper, a diagonal gradient across
//! the theme's ground surface (`ground_stops`) under a tiled 64×64
//! procedural noise overlay that breaks banding on the gradient.
//!
//! Structurally a thin `Container`-style wrapper around one optional
//! child, forwarding sync/layout/events like [`ui::widget::Container`]
//! but with no padding or chrome — just the picture beneath it.

use std::sync::Arc;

use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::TaffyTreeExt;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{
    Blob, Brush, Color, ColorStop, Extend, Fill, Gradient, ImageAlphaType, ImageBrush,
    ImageBrush as Image, ImageData, ImageFormat as Format,
};
use winit::event::WindowEvent;

/// High enough that the desktop reads as dark rather than blue, short
/// of 1.0 so each theme still tints its own ground.
const GROUND_NEUTRALITY: f32 = 0.72;

/// Enough to read as depth across a whole screen; gentle enough that a
/// light theme's ground stays light rather than ramping into grey.
const GROUND_FALLOFF: f32 = 0.82;

/// Edge length (in pixels) of the tileable noise texture.
const NOISE_SIZE: u32 = 64;

/// The concept's `opacity-20 mix-blend-overlay` works out to roughly
/// this value against a near-black gradient. Raise only if banding is
/// still visible — the dots must not be individually distinguishable.
const NOISE_ALPHA: f32 = 0.08;

/// Fixed so every session renders the same grain, keeping the backdrop
/// byte-identical across headless render tests.
const NOISE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Pixel count of the tileable [`NOISE_SIZE`]×[`NOISE_SIZE`] noise
/// texture. A `const` (rather than computed in the function body) so
/// the multiplication is evaluated at compile time — `NOISE_SIZE` never
/// changes, so there is nothing for `arithmetic_side_effects` to guard
/// against here.
const NOISE_PIXEL_COUNT: usize = (NOISE_SIZE as usize) * (NOISE_SIZE as usize);

/// [`NOISE_PIXEL_COUNT`] in RGBA8 bytes (4 bytes/pixel), likewise
/// compile-time constant.
const NOISE_BYTE_COUNT: usize = NOISE_PIXEL_COUNT * 4;

/// Each pixel is `(0xFF, 0xFF, 0xFF, alpha)`, `alpha ∈ [0, 40]` — kept
/// low before [`NOISE_ALPHA`] dampens it further.
fn generate_noise_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(NOISE_BYTE_COUNT);

    // xorshift64 state — must be non-zero.
    let mut state: u64 = NOISE_SEED;
    for _ in 0..NOISE_PIXEL_COUNT {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;

        let alpha = (state & 0xFF) as u8;
        // [0, 255] -> [0, 40] so the tallest speck is barely visible;
        // the numerator tops out at 255 * 40 = 10_200, well inside u16.
        let alpha = u16::from(alpha) * 40 / 255;
        // Safe: the division above bounds `alpha` to `[0, 40]`.
        let alpha = u8::try_from(alpha).unwrap_or(40);
        bytes.push(0xFF);
        bytes.push(0xFF);
        bytes.push(0xFF);
        bytes.push(alpha);
    }
    bytes
}

/// Build the tileable noise Image used by every [`DesktopBackdrop`]
/// instance. Constructed once at widget creation; cloning the `Image`
/// only bumps the internal `Arc` so repeated draws don't reallocate.
fn build_noise_image() -> Image {
    let bytes = generate_noise_bytes();
    let blob = Blob::new(Arc::new(bytes));
    let data = ImageData {
        data: blob,
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: NOISE_SIZE,
        height: NOISE_SIZE,
    };
    Image::new(data).with_extend(Extend::Repeat)
}

/// Widget that paints the desktop backdrop (a wallpaper, else gradient +
/// noise) behind its single child.
pub struct DesktopBackdrop {
    child: Option<Box<dyn Widget>>,
    rect: Rect,
    noise: Image,
    /// Re-encoded only on size or theme change; mirrors
    /// `ui::widget::Button`'s scene-cache pattern.
    cached: Scene,
    /// `Rect::ZERO` means no cache yet; a size mismatch triggers a rebuild.
    cache_rect: Rect,
    /// A theme switch invalidates the cache the same way a resize does.
    cache_stops: Option<(Color, Color)>,
    /// Private workspace identity used only to select a local image cache.
    /// It never enters workspace replication or app-visible widget state.
    workspace_id: Option<String>,
    wallpaper: Option<WallpaperProvider>,
    /// The displays' logical rectangles, in this widget's coordinates. A
    /// wallpaper covers each one on its own, so a laptop panel and an
    /// external monitor of another shape both show the whole picture
    /// rather than two halves of one stretched across them. Empty: the
    /// widget's own rect is the one display.
    displays: Vec<Rect>,
    /// The blob of the wallpaper last drawn, so a wallpaper the host
    /// changes (a portal import, a choice in Settings) asks for a frame.
    drawn_wallpaper: Option<u64>,
}

/// Host-supplied lookup from a workspace identity to its verified decoded wallpaper.
type WallpaperProvider = Box<dyn Fn(&str) -> Option<ImageData>>;

/// The ground is the system's largest, quietest surface — at desktop
/// scale the surface family's tint stops reading as a hue and becomes
/// "the colour of the machine". Easing toward luminance keeps each
/// theme's identity while letting content, not the wallpaper, carry colour.
fn ground_stops(theme: &Theme) -> (Color, Color) {
    let base = settle_toward_neutral(theme.tokens.colors.surfaces.base, GROUND_NEUTRALITY);
    (base, shade(base, GROUND_FALLOFF))
}

/// `v` is clamped into `[0.0, 255.0]` by every caller in this module
/// immediately before calling, so the truncation and sign-loss this
/// cast could otherwise cause cannot happen.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers clamp v into [0.0, 255.0] right before calling"
)]
fn clamped_f32_to_u8(v: f32) -> u8 {
    v as u8
}

/// Both gradient ends are the *same* ground, one lit and one fallen
/// away — deriving the far end from the near one makes the ramp read
/// as a surface catching light, not two materials.
fn shade(color: Color, factor: f32) -> Color {
    let [red, green, blue, alpha] = color.to_rgba8().to_u8_array();
    let clamped_factor = factor.clamp(0.0, 1.0);
    let scale = |channel: u8| {
        clamped_f32_to_u8(
            (f32::from(channel) * clamped_factor)
                .round()
                .clamp(0.0, 255.0),
        )
    };
    Color::from_rgba8(scale(red), scale(green), scale(blue), alpha)
}

/// Mix `color` toward its own perceived luminance by `amount` in
/// `0.0..=1.0`. At 1 it is fully grey at the same brightness, so the
/// result only drains the tint — never lightens or darkens.
fn settle_toward_neutral(color: Color, amount: f32) -> Color {
    let [red, green, blue, alpha] = color.to_rgba8().to_u8_array();
    let luma = 0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue);
    let mix_amount = amount.clamp(0.0, 1.0);
    let mix = |channel: u8| {
        clamped_f32_to_u8(
            (f32::from(channel) + (luma - f32::from(channel)) * mix_amount)
                .round()
                .clamp(0.0, 255.0),
        )
    };
    Color::from_rgba8(mix(red), mix(green), mix(blue), alpha)
}

/// Bit-exact float inequality, for cache-invalidation checks that need
/// to catch any change — including one too small for a tolerance-based
/// comparison to notice — rather than clippy's `float_cmp`-flagged `!=`.
fn f64_ne(a: f64, b: f64) -> bool {
    a.to_bits() != b.to_bits()
}

impl DesktopBackdrop {
    pub fn new() -> Self {
        Self {
            child: None,
            rect: Rect::ZERO,
            noise: build_noise_image(),
            cached: Scene::new(),
            cache_rect: Rect::ZERO,
            cache_stops: None,
            workspace_id: None,
            wallpaper: None,
            displays: Vec::new(),
            drawn_wallpaper: None,
        }
    }

    /// Construct a backdrop for one local workspace. The image index is
    /// watched by mtime, so a portal change becomes visible without a shell
    /// tree rebuild.
    pub fn for_workspace(workspace_id: impl Into<String>) -> Self {
        let mut backdrop = Self::new();
        backdrop.workspace_id = Some(workspace_id.into());
        backdrop
    }

    /// The host supplies verified decoded images; no native paths cross this
    /// boundary. A backdrop with no workspace asks for the empty id.
    #[must_use]
    pub fn with_wallpaper_provider(
        mut self,
        provider: impl Fn(&str) -> Option<ImageData> + 'static,
    ) -> Self {
        self.wallpaper = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
    }

    /// Place the wallpaper on `displays`, each display's logical rectangle:
    /// the picture is cover-fitted to every one of them. An empty list
    /// covers the backdrop's own rect.
    pub fn set_displays(&mut self, displays: &[Rect]) {
        self.displays.clear();
        self.displays.extend(
            displays
                .iter()
                .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0),
        );
    }

    /// The rectangles a wallpaper is fitted to.
    pub fn display_rects(&self) -> Vec<Rect> {
        if self.displays.is_empty() {
            vec![self.rect]
        } else {
            self.displays.clone()
        }
    }

    /// Exposed for tests; there is no runtime consumer.
    #[cfg(test)]
    pub(crate) fn noise_bytes_len(&self) -> usize {
        self.noise.image.data.len()
    }

    fn current_wallpaper(&self) -> Option<ImageData> {
        let provider = self.wallpaper.as_ref()?;
        provider(self.workspace_id.as_deref().unwrap_or_default())
            .filter(|image| image.width > 0 && image.height > 0)
    }

    fn paint_backdrop(&mut self, out_scene: &mut Scene, out_transform: Affine, theme: &Theme) {
        // A wallpaper is one opaque image fill per display: the picture is
        // decoded once by the host and stays resident in the renderer's
        // image atlas, so a frame costs its sampling and nothing else. It
        // covers the gradient entirely, so the gradient is not drawn under it.
        if let Some(image) = self.current_wallpaper() {
            self.drawn_wallpaper = Some(image.data.id());
            let blur = theme.tokens.colors.surfaces.material_blur_px;
            let mut pieces = Vec::new();
            for display in self.display_rects() {
                draw_cover_image(out_scene, out_transform, &display, &image);
                // What a docked tab strip frosts with: this display's
                // picture blurred, made once per wallpaper and size.
                if let Some(blurred) = ui::backdrop::desktop::blurred_cover(&image, display, blur) {
                    pieces.push(ui::backdrop::desktop::image_piece(
                        out_transform,
                        display,
                        blurred,
                    ));
                }
            }
            ui::backdrop::desktop::publish(pieces);
            return;
        }
        self.drawn_wallpaper = None;
        let stops = ground_stops(theme);
        // The gradient is already as smooth as its blur would be.
        ui::backdrop::desktop::publish(vec![ui::backdrop::desktop::smooth_piece(
            out_transform,
            self.rect,
            Brush::Gradient(self.gradient(stops)),
        )]);
        // Bit-exact comparison, not "close enough": any change (even one
        // ULP, from a layout recompute) must invalidate the cached scene,
        // so a tolerant comparison would let a real resize slip through
        // unnoticed.
        let size_changed = f64_ne(self.rect.width(), self.cache_rect.width())
            || f64_ne(self.rect.height(), self.cache_rect.height());
        let theme_changed = self.cache_stops != Some(stops);
        let needs_rebuild = self.cache_rect == Rect::ZERO || size_changed || theme_changed;

        if !needs_rebuild {
            // Pure-translation reuse — same as `Button::draw`.
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            // `pre_translate` is kurbo's non-operator equivalent of
            // `out_transform * Affine::translate((dx, dy))`.
            out_scene.append(
                &self.cached,
                Some(out_transform.pre_translate((dx, dy).into())),
            );
            return;
        }

        // Same `std::mem::take` dance as `Column::draw`,
        // to avoid an aliasing borrow.
        let mut local = std::mem::take(&mut self.cached);
        local.reset();
        self.encode_backdrop(&mut local, Affine::IDENTITY, stops);
        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_rect = self.rect;
        self.cache_stops = Some(stops);
    }

    fn gradient(&self, stops: (Color, Color)) -> Gradient {
        Gradient::new_linear(
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x1, self.rect.y1),
        )
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: stops.0.into(),
                },
                ColorStop {
                    offset: 1.0,
                    color: stops.1.into(),
                },
            ][..],
        )
    }

    fn encode_backdrop(&self, scene: &mut Scene, transform: Affine, stops: (Color, Color)) {
        let gradient = self.gradient(stops);
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(gradient),
            None,
            &self.rect,
        );

        // `push_layer` with `NOISE_ALPHA` dampens the already-subtle
        // per-pixel alpha further so the effect reads as grain, not dots.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            NOISE_ALPHA,
            transform,
            &self.rect,
        );
        // Align the noise pattern with the backdrop's top-left corner so
        // tiling is stable across draws.
        let brush_transform = Affine::translate((self.rect.x0, self.rect.y0));
        scene.fill(
            Fill::NonZero,
            transform,
            &self.noise,
            Some(brush_transform),
            &self.rect,
        );
        scene.pop_layer();
    }
}

/// Fill `rect` with `image` scaled to cover it, centred, cropping the
/// overhang of whichever side is longer.
fn draw_cover_image(scene: &mut Scene, transform: Affine, rect: &Rect, image: &ImageData) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || image.width == 0 || image.height == 0 {
        return;
    }
    let scale =
        (rect.width() / f64::from(image.width)).max(rect.height() / f64::from(image.height));
    let width = f64::from(image.width) * scale;
    let height = f64::from(image.height) * scale;
    let x = rect.x0 + (rect.width() - width) / 2.0;
    let y = rect.y0 + (rect.height() - height) / 2.0;
    let brush = ImageBrush::new(image.clone()).with_extend(Extend::Pad);
    let brush_transform = Affine::translate((x, y)).pre_scale_non_uniform(scale, scale);
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        rect,
    );
}

impl Default for DesktopBackdrop {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for DesktopBackdrop {
    ui::provides!(
        dyn ui::permissions::capability::PermissionGated,
        dyn ui::presence::capability::PublishesPresence
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_node = self
            .child
            .as_mut()
            .map(|c| c.build_layout(taffy, theme, node_map));

        let style = Style {
            display: taffy::Display::Flex,
            flex_shrink: 0.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        };

        let Some(child) = child_node else {
            return taffy.widget_leaf(style, node_map, self);
        };
        // `child` was just created in this same tree, so this is not
        // reachable to fail; if it (impossibly) does, fall back to
        // the child node itself rather than panicking this
        // panic=abort process.
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);

        if let Some(child) = &mut self.child {
            let Ok(children) = taffy.children(node) else {
                return;
            };
            let Some(&child_node) = children.first() else {
                return;
            };
            child.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint_backdrop(scene, transform, theme);
        if let Some(child) = &mut self.child {
            ui::widget::draw_child(
                child.as_mut(),
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(child) = &mut self.child {
            let Ok(children) = ctx.taffy.children(ctx.node_id) else {
                return;
            };
            let Some(&child_node) = children.first() else {
                return;
            };
            child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "DesktopBackdrop"
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty())
    }

    /// Dirty before the first draw (`cache_rect == ZERO` with no wallpaper
    /// drawn), and when the host's wallpaper is not the one last drawn; the
    /// child subtree is asked independently.
    fn needs_redraw(&self) -> bool {
        let first = self.cache_rect == Rect::ZERO && self.drawn_wallpaper.is_none();
        let wallpaper_moved = self.wallpaper.is_some()
            && self.current_wallpaper().map(|image| image.data.id()) != self.drawn_wallpaper;
        first || wallpaper_moved || self.child.as_ref().is_some_and(|c| c.needs_redraw())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// Forward layout-dirty queries to the child — the backdrop has no
    /// intrinsic size of its own, just `percent(1.0)` of the parent.
    /// Without this the default (`true`) would re-trigger taffy every
    /// frame.
    fn is_layout_dirty(&self) -> bool {
        self.child.as_ref().is_some_and(|c| c.is_layout_dirty())
    }

    fn mark_drawn(&mut self) {
        // Clearing happens implicitly via cache_rect becoming non-zero.
        if let Some(child) = &mut self.child {
            child.mark_drawn();
        }
    }
}

impl ui::presence::capability::PublishesPresence for DesktopBackdrop {}

impl ui::permissions::capability::PermissionGated for DesktopBackdrop {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The backdrop's baked noise texture must exist and have the right
    /// byte-size (64×64 RGBA8 = 16384 bytes). Guards against someone
    /// accidentally dropping the noise-generation path.
    #[test]
    fn noise_buffer_is_populated() {
        let backdrop = DesktopBackdrop::new();
        let expected = (NOISE_SIZE as usize) * (NOISE_SIZE as usize) * 4;
        assert_eq!(backdrop.noise_bytes_len(), expected);
        assert_eq!(backdrop.noise.image.width, NOISE_SIZE);
        assert_eq!(backdrop.noise.image.height, NOISE_SIZE);
    }

    /// The noise should actually have some non-zero alpha pixels; a
    /// buffer of all zeros would render nothing and silently regress the
    /// overlay without tripping the size assertion above.
    #[test]
    fn noise_buffer_has_nonzero_alpha() {
        let bytes = generate_noise_bytes();
        let any_visible = bytes.chunks_exact(4).any(|px| px[3] > 0);
        assert!(any_visible, "generated noise has no visible specks");
    }

    /// The PRNG must be deterministic — regenerating must produce the
    /// same bytes. Keeps headless render snapshots stable.
    #[test]
    fn noise_generation_is_deterministic() {
        assert_eq!(generate_noise_bytes(), generate_noise_bytes());
    }
}
