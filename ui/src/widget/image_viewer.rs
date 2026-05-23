//! Interactive image viewer with pan + zoom semantics.
//!
//! Unlike the static [`crate::widget::Image`] widget, [`ImageViewer`] is
//! built for inspection: scroll-wheel zoom, click-and-drag pan, and a
//! deterministic "fit to viewport" reset. The widget owns its
//! interaction state directly (zoom factor, pan offset, drag origin) —
//! collaboration-aware reflection into the CRDT is out of scope for the
//! initial cut; if a future spec needs synced pan/zoom this widget
//! grows a `sync` body, not a redesign.
//!
//! ## Coordinate model
//!
//! - `zoom == 1.0` is the "contain" baseline: the image is uniformly
//!   scaled so its longer axis fits exactly inside `self.rect` (the
//!   shorter axis ends up with margin on either side). That mirrors
//!   CSS `object-fit: contain`.
//! - Higher zooms multiply the baseline scale; `pan` is then an
//!   *image-space* offset applied after the scale. Storing pan in
//!   image space (rather than pixels) keeps the pan velocity constant
//!   across zooms: dragging by N pixels moves the visible image by N
//!   logical pixels regardless of current scale.
//! - Scroll-wheel zoom centres on the cursor: we compute the
//!   image-space point under the cursor before the scale change and
//!   nudge `pan` so the same image-space point lands under the same
//!   screen pixel afterwards. Without this, zooming "to a feature"
//!   would feel like the image was sliding away from the pointer.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{self, BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Minimum/maximum zoom factor. Below 0.1 the image becomes effectively
/// invisible; above 10.0 the sampling artifacts dominate over content.
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 10.0;
/// Per-notch zoom factor: one wheel tick (LineDelta y == 1.0) multiplies
/// the zoom by `ZOOM_STEP`. Chosen to feel responsive without skipping
/// over detail — `1.1` per notch matches Krita/Inkscape defaults.
const ZOOM_STEP: f32 = 1.1;

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
    /// Construct a viewer at the "fit to viewport" baseline. The image
    /// is borrowed by `Arc` underneath, so cloning the inner
    /// `peniko::ImageData` is cheap — the caller may share the same source
    /// with other widgets.
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

    /// Reset to the "contain" baseline: `zoom = 1.0`, `pan = ZERO`.
    /// Useful as a double-click handler or toolbar button target.
    pub fn fit_to_viewport(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }

    /// Current zoom multiplier on top of the fit-to-viewport baseline.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Current pan offset in image-space pixels (i.e. measured at
    /// `zoom == 1.0`).
    pub fn pan(&self) -> Vec2 {
        self.pan
    }

    /// Test-only setter for [`Self::zoom`]. Bypasses the wheel-event
    /// path so tests can stage state directly without simulating a
    /// chain of `MouseWheel` events.
    #[cfg(test)]
    pub fn set_zoom(&mut self, z: f32) {
        self.zoom = z;
    }

    /// Test-only setter for [`Self::pan`]. See [`Self::set_zoom`].
    #[cfg(test)]
    pub fn set_pan(&mut self, p: Vec2) {
        self.pan = p;
    }

    /// Baseline "contain" scale factor: the multiplier that fits the
    /// image inside `self.rect` at `zoom == 1.0`. Returns `0.0` if
    /// either the image or the viewport has a zero dimension — callers
    /// must check before using the result.
    fn base_scale(&self) -> f64 {
        let iw = self.image.width as f64;
        let ih = self.image.height as f64;
        if iw <= 0.0 || ih <= 0.0 {
            return 0.0;
        }
        let rw = self.rect.width();
        let rh = self.rect.height();
        if rw <= 0.0 || rh <= 0.0 {
            return 0.0;
        }
        (rw / iw).min(rh / ih)
    }

    /// Convert a logical-pixel point inside `self.rect` to image-space
    /// pixel coordinates given the current zoom and pan. Inverse of
    /// the brush-transform we'll build in `paint`. `None` when the
    /// effective scale is zero (degenerate image or viewport).
    fn screen_to_image(&self, screen: Vec2) -> Option<Vec2> {
        let base = self.base_scale();
        if base <= 0.0 {
            return None;
        }
        let total_scale = base * self.zoom as f64;
        // Centre of the rect — the image is anchored here at pan=0.
        let cx = (self.rect.x0 + self.rect.x1) * 0.5;
        let cy = (self.rect.y0 + self.rect.y1) * 0.5;
        let dx = screen.x - cx;
        let dy = screen.y - cy;
        // Reverse the affine: subtract pan (image-space), divide by scale.
        Some(Vec2::new(
            dx / total_scale - self.pan.x,
            dy / total_scale - self.pan.y,
        ))
    }
}

impl Widget for ImageViewer {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Background fill: any region of the rect not covered by the
        // scaled image gets the theme's base surface so the letterbox
        // bars match adjacent chrome.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        if self.image.width == 0 || self.image.height == 0 {
            return;
        }

        let base = self.base_scale();
        if base <= 0.0 {
            return;
        }
        let total_scale = base * self.zoom as f64;
        let iw = self.image.width as f64;
        let ih = self.image.height as f64;
        let dw = iw * total_scale;
        let dh = ih * total_scale;
        let cx = (self.rect.x0 + self.rect.x1) * 0.5;
        let cy = (self.rect.y0 + self.rect.y1) * 0.5;
        // The image's pixel (0,0) lands at the top-left of the scaled
        // destination quad, which is centred on the rect midpoint and
        // then translated by `pan * total_scale` (pan is in image-space
        // units, so it scales with zoom).
        let dx = cx - dw * 0.5 + self.pan.x * total_scale;
        let dy = cy - dh * 0.5 + self.pan.y * total_scale;

        // Clip to the rect so pan/zoom doesn't bleed over adjacent
        // chrome when the image is bigger than its viewport.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &self.rect);

        // `scene.draw_image` accepts an `Affine` that maps the image's
        // unit pixel space directly into world space — so a scale-then-
        // translate composes our destination quad.
        let img_transform = transform
            * Affine::translate((dx, dy))
            * Affine::scale_non_uniform(total_scale, total_scale);
        scene.draw_image(&self.image, img_transform);

        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Vec2::new(position.x / scale_factor, position.y / scale_factor);
                let prev = self.cursor;
                self.cursor = Some(logical);
                // Drag: continuous pan as long as the left button is
                // held. `pan` is image-space, so divide the screen
                // delta by the total scale; using `base * zoom` keeps
                // the on-screen translation rate equal to the cursor
                // delta regardless of zoom level (drag 50px → image
                // shifts 50px under the cursor).
                if let (Some(start), Some(prev_pos)) = (self.drag_origin, prev) {
                    let _ = start; // origin retained so we don't lose drag intent across events with no movement
                    let base = self.base_scale();
                    if base > 0.0 {
                        let total_scale = base * self.zoom as f64;
                        let delta = logical - prev_pos;
                        self.pan += Vec2::new(delta.x / total_scale, delta.y / total_scale);
                        *handled = true;
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
                            *handled = true;
                        }
                    }
                }
                ElementState::Released => {
                    if self.drag_origin.is_some() {
                        self.drag_origin = None;
                        *handled = true;
                    }
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let inside = match self.cursor {
                    Some(c) => self.rect.contains(Point::new(c.x, c.y)),
                    None => false,
                };
                if !inside {
                    return;
                }
                // Normalise both delta variants to a "lines scrolled"
                // scalar. PixelDelta is converted with a 100-pixel
                // notch (matches winit's documented convention on
                // platforms that report pixel deltas for trackpads).
                let scroll_y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(pp) => (pp.y as f32) / 100.0,
                };
                if scroll_y == 0.0 {
                    return;
                }

                // Anchor: compute the image-space point under the
                // cursor before changing zoom. After the zoom update
                // we re-project the cursor; the difference is how far
                // the image would have slid out from under the
                // pointer, so we counter-translate `pan` by that
                // amount to keep the anchor pixel fixed on screen.
                let cursor = self.cursor.expect("cursor inside-check passed");
                let before = self.screen_to_image(cursor);
                let factor = ZOOM_STEP.powf(scroll_y);
                let new_zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
                if (new_zoom - self.zoom).abs() < f32::EPSILON {
                    *handled = true;
                    return;
                }
                self.zoom = new_zoom;
                if let (Some(before), Some(after)) = (before, self.screen_to_image(cursor)) {
                    // `after - before` is how the anchor moved in
                    // image-space due to the zoom change; add it to
                    // pan to cancel out.
                    self.pan += after - before;
                }
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "ImageViewer"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageData as Image, ImageFormat as Format};
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

    /// Build a 16×16 opaque-white image suitable for tests. Cheap to
    /// construct; the `Arc` inside `Blob` keeps clones zero-copy.
    fn make_image() -> Image {
        let pixels = vec![255u8; 4 * 16 * 16];
        let blob = Blob::new(Arc::new(pixels));
        Image {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 16,
            height: 16,
        }
    }

    /// Synthesise the unsafe-but-conventional dummy `DeviceId` winit
    /// expects in tests. `DeviceId` has no public constructor, so
    /// `mem::zeroed` is the standard escape hatch other widget tests
    /// in this crate use (see `group_row.rs`).
    fn dummy_device_id() -> winit::event::DeviceId {
        // SAFETY: DeviceId is an opaque wrapper around a platform u64;
        // a zeroed value is what winit itself uses for the "no device"
        // sentinel on every backend. No fields require validity.
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn image_viewer_default_zoom_is_one() {
        let iv = ImageViewer::new(make_image());
        assert_eq!(iv.zoom(), 1.0);
        assert_eq!(iv.pan(), Vec2::ZERO);
    }

    #[test]
    fn image_viewer_scroll_up_zooms_in() {
        let mut iv = ImageViewer::new(make_image());
        // Stage a viewport rect by bypassing taffy — the layout pass
        // would otherwise need a parent.
        iv.rect = Rect::new(0.0, 0.0, 100.0, 100.0);

        let dev = dummy_device_id();
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus = None;
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::DEFAULT).unwrap();

        // Cursor moved into the centre of the rect first so the
        // wheel handler's `inside` check passes.
        iv.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(50.0, 50.0),
            },
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        // Positive y wheel delta → zoom in.
        iv.handle_event(
            &WindowEvent::MouseWheel {
                device_id: dev,
                delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        assert!(
            iv.zoom() > 1.0,
            "positive scroll y must increase zoom; got {}",
            iv.zoom()
        );
        assert!(handled, "wheel event over the viewer must be claimed");
    }

    #[test]
    fn image_viewer_drag_pans() {
        let mut iv = ImageViewer::new(make_image());
        // Apply layout indirectly by stuffing a rect into the field —
        // the wheel test above does the same and the public surface
        // doesn't need a setter for the rect (callers run through
        // taffy in production).
        iv.rect = Rect::new(0.0, 0.0, 200.0, 200.0);

        let dev = dummy_device_id();
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus = None;
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::DEFAULT).unwrap();

        // 1. Park cursor inside the rect.
        iv.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(50.0, 50.0),
            },
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        // 2. Press left button to start the drag.
        iv.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(
            iv.drag_origin.is_some(),
            "Pressed inside rect must start a drag"
        );

        // 3. Move cursor 30px right, 20px down. Pan must shift in
        // the same direction; the magnitude is delta / base_scale
        // (zoom is 1.0, so total_scale == base_scale).
        iv.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(80.0, 70.0),
            },
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        // 4. Release.
        iv.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
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

    #[test]
    fn image_viewer_fit_resets_zoom_and_pan() {
        let mut iv = ImageViewer::new(make_image());
        iv.set_zoom(2.0);
        iv.set_pan(Vec2::new(10.0, 10.0));
        iv.fit_to_viewport();
        assert_eq!(iv.zoom(), 1.0);
        assert_eq!(iv.pan(), Vec2::ZERO);
    }
}
