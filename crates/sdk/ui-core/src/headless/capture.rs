//! Captures of a headless frame: the laid-out scene rasterised to a PNG or
//! to RGBA rows, alone or under the desktop's window bar, at the capture's
//! device scale.

use vello::Scene;
use vello::kurbo::Affine;

use super::FrameHarness;

impl FrameHarness {
    /// Rasterise the most recently laid-out frame to a PNG over the window
    /// ground (`surfaces.content`), exactly as a live window clears before
    /// drawing. `--shots` writes this.
    ///
    /// `new`, input delivery, and `set_theme` all finish with `relayout`, so
    /// this never runs a separate screenshot-only widget pipeline. A browser
    /// page cannot read a frame back synchronously: there nothing is written.
    pub fn render_png(&self, out: &std::path::Path) {
        self.render_png_on(out, self.theme.tokens.colors.surfaces.content);
    }

    /// Rasterise the most recently laid-out frame over the window ground, as
    /// tightly packed RGBA8 rows `viewport.0` pixels wide. Empty in a browser
    /// page, which cannot read a frame back synchronously.
    pub fn render_rgba(&self) -> Vec<u8> {
        self.render_rgba_on(self.theme.tokens.colors.surfaces.content)
    }

    /// [`FrameHarness::render_rgba`] over a transparent background.
    pub fn render_rgba_transparent(&self) -> Vec<u8> {
        self.render_rgba_on(vello::peniko::Color::TRANSPARENT)
    }

    #[cfg(target_arch = "wasm32")]
    fn render_rgba_on(&self, _background: vello::peniko::Color) -> Vec<u8> {
        tracing::warn!("a browser page has no offscreen renderer; the frame is empty");
        Vec::new()
    }

    fn render_png_on(&self, out: &std::path::Path, background: vello::peniko::Color) {
        self.drain_component_wakes();
        self.write_scene_png(&self.scene, 0, background, out);
    }

    #[cfg(target_arch = "wasm32")]
    fn write_scene_png(
        &self,
        _scene: &Scene,
        _extra_rows: u32,
        _background: vello::peniko::Color,
        out: &std::path::Path,
    ) {
        tracing::warn!(path = %out.display(), "a browser page has no offscreen renderer; no PNG written");
    }

    /// Rasterise `scene` over the window ground to a PNG as wide as the
    /// frame and `extra_rows` device pixels taller than it.
    #[cfg(not(target_arch = "wasm32"))]
    fn write_scene_png(
        &self,
        scene: &Scene,
        extra_rows: u32,
        background: vello::peniko::Color,
        out: &std::path::Path,
    ) {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (w, h) = self.device_size();
        let mut renderer =
            crate::offscreen::OffscreenRenderer::new(w, h.saturating_add(extra_rows));
        renderer.set_backdrop(Some(self.backdrop.clone()));
        renderer.render_png(scene, background, out);
    }

    /// Write the `--shots` capture: the frame alone, or under the desktop's
    /// window bar when `window_bar` is set. Only the native launcher takes
    /// `--shots`.
    #[cfg(feature = "native")]
    pub(crate) fn write_shot(&mut self, out: &std::path::Path, window_bar: bool) {
        if window_bar {
            self.present_in_window_bar();
        }
        self.prepare_component_output();
        if window_bar {
            self.render_window_png(out);
        } else {
            self.render_png(out);
        }
    }

    /// Rasterise the most recently laid-out frame under the desktop's window
    /// bar ([`Self::present_in_window_bar`]) to a PNG as tall as the window:
    /// the bar, then the app's frame under it.
    pub fn render_window_png(&mut self, out: &std::path::Path) {
        self.drain_component_wakes();
        let scale = crate::offscreen::capture_device_scale();
        let bar = self.window_bar_height_px(self.viewport.0);
        let device_bar = bar.saturating_mul(scale);
        let transform = self.capture_draw_transform();
        let mut scene = Scene::new();
        self.paint_window_bar(&mut scene, transform, f64::from(self.viewport.0));
        scene.append(
            &self.scene,
            Some(Affine::translate((0.0, f64::from(device_bar)))),
        );
        self.paint_window_bar_top_layer(&mut scene, transform);
        self.write_scene_png(
            &scene,
            device_bar,
            self.theme.tokens.colors.surfaces.content,
            out,
        );
    }

    /// The transform frames are drawn with: the identity, or the capture's
    /// device scale ([`crate::offscreen::set_capture_device_scale`]).
    pub(crate) fn capture_draw_transform(&self) -> Affine {
        crate::scale::composite_transform(f64::from(crate::offscreen::capture_device_scale()))
    }

    /// The frame's size in device pixels: the viewport at the capture's
    /// device scale.
    ///
    /// # Panics
    ///
    /// Panics when a capture's device scale makes the frame larger than
    /// Vello renders in one pass (256 bins of 256 x 256 px): it would drop
    /// the bins past that and write a frame with holes.
    pub(super) fn device_size(&self) -> (u32, u32) {
        let scale = crate::offscreen::capture_device_scale();
        let size = (
            self.viewport.0.saturating_mul(scale),
            self.viewport.1.saturating_mul(scale),
        );
        assert!(
            scale == 1 || size.0.div_ceil(256).saturating_mul(size.1.div_ceil(256)) <= 256,
            "a {}x{} capture at device scale {scale} is {}x{} pixels, more than Vello renders \
             in one pass (256 bins of 256 x 256); pass a smaller --viewport (1280x768 fits at 4x)",
            self.viewport.0,
            self.viewport.1,
            size.0,
            size.1
        );
        size
    }
}

// Native capture owns synchronous GPU readback. Both the convenience path
// and reusable-renderer oracle share this existing platform boundary.
#[cfg(not(target_arch = "wasm32"))]
impl FrameHarness {
    fn render_rgba_on(&self, background: vello::peniko::Color) -> Vec<u8> {
        let (w, h) = self.device_size();
        let mut renderer = crate::offscreen::OffscreenRenderer::new(w, h);
        self.render_rgba_with(&mut renderer, background)
    }

    /// Rasterise the recorded frame and its captured backdrop with a caller-owned
    /// renderer. Pixel oracles can reuse a renderer with stable preprocessing
    /// without changing process-wide settings or the widget draw pipeline.
    ///
    /// # Panics
    /// Panics if the renderer dimensions differ from the capture's device size,
    /// or the GPU cannot produce the bounded consecutive valid submissions.
    #[must_use]
    pub fn render_rgba_with(
        &self,
        renderer: &mut crate::offscreen::OffscreenRenderer,
        background: vello::peniko::Color,
    ) -> Vec<u8> {
        self.drain_component_wakes();
        assert_eq!(
            (renderer.width, renderer.height),
            self.device_size(),
            "the renderer must match the actual capture dimensions"
        );
        renderer.set_backdrop(Some(self.backdrop.clone()));
        renderer.render_resilient(&self.scene, background)
    }
}
