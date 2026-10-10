//! Retained image content for compositor-owned capture surfaces.
//!
//! The overlay host owns window placement and event ordering. This module owns
//! the image handoff itself: live multi-region motion, pinned-image crossfades,
//! and scrollable tall-document presentation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

use crate::shell::i18n::t_args;

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

pub(super) struct PreparedCaptureSurface<'a> {
    pub(super) id: &'a str,
    pub(super) title: &'a str,
    pub(super) image: Image,
    pub(super) pixel_aspect: f64,
    pub(super) preferred_size: Option<vello::kurbo::Size>,
    pub(super) spawn_from: Option<Rect>,
    pub(super) handoff_layers: Vec<CaptureMirrorHandoffLayer>,
    pub(super) crossfade_preview: Option<Image>,
    pub(super) document_aspect: Option<f64>,
}

impl<'a> CaptureSurfaceRequest<'a> {
    pub(super) fn prepare(self) -> PreparedCaptureSurface<'a> {
        let Self {
            id,
            title,
            rgba,
            width,
            height,
            spawn_from,
            kind,
        } = self;
        let pixel_aspect = f64::from(width) / f64::from(height.max(1));
        let (image, preferred_size, handoff_layers, crossfade_preview, document_aspect) = match kind
        {
            CaptureSurfaceKind::Live {
                preferred_size,
                handoff_layers,
            } => (
                Image::from_rgba(rgba, width, height)
                    .with_fit(ImageFit::Contain)
                    .with_alt(t_args("capture-live-alt", &[("title", title.into())])),
                preferred_size,
                handoff_layers,
                None,
                None,
            ),
            CaptureSurfaceKind::Pinned { preview } => {
                let document_aspect = (pixel_aspect < 0.58).then_some(pixel_aspect);
                let fit = if document_aspect.is_some() {
                    ImageFit::Fill
                } else {
                    ImageFit::Contain
                };
                let image = Image::from_rgba(rgba, width, height)
                    .with_fit(fit)
                    .with_alt(t_args("capture-pinned-alt", &[("title", title.into())]));
                let preview_rgba: Vec<u8> = preview
                    .pixels
                    .into_iter()
                    .flat_map(std::iter::IntoIterator::into_iter)
                    .collect();
                let preview_image = Image::from_rgba(
                    Arc::from(preview_rgba),
                    u32::from(preview.columns.max(1)),
                    u32::from(preview.rows.max(1)),
                )
                .with_fit(ImageFit::Fill)
                .with_alt(t_args(
                    "capture-pinned-preview-alt",
                    &[("title", title.into())],
                ));
                (
                    image,
                    document_aspect.map(|_| vello::kurbo::Size::new(340.0, 420.0)),
                    Vec::new(),
                    Some(preview_image),
                    document_aspect,
                )
            }
        };
        PreparedCaptureSurface {
            id,
            title,
            image,
            pixel_aspect,
            preferred_size,
            spawn_from,
            handoff_layers,
            crossfade_preview,
            document_aspect,
        }
    }
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
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.document_aspect.is_none() {
            return;
        }
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return;
        };
        let movement = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 54.0,
            MouseScrollDelta::PixelDelta(position) => -position.y / ctx.scale_factor.max(0.1),
        };
        if self.scroll_document_by(movement) {
            *ctx.handled = true;
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

pub(super) fn capture_mirror_origin(
    bounds: Rect,
    source: Option<Rect>,
    window_size: vello::kurbo::Size,
) -> Point {
    let top = bounds.y0 + 52.0;
    let right = bounds.x1 - window_size.width - 16.0;
    let bottom = bounds.y1 - window_size.height - 16.0;
    let left = bounds.x0 + 16.0;
    let candidates = [
        Point::new(right, bottom),
        Point::new(right, top),
        Point::new(left, bottom),
        Point::new(left, top),
    ];
    source.map_or(candidates[0], |source| {
        candidates
            .into_iter()
            .enumerate()
            .min_by(|(ai, a), (bi, b)| {
                let a_rect = Rect::from_origin_size(*a, window_size);
                let b_rect = Rect::from_origin_size(*b, window_size);
                let a_overlap = a_rect.intersect(source).area().max(0.0);
                let b_overlap = b_rect.intersect(source).area().max(0.0);
                a_overlap.total_cmp(&b_overlap).then_with(|| ai.cmp(bi))
            })
            .map_or(candidates[0], |(_, origin)| origin)
    })
}

/// Keep the ownership handoff meaningful when its source lives on the
/// continuous desktop but wholly outside the physical output. Visible pixels
/// are used exactly; off-screen pixels collapse to a small edge portal on the
/// side where the source lives, so the resulting `PiP` arrives from its real
/// virtual direction instead of popping in at the destination.
pub(super) fn visible_capture_spawn(bounds: Rect, source: Option<Rect>) -> Option<Rect> {
    const PORTAL_WIDTH: f64 = 84.0;
    const PORTAL_HEIGHT: f64 = 52.0;
    const EDGE_INSET: f64 = 6.0;
    const TOP_SAFE: f64 = 48.0;

    let source = source.filter(|rect| rect.is_finite() && rect.area() > 1.0)?;
    let visible = source.intersect(bounds);
    if visible.width() > 1.0 && visible.height() > 1.0 {
        return Some(visible);
    }

    let center = source.center();
    let x = if center.x < bounds.x0 {
        bounds.x0 + EDGE_INSET
    } else if center.x > bounds.x1 {
        bounds.x1 - EDGE_INSET - PORTAL_WIDTH
    } else {
        (center.x - PORTAL_WIDTH / 2.0).clamp(
            bounds.x0 + EDGE_INSET,
            bounds.x1 - EDGE_INSET - PORTAL_WIDTH,
        )
    };
    let y = if center.y < bounds.y0 {
        bounds.y0 + TOP_SAFE
    } else if center.y > bounds.y1 {
        bounds.y1 - EDGE_INSET - PORTAL_HEIGHT
    } else {
        (center.y - PORTAL_HEIGHT / 2.0)
            .clamp(bounds.y0 + TOP_SAFE, bounds.y1 - EDGE_INSET - PORTAL_HEIGHT)
    };
    Some(Rect::new(x, y, x + PORTAL_WIDTH, y + PORTAL_HEIGHT))
}

pub(super) fn capture_mirror_initial_size(
    viewport: Rect,
    source_aspect: f64,
    preferred: Option<vello::kurbo::Size>,
) -> vello::kurbo::Size {
    let requested_width = preferred.map_or(420.0, |size| size.width);
    let width_cap = (viewport.width() * 0.44).clamp(300.0, 720.0);
    let width = requested_width.min(width_cap).clamp(240.0, 720.0);
    let aspect = preferred
        .filter(|size| size.width > 1.0 && size.height > 1.0)
        .map_or(source_aspect, |size| size.width / size.height)
        .max(0.1);
    let height_cap = (viewport.height() * 0.55).max(135.0);
    let height = (width / aspect).clamp(135.0, 520.0).min(height_cap);
    vello::kurbo::Size::new(width, height)
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
    fn capture_requests_keep_live_and_pinned_policies_and_retained_layout() {
        fn request_materialization_keeps_live_and_pinned_policies_distinct() {
            let spawn = Rect::new(40.0, 50.0, 140.0, 100.0);
            let live = CaptureSurfaceRequest {
                id: "live",
                title: "Live region",
                rgba: Arc::from(vec![0_u8; 8]),
                width: 2,
                height: 1,
                spawn_from: Some(spawn),
                kind: CaptureSurfaceKind::Live {
                    preferred_size: Some(vello::kurbo::Size::new(480.0, 270.0)),
                    handoff_layers: vec![CaptureMirrorHandoffLayer {
                        rgba: pixel(255),
                        width: 1,
                        height: 1,
                        source: Rect::new(0.0, 0.0, 1.0, 1.0),
                        target: Rect::new(0.0, 0.0, 1.0, 1.0),
                    }],
                },
            }
            .prepare();
            assert_eq!(live.pixel_aspect, 2.0);
            assert_eq!(live.spawn_from, Some(spawn));
            assert_eq!(
                live.preferred_size,
                Some(vello::kurbo::Size::new(480.0, 270.0))
            );
            assert_eq!(live.handoff_layers.len(), 1);
            assert!(live.crossfade_preview.is_none());
            assert!(live.document_aspect.is_none());

            let pinned = CaptureSurfaceRequest {
                id: "pinned",
                title: "Tall screenshot",
                rgba: Arc::from(vec![0_u8; 16]),
                width: 1,
                height: 4,
                spawn_from: None,
                kind: CaptureSurfaceKind::Pinned {
                    preview: ui::widget::CapturePreview {
                        columns: 1,
                        rows: 1,
                        pixels: vec![[0, 0, 0, 255]],
                    },
                },
            }
            .prepare();
            assert_eq!(pinned.pixel_aspect, 0.25);
            assert_eq!(pinned.document_aspect, Some(0.25));
            assert_eq!(
                pinned.preferred_size,
                Some(vello::kurbo::Size::new(340.0, 420.0))
            );
            assert!(pinned.handoff_layers.is_empty());
            assert!(pinned.crossfade_preview.is_some());
        }

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

        fn live_frame_refresh_preserves_retained_layout() {
            let mut mirror =
                CaptureMirrorImage::new(Image::from_rgba(pixel(255), 1, 1), Vec::new());
            mirror.rect = Rect::new(20.0, 30.0, 220.0, 130.0);

            mirror.set_image(Image::from_rgba(pixel(128), 1, 1));

            assert_eq!(mirror.rect, Rect::new(20.0, 30.0, 220.0, 130.0));
        }

        request_materialization_keeps_live_and_pinned_policies_distinct();
        rearranged_pip_holds_spatial_pixels_then_yields_to_live_layout();
        live_frame_refresh_preserves_retained_layout();
    }

    #[test]
    fn pip_placement_avoids_its_source_enters_from_its_edge_and_sizes_by_aspect() {
        fn pip_placement_avoids_its_source_and_keeps_edge_margin() {
            let bounds = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            let size = vello::kurbo::Size::new(420.0, 200.0);
            let bottom_right_source = Rect::new(900.0, 400.0, 1_580.0, 880.0);
            let origin = capture_mirror_origin(bounds, Some(bottom_right_source), size);
            let placed = Rect::from_origin_size(origin, size);
            assert_eq!(placed.x1, 1_584.0);
            assert_eq!(
                placed.y0, 52.0,
                "top-right should beat an obscured bottom-right"
            );
            assert_eq!(placed.intersect(bottom_right_source).area(), 0.0);
            assert_eq!(
                capture_mirror_origin(bounds, None, size),
                Point::new(1_164.0, 684.0)
            );
        }

        fn offscreen_pip_source_enters_from_its_virtual_desktop_edge() {
            let bounds = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            let left =
                visible_capture_spawn(bounds, Some(Rect::new(-1_200.0, 180.0, -200.0, 780.0)))
                    .unwrap_or(Rect::ZERO);
            assert_eq!(left.x0, 6.0);
            assert!(left.y0 >= 48.0 && left.y1 <= 894.0);

            let right =
                visible_capture_spawn(bounds, Some(Rect::new(1_900.0, 120.0, 2_900.0, 720.0)))
                    .unwrap_or(Rect::ZERO);
            assert_eq!(right.x1, 1_594.0);

            let partly_visible = Rect::new(1_500.0, 100.0, 1_900.0, 500.0);
            assert_eq!(
                visible_capture_spawn(bounds, Some(partly_visible)),
                Some(Rect::new(1_500.0, 100.0, 1_600.0, 500.0))
            );
        }

        fn initial_size_preserves_aspect_without_dominating_viewport() {
            let compact = Rect::new(0.0, 0.0, 800.0, 600.0);
            let size = capture_mirror_initial_size(compact, 16.0 / 9.0, None);
            assert_eq!(size.width, 352.0);
            assert_eq!(size.height, 198.0);
            assert!(size.width < compact.width() / 2.0);

            let preferred = capture_mirror_initial_size(
                compact,
                1.0,
                Some(vello::kurbo::Size::new(480.0, 270.0)),
            );
            assert_eq!(preferred.width, 352.0);
            assert_eq!(preferred.height, 198.0);

            let document = capture_mirror_initial_size(
                compact,
                524.0 / 3_021.0,
                Some(vello::kurbo::Size::new(340.0, 420.0)),
            );
            assert_eq!(document.width, 340.0);
            assert_eq!(document.height, 330.0);
        }

        fn tall_pinned_document_unfolds_to_full_width_and_scrolls() {
            let image = Image::from_rgba(pixel(255), 1, 1).with_fit(ImageFit::Fill);
            let preview = Image::from_rgba(pixel(255), 1, 1).with_fit(ImageFit::Fill);
            let mut mirror =
                CaptureMirrorImage::new_crossfade(image, preview, Some(524.0 / 3_021.0));
            mirror.rect = Rect::new(0.0, 0.0, 340.0, 300.0);
            mirror.set_handoff_progress(1_000);

            let painted = mirror.painted_image_rect(mirror.handoff_mix());
            assert_eq!(painted.width(), 340.0);
            assert!(painted.height() > 1_900.0, "full pixels fit the card width");
            assert!(mirror.crossfade_handoff);
            assert!(mirror.scroll_document_by(54.0));
            assert_eq!(mirror.document_scroll_y, 54.0);
        }

        pip_placement_avoids_its_source_and_keeps_edge_margin();
        offscreen_pip_source_enters_from_its_virtual_desktop_edge();
        initial_size_preserves_aspect_without_dominating_viewport();
        tall_pinned_document_unfolds_to_full_width_and_scrolls();
    }
}
