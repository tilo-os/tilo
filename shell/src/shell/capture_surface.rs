//! Retained image content for compositor-owned capture surfaces.
//!
//! The overlay host owns window placement and event ordering. This module owns
//! the image handoff itself: live multi-region motion, pinned-image crossfades,
//! and scrollable tall-document presentation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Image, ImageFit, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{MouseScrollDelta, WindowEvent};

pub(crate) struct CaptureMirrorHandoffLayer {
    pub rgba: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// Normalized position inside the selected source union.
    pub source: Rect,
    /// Normalized position inside the final Grid/Stack composition.
    pub target: Rect,
}

pub(crate) struct CaptureSurfaceRequest<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub rgba: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub spawn_from: Option<Rect>,
    pub kind: CaptureSurfaceKind,
}

pub(crate) enum CaptureSurfaceKind {
    Live {
        preferred_size: Option<vello::kurbo::Size>,
        handoff_layers: Vec<CaptureMirrorHandoffLayer>,
    },
    Pinned {
        preview: ui::widget::CapturePreview,
    },
}

struct CaptureMirrorLayer {
    image: Image,
    source: Rect,
    target: Rect,
}

/// A compositor-fed image that always occupies its floating window's content
/// box. The general `Image` widget is intrinsically sized; capture mirrors are
/// viewport content and therefore need explicit fill semantics.
pub(super) struct CaptureMirrorImage {
    image: Image,
    /// Independently movable source regions. Keeping them separate lets a
    /// Grid/Stack `PiP` pack itself during the flight without crossfading or
    /// teleporting high-contrast pixels.
    handoff_layers: Vec<CaptureMirrorLayer>,
    /// Pinned stills use their receipt-scale preview as a calm launch proxy,
    /// then resolve into the exact artifact as the surface becomes readable.
    /// Multi-source live `PiP` uses spatial layer motion instead.
    crossfade_handoff: bool,
    /// Original pixel aspect for tall frozen documents. `Some` switches the
    /// image from whole-artifact preview into a width-fitted scroll viewport
    /// as the handoff lands.
    document_aspect: Option<f64>,
    document_scroll_y: f64,
    handoff_started_at: Instant,
    handoff_progress_override: Option<f64>,
    rect: Rect,
}

impl CaptureMirrorImage {
    pub(super) fn new(image: Image, handoff_layers: Vec<CaptureMirrorHandoffLayer>) -> Self {
        Self {
            image,
            handoff_layers: handoff_layers
                .into_iter()
                .map(|layer| CaptureMirrorLayer {
                    image: Image::from_rgba(layer.rgba, layer.width, layer.height)
                        .with_fit(ImageFit::Fill),
                    source: layer.source,
                    target: layer.target,
                })
                .collect(),
            crossfade_handoff: false,
            document_aspect: None,
            document_scroll_y: 0.0,
            handoff_started_at: Instant::now(),
            handoff_progress_override: None,
            rect: Rect::ZERO,
        }
    }

    pub(super) fn new_crossfade(
        image: Image,
        preview: Image,
        document_aspect: Option<f64>,
    ) -> Self {
        Self {
            image,
            handoff_layers: vec![CaptureMirrorLayer {
                image: preview,
                source: Rect::new(0.0, 0.0, 1.0, 1.0),
                target: Rect::new(0.0, 0.0, 1.0, 1.0),
            }],
            crossfade_handoff: true,
            document_aspect,
            document_scroll_y: 0.0,
            handoff_started_at: Instant::now(),
            handoff_progress_override: None,
            rect: Rect::ZERO,
        }
    }

    pub(super) fn set_image(&mut self, image: Image) {
        // Preserve `rect`: the frame payload changes continuously, but its
        // layout does not. Replacing the whole widget here would zero its rect
        // after every compositor readback.
        self.image = image;
    }

    pub(super) fn set_handoff_progress(&mut self, permille: u16) {
        if !self.handoff_layers.is_empty() {
            self.handoff_progress_override = Some(f64::from(permille.min(1_000)) / 1_000.0);
        }
    }

    fn handoff_mix(&self) -> f64 {
        const HANDOFF_DURATION: Duration = Duration::from_millis(360);
        let raw = self.handoff_progress_override.unwrap_or_else(|| {
            (self.handoff_started_at.elapsed().as_secs_f64() / HANDOFF_DURATION.as_secs_f64())
                .clamp(0.0, 1.0)
        });
        raw * raw * (3.0 - 2.0 * raw)
    }

    fn painted_image_rect(&mut self, mix: f64) -> Rect {
        let Some(aspect) = self.document_aspect else {
            return self.rect;
        };
        let raw = ((mix - 0.14) / 0.86).clamp(0.0, 1.0);
        let unfold = raw * raw * (3.0 - 2.0 * raw);
        let document_height = self.rect.width() / aspect.max(0.05);
        let height = self.rect.height() + (document_height - self.rect.height()) * unfold;
        let max_scroll = (height - self.rect.height()).max(0.0);
        self.document_scroll_y = self.document_scroll_y.clamp(0.0, max_scroll);
        Rect::new(
            self.rect.x0,
            self.rect.y0 - self.document_scroll_y,
            self.rect.x1,
            self.rect.y0 - self.document_scroll_y + height,
        )
    }

    fn scroll_document_by(&mut self, movement: f64) -> bool {
        let Some(aspect) = self.document_aspect else {
            return false;
        };
        let document_height = self.rect.width() / aspect.max(0.05);
        let max_scroll = (document_height - self.rect.height()).max(0.0);
        if max_scroll <= 0.0 {
            return false;
        }
        self.document_scroll_y = (self.document_scroll_y + movement).clamp(0.0, max_scroll);
        true
    }

    fn draw_document_scrollbar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        mix: f64,
    ) {
        let Some(aspect) = self.document_aspect else {
            return;
        };
        let raw = ((mix - 0.58) / 0.42).clamp(0.0, 1.0);
        let alpha = f64_to_f32_lossy(raw * raw * (3.0 - 2.0 * raw) * 0.46);
        if alpha <= 0.001 {
            return;
        }
        let document_height = self.rect.width() / aspect.max(0.05);
        let max_scroll = (document_height - self.rect.height()).max(0.0);
        if max_scroll <= 0.0 {
            return;
        }
        let rail = Rect::new(
            self.rect.x1 - 5.0,
            self.rect.y0 + 10.0,
            self.rect.x1 - 2.0,
            self.rect.y1 - 10.0,
        );
        let thumb_height =
            (rail.height() * self.rect.height() / document_height).clamp(28.0, rail.height());
        let travel = (rail.height() - thumb_height).max(0.0);
        let y = rail.y0 + travel * (self.document_scroll_y / max_scroll);
        let thumb = Rect::new(rail.x0, y, rail.x1, y + thumb_height);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.text_dim.with_alpha_factor(alpha),
            None,
            &RoundedRect::from_rect(thumb, thumb.width() / 2.0),
        );
    }
}

impl Widget for CaptureMirrorImage {
    fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = match taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        }) {
            Ok(node) => node,
            Err(error) => {
                tracing::error!(%error, "capture mirror leaf allocation failed");
                NodeId::from(0_u64)
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        let mix = self.handoff_mix();
        if self.handoff_layers.is_empty() {
            self.image.paint_at(scene, transform, self.rect);
            return;
        }
        if self.crossfade_handoff {
            let target = self.painted_image_rect(mix);
            if mix >= 0.999 {
                self.image.paint_at(scene, transform, target);
                self.draw_document_scrollbar(scene, transform, theme, mix);
                return;
            }
            if let Some(layer) = self.handoff_layers.first_mut() {
                layer.image.paint_at(scene, transform, target);
            }
            if mix > 0.001 {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    f64_to_f32_lossy(mix),
                    transform,
                    &self.rect,
                );
                self.image.paint_at(scene, transform, target);
                scene.pop_layer();
            }
            self.draw_document_scrollbar(scene, transform, theme, mix);
            return;
        }
        if mix >= 0.999 {
            self.image.paint_at(scene, transform, self.rect);
            return;
        }
        for layer in &mut self.handoff_layers {
            let normalized = lerp_normalized_rect(layer.source, layer.target, mix);
            let target = Rect::new(
                self.rect.x0 + normalized.x0 * self.rect.width(),
                self.rect.y0 + normalized.y0 * self.rect.height(),
                self.rect.x0 + normalized.x1 * self.rect.width(),
                self.rect.y0 + normalized.y1 * self.rect.height(),
            );
            layer.image.paint_at(scene, transform, target);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        handled: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
        if self.document_aspect.is_none() {
            return;
        }
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return;
        };
        let movement = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 54.0,
            MouseScrollDelta::PixelDelta(position) => -position.y / scale_factor.max(0.1),
        };
        if self.scroll_document_by(movement) {
            *handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "CaptureMirrorImage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn lerp_normalized_rect(from: Rect, to: Rect, progress: f64) -> Rect {
    Rect::new(
        from.x0 + (to.x0 - from.x0) * progress,
        from.y0 + (to.y0 - from.y0) * progress,
        from.x1 + (to.x1 - from.x1) * progress,
        from.y1 + (to.y1 - from.y1) * progress,
    )
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space geometry stays well within f32 precision"
)]
fn f64_to_f32_lossy(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(alpha: u8) -> Arc<[u8]> {
        Arc::from(vec![20_u8, 30, 40, alpha])
    }

    #[test]
    fn tall_pinned_document_unfolds_to_full_width_and_scrolls() {
        let image = Image::from_rgba(pixel(255), 1, 1).with_fit(ImageFit::Fill);
        let preview = Image::from_rgba(pixel(255), 1, 1).with_fit(ImageFit::Fill);
        let mut mirror = CaptureMirrorImage::new_crossfade(image, preview, Some(524.0 / 3_021.0));
        mirror.rect = Rect::new(0.0, 0.0, 340.0, 300.0);
        mirror.set_handoff_progress(1_000);

        let painted = mirror.painted_image_rect(mirror.handoff_mix());
        assert_eq!(painted.width(), 340.0);
        assert!(painted.height() > 1_900.0, "full pixels fit the card width");
        assert!(mirror.crossfade_handoff);
        assert!(mirror.scroll_document_by(54.0));
        assert_eq!(mirror.document_scroll_y, 54.0);
    }

    #[test]
    fn rearranged_pip_holds_spatial_pixels_then_yields_to_live_layout() {
        let source = Rect::new(0.0, 0.0, 0.4, 0.5);
        let target = Rect::new(0.0, 0.0, 0.5, 1.0);
        let mut mirror = CaptureMirrorImage::new(
            Image::from_rgba(pixel(0), 1, 1),
            vec![CaptureMirrorHandoffLayer {
                rgba: pixel(0),
                width: 1,
                height: 1,
                source,
                target,
            }],
        );

        mirror.set_handoff_progress(0);
        assert_eq!(mirror.handoff_mix(), 0.0);
        mirror.set_handoff_progress(400);
        assert!((mirror.handoff_mix() - 0.352).abs() < 0.000_001);
        mirror.set_handoff_progress(600);
        assert!((mirror.handoff_mix() - 0.648).abs() < 0.000_001);
        mirror.set_handoff_progress(1_000);
        assert_eq!(mirror.handoff_mix(), 1.0);
        assert_eq!(lerp_normalized_rect(source, target, 0.0), source);
        assert_eq!(lerp_normalized_rect(source, target, 1.0), target);
    }

    #[test]
    fn live_frame_refresh_preserves_retained_layout() {
        let mut mirror = CaptureMirrorImage::new(Image::from_rgba(pixel(255), 1, 1), Vec::new());
        mirror.rect = Rect::new(20.0, 30.0, 220.0, 130.0);

        mirror.set_image(Image::from_rgba(pixel(128), 1, 1));

        assert_eq!(mirror.rect, Rect::new(20.0, 30.0, 220.0, 130.0));
    }
}
