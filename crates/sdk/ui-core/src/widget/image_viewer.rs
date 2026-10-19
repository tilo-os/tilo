//! Interactive image viewer with pan + zoom semantics, unlike the static
//! [`crate::widget::Image`] widget: scroll-wheel zoom, click-drag pan, and
//! a deterministic "fit to viewport" reset. Interaction state (zoom, pan,
//! drag origin) lives on the widget directly; CRDT reflection is out of scope.
//!
//! Coordinate model: `zoom == 1.0` is the "contain" baseline (mirrors CSS
//! `object-fit: contain`). Higher zooms multiply that baseline scale, and
//! `pan` is an *image-space* offset applied after it — storing pan in image
//! space rather than pixels keeps drag velocity constant across zooms.
//! Scroll-wheel zoom centres on the cursor by re-projecting the image-space
//! point under it before/after the scale change and nudging `pan` to keep
//! that point fixed on screen — otherwise zooming "to a feature" would feel
//! like the image sliding away from the pointer.

use crate::num::f32_of_f64;
use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{self, BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Minimum/maximum zoom factor. Below 0.1 the image becomes effectively
/// invisible; above 10.0 the sampling artifacts dominate over content.
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 10.0;
/// Per-notch zoom factor: one wheel tick (`LineDelta` y == 1.0) multiplies
/// the zoom by `ZOOM_STEP`. Chosen to feel responsive without skipping
/// over detail — `1.1` per notch matches Krita/Inkscape defaults.
const ZOOM_STEP: f32 = 1.1;

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated_and_scaled(transform: Affine, x: f64, y: f64, scale: f64) -> Affine {
    transform * Affine::translate((x, y)) * Affine::scale_non_uniform(scale, scale)
}

/// Pure photo viewport geometry shared by live widgets and immutable History.
#[derive(Clone, Copy, Debug)]
pub struct ImageViewport {
    pub rect: Rect,
    pub zoom: f64,
    pub pan: Vec2,
}
impl ImageViewport {
    pub fn base_scale(self, width: u32, height: u32) -> f64 {
        if width == 0 || height == 0 || self.rect.width() <= 0. || self.rect.height() <= 0. {
            return 0.;
        }
        (self.rect.width() / f64::from(width)).min(self.rect.height() / f64::from(height))
    }
    pub fn screen_to_image(self, width: u32, height: u32, screen: Vec2) -> Option<Vec2> {
        let scale = self.base_scale(width, height) * self.zoom;
        if !scale.is_finite() || scale <= 0. {
            return None;
        }
        Some(Vec2::new(
            (screen.x - self.rect.center().x) / scale - self.pan.x,
            (screen.y - self.rect.center().y) / scale - self.pan.y,
        ))
    }
    pub fn image_transform(self, width: u32, height: u32) -> Option<Affine> {
        let scale = self.base_scale(width, height) * self.zoom;
        if !scale.is_finite() || scale <= 0. || !self.pan.x.is_finite() || !self.pan.y.is_finite() {
            return None;
        }
        let x = self.rect.center().x - f64::from(width) * scale * 0.5 + self.pan.x * scale;
        let y = self.rect.center().y - f64::from(height) * scale * 0.5 + self.pan.y * scale;
        Some(translated_and_scaled(Affine::IDENTITY, x, y, scale))
    }
    /// Paint only supplied pixels. No widget, event routing, or live service.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine composition is floating-point geometry"
    )]
    pub fn draw(
        self,
        image: &peniko::ImageData,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.media.letterbox,
            None,
            &self.rect,
        );
        let Some(image_transform) = self.image_transform(image.width, image.height) else {
            return;
        };
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.,
            transform,
            &self.rect,
        );
        scene.draw_image(image, transform * image_transform);
        scene.pop_layer();
    }
}

/// Interactive viewer for a pre-decoded [`peniko::ImageData`]. See module
/// docs for the contract.
pub struct ImageViewer {
    image: peniko::ImageData,
    zoom: f32,
    pan: Vec2,
    drag_origin: Option<Vec2>,
    /// Last cursor position seen in this widget's logical-pixel space.
    /// Cached so [`WindowEvent::MouseWheel`] (which carries no position)
    /// can still anchor its zoom on the pointer.
    cursor: Option<Vec2>,
    rect: Rect,
}

impl ImageViewer {
    /// The image is `Arc`-backed, so cloning the inner `peniko::ImageData`
    /// is cheap — the caller may share the same source with other widgets.
    pub fn new(image: peniko::ImageData) -> Self {
        Self {
            image,
            zoom: 1.0,
            pan: Vec2::ZERO,
            drag_origin: None,
            cursor: None,
            rect: Rect::ZERO,
        }
    }

    pub fn fit_to_viewport(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Image-space pixels (i.e. measured at `zoom == 1.0`).
    pub fn pan(&self) -> Vec2 {
        self.pan
    }

    /// Zoom to `z`, within the zoom's limits, about the viewport's centre,
    /// as the wheel turned with the cursor there does: the screen-to-image
    /// mapping is the same there at every zoom, so the pan stays.
    pub fn set_zoom(&mut self, z: f32) {
        self.zoom = z.clamp(ZOOM_MIN, ZOOM_MAX);
    }

    #[cfg(test)]
    pub fn set_pan(&mut self, p: Vec2) {
        self.pan = p;
    }

    fn viewport(&self) -> ImageViewport {
        ImageViewport {
            rect: self.rect,
            zoom: f64::from(self.zoom),
            pan: self.pan,
        }
    }

    /// Returns `0.0` if either the image or the viewport has a zero
    /// dimension — callers must check before using the result.
    fn base_scale(&self) -> f64 {
        self.viewport()
            .base_scale(self.image.width, self.image.height)
    }

    /// Inverse of the destination-quad transform built in `draw`.
    /// `None` when the effective scale is zero (degenerate image or
    /// viewport).
    fn screen_to_image(&self, screen: Vec2) -> Option<Vec2> {
        self.viewport()
            .screen_to_image(self.image.width, self.image.height, screen)
    }
}

impl Widget for ImageViewer {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.viewport().draw(&self.image, scene, transform, theme);
    }

    /// `kurbo::Vec2`'s `Sub`/`AddAssign` below are plain f64 component-wise
    /// math with no panic path; `arithmetic_side_effects` flags the
    /// operator overloads anyway.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo::Vec2 arithmetic is f64 component-wise math; it never panics or overflows, unlike integer +/-/*"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Vec2::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let prev = self.cursor;
                self.cursor = Some(logical);
                // `pan` is image-space, so the screen delta is divided by
                // the total scale — a drag of 50px shifts the image 50px
                // under the cursor regardless of zoom level.
                if let (Some(start), Some(prev_pos)) = (self.drag_origin, prev) {
                    let _ = start;
                    let base = self.base_scale();
                    if base > 0.0 {
                        let total_scale = base * f64::from(self.zoom);
                        let delta = logical - prev_pos;
                        self.pan += Vec2::new(delta.x / total_scale, delta.y / total_scale);
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => match element_state {
                ElementState::Pressed => {
                    if let Some(c) = self.cursor {
                        let p = Point::new(c.x, c.y);
                        if self.rect.contains(p) {
                            self.drag_origin = Some(c);
                            *ctx.handled = true;
                        }
                    }
                }
                ElementState::Released => {
                    if self.drag_origin.is_some() {
                        self.drag_origin = None;
                        *ctx.handled = true;
                    }
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(cursor) = self.cursor else {
                    return;
                };
                if !self.rect.contains(Point::new(cursor.x, cursor.y)) {
                    return;
                }
                // PixelDelta is normalised to a "lines scrolled" scalar
                // with a 100-pixel notch, matching winit's convention for
                // platforms that report pixel deltas for trackpads.
                let scroll_y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(pp) => f32_of_f64(pp.y) / 100.0,
                };
                if scroll_y == 0.0 {
                    return;
                }

                // The image-space point under the cursor is re-projected
                // after the zoom change; `pan` is counter-translated by
                // the difference to keep it fixed on screen.
                let before = self.screen_to_image(cursor);
                let factor = ZOOM_STEP.powf(scroll_y);
                let new_zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
                if (new_zoom - self.zoom).abs() < f32::EPSILON {
                    *ctx.handled = true;
                    return;
                }
                self.zoom = new_zoom;
                if let (Some(before), Some(after)) = (before, self.screen_to_image(cursor)) {
                    self.pan += after - before;
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ImageViewer"
    }

    /// No alt-text field on the viewer (it's a pan/zoom canvas around
    /// raw `ImageData`), so the node carries no intrinsic name — a host
    /// that knows the source can supply one via its own wrapping `Image`.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageData as Image};
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

    /// 16×16 opaque white; the `Arc` inside `Blob` keeps clones zero-copy.
    fn make_image() -> Image {
        let pixels = vec![255u8; 4 * 16 * 16];
        let blob = Blob::new(Arc::new(pixels));
        crate::raster::rgba8(blob, 16, 16)
    }

    #[test]
    fn image_viewer_zooms_pans_fits_and_reports_role() {
        fn accessibility_reports_image_role() {
            use crate::a11y::Role;
            let v = ImageViewer::new(make_image());
            let node = Widget::accessibility(&v).expect("viewer is not presentational");
            assert_eq!(node.role, Role::Image);
        }

        fn image_viewer_default_zoom_is_one() {
            let iv = ImageViewer::new(make_image());
            assert_eq!(iv.zoom(), 1.0);
            assert_eq!(iv.pan(), Vec2::ZERO);
        }

        fn image_viewer_scroll_up_zooms_in() {
            let mut iv = ImageViewer::new(make_image());
            // Bypasses taffy — a real layout pass would need a parent.
            iv.rect = Rect::new(0.0, 0.0, 100.0, 100.0);

            let dev = winit::event::DeviceId::dummy();

            // So the wheel handler's `inside` check passes.
            let mut handled = dispatch(
                &mut iv,
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: PhysicalPosition::new(50.0, 50.0),
                },
            );

            handled |= dispatch(
                &mut iv,
                &WindowEvent::MouseWheel {
                    device_id: dev,
                    delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );

            assert!(
                iv.zoom() > 1.0,
                "positive scroll y must increase zoom; got {}",
                iv.zoom()
            );
            assert!(handled, "wheel event over the viewer must be claimed");
        }

        fn image_viewer_drag_pans() {
            let mut iv = ImageViewer::new(make_image());
            // The public surface doesn't need a rect setter — callers run
            // through taffy in production.
            iv.rect = Rect::new(0.0, 0.0, 200.0, 200.0);

            let dev = winit::event::DeviceId::dummy();

            dispatch(
                &mut iv,
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: PhysicalPosition::new(50.0, 50.0),
                },
            );

            dispatch(
                &mut iv,
                &WindowEvent::MouseInput {
                    device_id: dev,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            assert!(
                iv.drag_origin.is_some(),
                "Pressed inside rect must start a drag"
            );

            // Move cursor 30px right, 20px down; the magnitude is delta /
            // base_scale (zoom is 1.0, so total_scale == base_scale).
            dispatch(
                &mut iv,
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: PhysicalPosition::new(80.0, 70.0),
                },
            );

            dispatch(
                &mut iv,
                &WindowEvent::MouseInput {
                    device_id: dev,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            assert!(iv.drag_origin.is_none(), "Release must clear drag origin");
            let pan = iv.pan();
            assert_ne!(pan, Vec2::ZERO, "drag must produce a non-zero pan");
            assert!(
                pan.x > 0.0,
                "rightward drag must increase pan.x; got {}",
                pan.x
            );
            assert!(
                pan.y > 0.0,
                "downward drag must increase pan.y; got {}",
                pan.y
            );
        }

        fn image_viewer_fit_resets_zoom_and_pan() {
            let mut iv = ImageViewer::new(make_image());
            iv.set_zoom(2.0);
            iv.set_pan(Vec2::new(10.0, 10.0));
            iv.fit_to_viewport();
            assert_eq!(iv.zoom(), 1.0);
            assert_eq!(iv.pan(), Vec2::ZERO);
        }

        accessibility_reports_image_role();
        image_viewer_default_zoom_is_one();
        image_viewer_scroll_up_zooms_in();
        image_viewer_drag_pans();
        image_viewer_fit_resets_zoom_and_pan();
    }
}
