//! Rendering the live window previews shell chrome shows.
//!
//! The switcher and the overview ask for previews through
//! [`crate::shell::overlay_host::ShellOverlayHost::window_preview_requests`],
//! which also owns the throttle, the turn order and the memory bound. This
//! renders each requested window into one small offscreen GPU target and
//! hands the pixels back:
//!
//! - a **client** window is drawn from its latest buffer in the frame store
//!   (already cut to the window's own geometry), scaled on the GPU, and only
//!   when that buffer changed since its last preview;
//! - a window the **shell** draws is painted through the same live path a
//!   screenshot of it takes ([`FrameBuilder::paint_capture_targets`]) and
//!   cropped to its window the same way.
//!
//! One renderer at the largest preview size serves every window; it exists
//! only while previews are wanted.

use std::sync::Arc;
use std::time::Instant;

use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{
    Blob, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality,
};

use crate::compositor::foreign::store;
use crate::compositor::frame::FrameBuilder;
use crate::shell::window_previews::{
    MAX_PREVIEW_HEIGHT, MAX_PREVIEW_WIDTH, RENDERS_PER_REFRESH, preview_size,
};
use tilo_desktop::window_switcher::PreviewSource;

/// What one refresh did, for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewRefresh {
    /// Requests the round went through.
    pub visited: usize,
    /// Previews actually rendered.
    pub rendered: usize,
}

/// Renders requested window previews into one small offscreen target.
#[derive(Default)]
pub struct WindowPreviewRenderer {
    renderer: Option<OffscreenRenderer>,
    /// Set when no GPU target could be made, so a machine without one does
    /// not retry every round; chrome keeps its app marks.
    unavailable: bool,
}

impl WindowPreviewRenderer {
    /// Run a refresh round if one is due at `now`. Does nothing, and frees
    /// the GPU target, while no chrome wants previews.
    pub fn refresh(&mut self, frame_builder: &mut FrameBuilder, now: Instant) -> PreviewRefresh {
        let requests = frame_builder.host_mut().window_preview_requests(now);
        if requests.is_empty() {
            if frame_builder.host().window_previews().is_empty() {
                self.renderer = None;
            }
            return PreviewRefresh::default();
        }
        let mut report = PreviewRefresh::default();
        for request in requests {
            if report.rendered >= RENDERS_PER_REFRESH {
                break;
            }
            report.visited = report.visited.saturating_add(1);
            let rendered = match &request.source {
                PreviewSource::Client { surface } => {
                    self.render_client(frame_builder, request.tab, *surface)
                }
                PreviewSource::Painted { capture_id } => {
                    self.render_painted(frame_builder, request.tab, capture_id)
                }
                PreviewSource::Withheld | PreviewSource::Unavailable => false,
            };
            if rendered {
                report.rendered = report.rendered.saturating_add(1);
            }
        }
        frame_builder
            .host_mut()
            .advance_window_previews(report.visited);
        report
    }

    fn render_client(&mut self, frame_builder: &mut FrameBuilder, tab: u64, surface: u64) -> bool {
        let seq = store().frame_seq(surface);
        if seq == 0 || frame_builder.host().window_preview_stamp(tab) == Some(seq) {
            return false;
        }
        let Some(frame) = store().snapshot(surface) else {
            return false;
        };
        let Some((width, height)) = preview_size(f64::from(frame.width), f64::from(frame.height))
        else {
            return false;
        };
        let image = ImageData {
            data: Blob::new(Arc::new(frame.rgba)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: frame.width,
            height: frame.height,
        };
        let brush = ImageBrush::new(image)
            .with_extend(Extend::Pad)
            .with_quality(ImageQuality::High);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::scale_non_uniform(
                f64::from(width) / f64::from(frame.width),
                f64::from(height) / f64::from(frame.height),
            ),
            &brush,
            None,
            &Rect::new(0.0, 0.0, f64::from(frame.width), f64::from(frame.height)),
        );
        self.finish(frame_builder, tab, &scene, (width, height), Some(frame.seq))
    }

    fn render_painted(
        &mut self,
        frame_builder: &mut FrameBuilder,
        tab: u64,
        capture_id: &str,
    ) -> bool {
        let (source, rects) = frame_builder.paint_capture_targets(
            &[capture_id.to_string()],
            ui::widget::CaptureExtent::FullApp,
            false,
        );
        let Some(window) = rects.first().copied().filter(|rect| rect.area() > 0.0) else {
            return false;
        };
        let Some((width, height)) = preview_size(window.width(), window.height()) else {
            return false;
        };
        let transform = Affine::translate((-window.x0, -window.y0)).then_scale_non_uniform(
            f64::from(width) / window.width(),
            f64::from(height) / window.height(),
        );
        let mut scene = Scene::new();
        crate::compositor::capture::append_clipped(&mut scene, &source, window, transform);
        self.finish(frame_builder, tab, &scene, (width, height), None)
    }

    /// Render `scene`, whose preview occupies the top-left `size`, and keep
    /// those pixels as the preview of `tab`.
    fn finish(
        &mut self,
        frame_builder: &mut FrameBuilder,
        tab: u64,
        scene: &Scene,
        (width, height): (u32, u32),
        stamp: Option<u64>,
    ) -> bool {
        let Some(renderer) = self.renderer() else {
            return false;
        };
        let full = renderer.render_rgba_resilient(scene);
        let Some(rgba) = crop_top_left(&full, renderer.width, width, height) else {
            return false;
        };
        frame_builder
            .host_mut()
            .store_window_preview(tab, Arc::from(rgba), width, height, stamp);
        true
    }

    fn renderer(&mut self) -> Option<&mut OffscreenRenderer> {
        if self.renderer.is_none() && !self.unavailable {
            match OffscreenRenderer::try_new(MAX_PREVIEW_WIDTH, MAX_PREVIEW_HEIGHT) {
                Ok(renderer) => self.renderer = Some(renderer),
                Err(error) => {
                    tracing::warn!(%error, "window previews unavailable: no offscreen target");
                    self.unavailable = true;
                }
            }
        }
        self.renderer.as_mut()
    }

    /// Whether a GPU target is currently held.
    pub fn holds_gpu_target(&self) -> bool {
        self.renderer.is_some()
    }
}

/// The top-left `width` × `height` pixels of an RGBA buffer `stride` pixels wide.
fn crop_top_left(rgba: &[u8], stride: u32, width: u32, height: u32) -> Option<Vec<u8>> {
    let stride = usize::try_from(stride).ok()?.checked_mul(4)?;
    let row = usize::try_from(width).ok()?.checked_mul(4)?;
    let rows = usize::try_from(height).ok()?;
    if row > stride {
        return None;
    }
    let mut out = Vec::with_capacity(row.checked_mul(rows)?);
    for y in 0..rows {
        let start = y.checked_mul(stride)?;
        out.extend_from_slice(rgba.get(start..start.checked_add(row)?)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_keeps_exactly_the_preview_rows() {
        let stride = 4_u32;
        let rgba: Vec<u8> = (0..(stride * 3 * 4))
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let cropped = crop_top_left(&rgba, stride, 2, 2).expect("fits");
        assert_eq!(cropped.len(), 2 * 2 * 4);
        assert_eq!(cropped.get(0..8), rgba.get(0..8));
        assert_eq!(cropped.get(8..16), rgba.get(16..24));
        assert!(crop_top_left(&rgba, stride, 5, 1).is_none());
        assert!(crop_top_left(&rgba, stride, 2, 9).is_none());
    }
}
