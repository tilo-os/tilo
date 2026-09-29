//! Desktop backdrop widget — paints the desktop background behind all
//! shell content: a diagonal gradient across the theme's ground surface
//! ([`ground_stops`]), a 24×24 px grid of faint lines, and a tiled
//! 64×64 procedural noise overlay that breaks banding on the gradient.
//!
//! Structurally a thin `Container`-style wrapper around one optional
//! child, forwarding sync/layout/events like [`ui::widget::Container`]
//! but with no padding or chrome — just the gradient + grid beneath it.

use std::sync::Arc;

use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use ui::permissions::{PermissionContext, PermissionScope};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{
    Blob, Brush, Color, ColorStop, Extend, Fill, Gradient, ImageAlphaType, ImageBrush,
    ImageBrush as Image, ImageData, ImageFormat as Format,
};
use winit::event::WindowEvent;

/// Size (in logical pixels) of one grid cell.
const GRID_STEP: f64 = 24.0;

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

/// Widget that paints the nirios desktop backdrop (gradient + grid + noise)
/// behind its single child.
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

/// How many grid lines spaced [`GRID_STEP`] apart, starting at
/// `start_col * GRID_STEP`, fall strictly before `end`. A count rather
/// than a `while` loop over the float position itself, so there is no
/// floating-point loop counter to drift on a wide rect.
fn grid_line_count(start_col: f64, end: f64) -> u32 {
    let span = end - start_col * GRID_STEP;
    if !span.is_finite() || span <= 0.0 {
        return 0;
    }
    // A real screen rect never needs anywhere near u32::MAX grid lines;
    // clamp rather than let an extreme rect panic or wrap the cast.
    f64_to_u32_clamped((span / GRID_STEP).ceil())
}

/// Clamp `v` into `u32`'s range before converting — safe for any finite
/// `v`, including one far outside `u32`, and for a non-finite `v`.
fn f64_to_u32_clamped(v: f64) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        0
    } else if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // Safe: v is checked into [0.0, u32::MAX] immediately above.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "v is checked into [0.0, u32::MAX] immediately above"
        )]
        {
            v as u32
        }
    }
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

    /// The host supplies verified decoded images; no native paths cross this boundary.
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

    /// Exposed for tests; there is no runtime consumer.
    #[cfg(test)]
    pub(crate) fn noise_bytes_len(&self) -> usize {
        self.noise.image.data.len()
    }

    /// Re-encoding the gradient + ~80 grid strokes + noise every frame
    /// dominated the at-rest `draw=` budget under `TILO_UI_PERF=1`, so
    /// this bakes once and replays via translate.
    fn paint_backdrop(&mut self, out_scene: &mut Scene, out_transform: Affine, theme: &Theme) {
        let stops = ground_stops(theme);
        // Kept outside the gradient/grid scene cache: a portal import updates
        // the private wallpaper registry asynchronously and must appear
        // without requiring an unrelated resize or theme change.
        if let Some(image) = self
            .workspace_id
            .as_deref()
            .and_then(|id| self.wallpaper.as_ref().and_then(|provider| provider(id)))
        {
            draw_cover_image(out_scene, out_transform, &self.rect, &image);
        }
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

        // Same `std::mem::take` dance as `Column::draw_with_presence`,
        // to avoid an aliasing borrow.
        let mut local = std::mem::take(&mut self.cached);
        local.reset();
        self.encode_backdrop(&mut local, Affine::IDENTITY, stops);
        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_rect = self.rect;
        self.cache_stops = Some(stops);
    }

    fn encode_backdrop(&self, scene: &mut Scene, transform: Affine, stops: (Color, Color)) {
        let gradient = Gradient::new_linear(
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
        );
        // Keep the shell's tonal treatment above a photo, but let the
        // imported image visibly determine the desktop. This also preserves
        // the old appearance when no wallpaper is configured.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            if self.workspace_id.is_some() {
                0.42
            } else {
                1.0
            },
            transform,
            &self.rect,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(gradient),
            None,
            &self.rect,
        );
        scene.pop_layer();

        // The grid is a hairline derived against the ground it sits on
        // (HIG §9.5) — the backdrop's own first stop — so it stays faint on
        // light and dark alike instead of a fixed mid-grey that reads darker
        // on one and lighter on the other.
        let line_color = ui::tokens::colors::Colors::hairline_on(stops.0, 18);
        let stroke = Stroke::new(1.0);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );

        // Grid-line count computed up front, then each position derived
        // fresh from an integer index (`start_col + f64::from(i)`)
        // rather than accumulated via `x += GRID_STEP` — a float loop
        // counter drifts by rounding error over a wide rect.
        let start_col = (self.rect.x0 / GRID_STEP).ceil();
        for i in 0..grid_line_count(start_col, self.rect.x1) {
            let x = (start_col + f64::from(i)) * GRID_STEP;
            let line = Line::new((x, self.rect.y0), (x, self.rect.y1));
            scene.stroke(&stroke, transform, line_color, None, &line);
        }

        let start_row = (self.rect.y0 / GRID_STEP).ceil();
        for i in 0..grid_line_count(start_row, self.rect.y1) {
            let y = (start_row + f64::from(i)) * GRID_STEP;
            let line = Line::new((self.rect.x0, y), (self.rect.x1, y));
            scene.stroke(&stroke, transform, line_color, None, &line);
        }

        scene.pop_layer();

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
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::undo::UndoPoints
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

        let node = if let Some(child) = child_node {
            // `child` was just created in this same tree, so this is not
            // reachable to fail; if it (impossibly) does, fall back to
            // the child node itself rather than panicking this
            // panic=abort process.
            taffy.new_with_children(style, &[child]).unwrap_or(child)
        } else {
            match taffy.new_leaf(style) {
                Ok(node) => node,
                Err(err) => {
                    // `TaffyTree::new_leaf` only ever returns `Ok` for a
                    // fresh, unattached leaf; this arm exists so a
                    // future upstream change degrades the backdrop
                    // instead of aborting the whole session.
                    tracing::error!("desktop_backdrop: leaf allocation failed: {err}");
                    NodeId::from(0u64)
                }
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );

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
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        self.paint_backdrop(scene, transform, theme);
        if let Some(child) = &mut self.child {
            child.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
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

    fn is_focused(&self) -> bool {
        false
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty())
    }

    /// A pure function of `self.rect`'s size, so it never reports dirty
    /// after the first draw; the child subtree is asked independently.
    /// `cache_rect == ZERO` covers the pre-first-draw boot path.
    fn needs_redraw(&self) -> bool {
        self.cache_rect == Rect::ZERO || self.child.as_ref().is_some_and(|c| c.needs_redraw())
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

impl ui::undo::UndoPoints for DesktopBackdrop {
    fn needs_commit(&self) -> bool {
        self.children().iter().any(|c| ui::undo::needs_commit(*c))
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            ui::undo::on_commit(child);
        }
    }
}

impl ui::presence::capability::PublishesPresence for DesktopBackdrop {
    fn presence_key(&self) -> Option<PresenceKey> {
        None
    }
}

impl ui::permissions::capability::PermissionGated for DesktopBackdrop {
    fn permission_scope(&self) -> Option<PermissionScope> {
        None
    }
}

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
