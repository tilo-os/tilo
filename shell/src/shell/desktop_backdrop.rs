//! Desktop backdrop widget — paints the nirios desktop background behind
//! all shell content.
//!
//! Matches `nirios-concept/src/components/DesktopView.tsx:210-217`:
//!   1. Linear gradient top-left → bottom-right, slate-900 (#0f172a) to
//!      slate-950 (#020617).
//!   2. 24×24 px grid of 1px lines at `rgba(128, 128, 128, 0.07)`.
//!   3. A tiled 64×64 noise overlay at low opacity that breaks banding on
//!      the gradient. The concept uses `bg-[url('noise.svg')] opacity-20
//!      mix-blend-overlay`; we approximate with a procedurally-generated
//!      RGBA8 buffer uploaded to a Vello `Image` with `Extend::Repeat` so
//!      it tiles across the full backdrop rect.
//!
//! Structurally, this widget is a thin `Container`-style wrapper around an
//! optional single child: it forwards sync/layout/events/presence/commit
//! exactly like [`ui::widget::Container`], but with no padding and
//! no rounded-rect chrome — just the flat gradient + grid beneath the
//! child.

use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::permissions::{PermissionContext, PermissionScope};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{
    Blob, Brush, Color, ColorStop, ColorStops, Extend, Fill, Gradient, GradientKind,
    ImageAlphaType, ImageBrush as Image, ImageData, ImageFormat as Format,
};
use winit::event::{Modifiers, WindowEvent};

/// Size (in logical pixels) of one grid cell.
const GRID_STEP: f64 = 24.0;

/// Edge length (in pixels) of the tileable noise texture.
const NOISE_SIZE: u32 = 64;

/// Opacity applied when tiling the noise image across the backdrop. The
/// concept uses `opacity-20 mix-blend-overlay` which — because overlay
/// dampens mid-grey against a near-black gradient — works out to roughly
/// this value. Dial this up only if banding is still visible; the dots
/// should not be individually distinguishable.
const NOISE_ALPHA: f32 = 0.08;

/// Deterministic seed for the noise PRNG. Keeping this fixed means every
/// session renders the exact same grain pattern, which in turn keeps the
/// backdrop byte-identical across the headless render tests.
const NOISE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Generate a 64×64 RGBA8 buffer of deterministic white-noise specks
/// suitable for tiling. Each pixel is `(0xFF, 0xFF, 0xFF, alpha)` where
/// `alpha ∈ [0, 40]` — keeping the peak alpha low ensures the overlay
/// stays subtle even before the layer-wide [`NOISE_ALPHA`] multiplier is
/// applied on top.
///
/// Uses a tiny hand-rolled xorshift64 PRNG (no `rand` dep needed) seeded
/// from [`NOISE_SEED`] so the output is bit-for-bit reproducible.
fn generate_noise_bytes() -> Vec<u8> {
    let pixel_count = (NOISE_SIZE as usize) * (NOISE_SIZE as usize);
    let mut bytes = Vec::with_capacity(pixel_count * 4);

    // xorshift64 state — must be non-zero.
    let mut state: u64 = NOISE_SEED;
    for _ in 0..pixel_count {
        // xorshift64 step
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;

        let alpha = (state & 0xFF) as u8;
        // Map [0, 255] → [0, 40] so the tallest speck is barely visible.
        let alpha = (alpha as u16 * 40 / 255) as u8;
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
    /// Pre-baked tileable noise texture. Uploaded once on construction and
    /// reused on every draw via `Extend::Repeat`.
    noise: Image,
}

impl DesktopBackdrop {
    pub fn new() -> Self {
        Self {
            child: None,
            rect: Rect::ZERO,
            noise: build_noise_image(),
        }
    }

    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
    }

    /// Length in bytes of the raw RGBA8 noise buffer backing the tiled
    /// overlay. Exposed for tests; there is no runtime consumer.
    #[cfg(test)]
    pub(crate) fn noise_bytes_len(&self) -> usize {
        self.noise.image.data.len()
    }

    fn paint_backdrop(&self, scene: &mut Scene, transform: Affine) {
        // 1. Diagonal gradient: top-left slate-900 → bottom-right slate-950.
        let gradient = Gradient::new_linear(
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x1, self.rect.y1),
        )
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: Color::from_rgb8(0x0f, 0x17, 0x2a).into(),
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::from_rgb8(0x02, 0x06, 0x17).into(),
                },
            ][..],
        );
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(gradient),
            None,
            &self.rect,
        );

        // 2. Grid — 1px lines at rgba(128, 128, 128, 0.07) every 24px.
        //    Clip to self.rect so lines don't bleed outside the backdrop.
        let line_color = Color::from_rgba8(128, 128, 128, 18); // 0.07 * 255 ≈ 18
        let stroke = Stroke::new(1.0);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );

        // Vertical lines.
        let mut x = (self.rect.x0 / GRID_STEP).ceil() * GRID_STEP;
        while x < self.rect.x1 {
            let line = Line::new((x, self.rect.y0), (x, self.rect.y1));
            scene.stroke(&stroke, transform, line_color, None, &line);
            x += GRID_STEP;
        }

        // Horizontal lines.
        let mut y = (self.rect.y0 / GRID_STEP).ceil() * GRID_STEP;
        while y < self.rect.y1 {
            let line = Line::new((self.rect.x0, y), (self.rect.x1, y));
            scene.stroke(&stroke, transform, line_color, None, &line);
            y += GRID_STEP;
        }

        scene.pop_layer();

        // 3. Noise overlay — tile the 64×64 repeating Image across the
        //    backdrop rect. The fill shape is the rect itself; `Extend::Repeat`
        //    on the Image makes it wrap. A `push_layer` with `NOISE_ALPHA`
        //    dampens the already-subtle per-pixel alpha further so the effect
        //    registers as grain rather than visible dots.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            NOISE_ALPHA,
            transform,
            &self.rect,
        );
        // Align the noise pattern with the backdrop's top-left corner so
        // the tiling is stable across draws (Image uv defaults to (0, 0) at
        // the origin in brush-transform space).
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

impl Default for DesktopBackdrop {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for DesktopBackdrop {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(child) = &mut self.child {
            child.sync(state, path);
        }
    }

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
                width: percent(1.0),
                height: percent(1.0),
            },
            ..Default::default()
        };

        let node = if let Some(child) = child_node {
            taffy.new_with_children(style, &[child]).unwrap()
        } else {
            taffy.new_leaf(style).unwrap()
        };
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

        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node).unwrap()[0];
            child.apply_layout(
                taffy,
                child_node,
                Point::new(self.rect.x0, self.rect.y0),
            );
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
        self.paint_backdrop(scene, transform);
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
        self.paint_backdrop(scene, transform);
        if let Some(child) = &mut self.child {
            child.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node_id).unwrap()[0];
            child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
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

    fn name(&self) -> &str {
        "DesktopBackdrop"
    }

    fn is_focused(&self) -> bool {
        false
    }

    fn needs_commit(&self) -> bool {
        self.children().iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for child in self.children_mut() {
            child.on_commit();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false)
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        None
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The backdrop's baked noise texture must exist and have the right
    /// byte-size (64×64 RGBA8 = 16384 bytes). Guards against someone
    /// accidentally dropping the `include_bytes!` / procedural generation
    /// path and reverting the visual gap documented in
    /// `docs/nirios/visual-gaps.md` item #10.
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
