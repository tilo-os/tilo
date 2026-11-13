//! Viewport work is applied by redraw, and acknowledged only after presentation.

/// Widest or tallest canvas backing store the renderer draws: the largest
/// texture side WebGPU guarantees on every adapter
/// (`maxTextureDimension2D`), and the one the device is opened with.
pub(crate) const MAX_SURFACE_SIDE: u32 = 8192;
/// The side of one of vello's bins, in pixels.
const BIN_SIDE: u32 = 256;
/// The bins vello draws in one pass: its binning stage tracks every bin of
/// the target in one 256-wide workgroup, and a target with more is drawn
/// with holes where the bins past them fall.
const MAX_BINS: u32 = 256;
/// Pixels one frame renders: all vello draws in one pass, so a 5K display
/// (a 2560 x 1440 window at 2x) draws at its full device resolution and a
/// full-screen 6K Retina canvas (20 million pixels) a little below it.
pub(crate) const MAX_SURFACE_PIXELS: u64 = (BIN_SIDE as u64).pow(2) * MAX_BINS as u64;
/// Device pixel ratio the renderer draws at, whatever the page zoom.
pub(crate) const MAX_DPR: f64 = 2.0;

/// Bounds on the canvas backing store.
#[derive(Clone, Copy, Debug, PartialEq)]
#[expect(clippy::struct_field_names, reason = "every field is a maximum")]
pub(crate) struct SurfaceCaps {
    pub max_side: u32,
    pub max_pixels: u64,
    pub max_dpr: f64,
}

impl SurfaceCaps {
    pub const STANDARD: Self = Self {
        max_side: MAX_SURFACE_SIDE,
        max_pixels: MAX_SURFACE_PIXELS,
        max_dpr: MAX_DPR,
    };
    /// What a mount draws within once its GPU failed at the standard caps
    /// (the device was lost or ran out of memory): a 4K frame, which every
    /// WebGPU adapter in use draws comfortably.
    pub const REDUCED: Self = Self {
        max_side: 4096,
        max_pixels: 3840 * 2160,
        max_dpr: MAX_DPR,
    };
    /// `WebKit`'s GPU process holds every surface and binding of a frame
    /// resident on top of a large fixed cost, under Safari's 600 MB limit.
    /// The scratch and atlas budgets carry most of that saving
    /// (`conservative_gpu`); the canvas keeps a laptop's full-screen window
    /// sharp at its Retina scale (a 14-inch panel is 3024 x 1964), and only
    /// larger external displays draw below it.
    pub const CONSERVATIVE: Self = Self {
        max_side: 3456,
        max_pixels: 3024 * 1964,
        max_dpr: MAX_DPR,
    };

    /// The caps a mount recovers with after its GPU failed at these: one
    /// step smaller, so a device that cannot hold a full-resolution frame
    /// keeps drawing, slightly softer, instead of failing again. The smaller
    /// caps stay as they are.
    #[must_use]
    pub fn after_gpu_failure(self) -> Self {
        if self == Self::STANDARD {
            Self::REDUCED
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Viewport {
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
}
impl Viewport {
    /// The viewport rendered for an observed canvas extent: the same logical
    /// size, with a backing store inside the surface caps. The browser scales
    /// a reduced backing store up to the canvas's CSS size.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "canvas sizes are small positive pixel counts, clamped to at least 1"
    )]
    #[cfg(test)]
    pub fn bounded(self) -> Self {
        self.bounded_by(SurfaceCaps::STANDARD)
    }

    /// [`Self::bounded`] within `caps`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "canvas sizes are small positive pixel counts, clamped to at least 1"
    )]
    pub fn bounded_by(self, caps: SurfaceCaps) -> Self {
        if self.width == 0 || self.height == 0 {
            return self;
        }
        let (width, height) = (f64::from(self.width), f64::from(self.height));
        let scale = (f64::from(caps.max_side) / width.max(height))
            .min((caps.max_pixels as f64 / (width * height)).sqrt())
            .min(if self.dpr > caps.max_dpr {
                caps.max_dpr / self.dpr
            } else {
                1.0
            })
            .min(1.0);
        let scale = scale.min(Self::one_pass_scale(width * scale, height * scale) * scale);
        if scale >= 1.0 {
            return self;
        }
        Self {
            width: ((width * scale).floor() as u32).max(1),
            height: ((height * scale).floor() as u32).max(1),
            dpr: self.dpr * scale,
        }
    }

    /// The largest scale, at most 1, at which a `width` x `height` target
    /// covers no more than [`MAX_BINS`] bins: of every split of the bins
    /// into columns and rows, the one that keeps the most of the target.
    fn one_pass_scale(width: f64, height: f64) -> f64 {
        let side = f64::from(BIN_SIDE);
        let bins = |length: f64| (length / side).ceil().max(1.0);
        if bins(width) * bins(height) <= f64::from(MAX_BINS) {
            return 1.0;
        }
        (1..=MAX_BINS)
            .map(|columns| {
                let rows = MAX_BINS.checked_div(columns).unwrap_or(0);
                (f64::from(columns) * side / width).min(f64::from(rows) * side / height)
            })
            .fold(0.0, f64::max)
            .min(1.0)
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "canvas sizes are small positive pixel counts, clamped to at least 1"
    )]
    pub fn logical_size(self) -> (u32, u32) {
        (
            (f64::from(self.width) / self.dpr).max(1.0) as u32,
            (f64::from(self.height) / self.dpr).max(1.0) as u32,
        )
    }
}

pub(crate) struct ViewportState {
    /// The canvas extent winit reports; pointer coordinates are relative to it.
    observed: (u32, u32),
    /// The device pixel ratio `observed` was measured at. Winit measures the
    /// canvas when its box changes and says nothing when only the ratio does
    /// (browser zoom, another display, an emulated scale), so the extent and
    /// the ratio can disagree between two of its reports.
    observed_scale: f64,
    caps: SurfaceCaps,
    requested: Viewport,
    applied: Viewport,
    presented: Option<Viewport>,
    outdated_reconfigured: bool,
}
impl ViewportState {
    /// `viewport` is the observed canvas extent; the state renders it bounded.
    #[cfg(test)]
    pub fn new(viewport: Viewport) -> Self {
        Self::with_caps(viewport, SurfaceCaps::STANDARD)
    }

    pub fn with_caps(viewport: Viewport, caps: SurfaceCaps) -> Self {
        let bounded = viewport.bounded_by(caps);
        Self {
            observed: (viewport.width, viewport.height),
            observed_scale: viewport.dpr,
            caps,
            requested: bounded,
            applied: bounded,
            presented: None,
            outdated_reconfigured: false,
        }
    }
    /// Draw within `caps` from the next request on.
    pub fn set_caps(&mut self, caps: SurfaceCaps) {
        self.caps = caps;
    }
    /// What the page draws within.
    pub fn caps(&self) -> SurfaceCaps {
        self.caps
    }
    /// Take the canvas as the window reports it now: `extent` is what winit
    /// last measured, in device pixels, and `scale` the ratio the page draws
    /// at now.
    ///
    /// Winit measures the canvas's box, and the box is in CSS pixels: a
    /// change of the device pixel ratio alone leaves the extent it reports at
    /// the old scale. Read as the extent of the new one, a phone's 780 pixels
    /// became a laptop's 780 CSS pixels. What stays across such a change is
    /// the box, so the extent at the new ratio is that box scaled, not the
    /// number of the old one.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "canvas sizes are small positive pixel counts, clamped to at least 1"
    )]
    #[allow(
        clippy::float_cmp,
        reason = "a ratio either changed or it did not; the browser reports a value, not a measurement"
    )]
    pub fn observe(&mut self, extent: (u32, u32), scale: f64) -> bool {
        let extent = if scale == self.observed_scale {
            extent
        } else {
            // The same box, drawn at another ratio.
            let ratio = scale / self.observed_scale;
            (
                (f64::from(self.observed.0) * ratio).round() as u32,
                (f64::from(self.observed.1) * ratio).round() as u32,
            )
        };
        self.observed = extent;
        self.observed_scale = scale;
        let viewport = Viewport {
            width: self.observed.0,
            height: self.observed.1,
            dpr: scale,
        };
        self.request(viewport)
    }
    /// Take `observed` as the viewport to draw: the canvas at the ratio it
    /// draws at, bounded by [`Self::caps`]. `true` when that is not what the
    /// last request draws.
    fn request(&mut self, observed: Viewport) -> bool {
        self.observed = (observed.width, observed.height);
        self.observed_scale = observed.dpr;
        let viewport = observed.bounded_by(self.caps);
        if self.requested == viewport {
            return false;
        }
        if (self.requested.width, self.requested.height) != (viewport.width, viewport.height) {
            self.outdated_reconfigured = false;
        }
        self.requested = viewport;
        true
    }
    pub fn surface_installed(&mut self) {
        self.presented = None;
        self.outdated_reconfigured = false;
    }
    pub fn requested(&self) -> Viewport {
        self.requested
    }
    pub fn applied(&self) -> Viewport {
        self.applied
    }
    pub fn needs_apply(&self) -> bool {
        self.requested != self.applied
    }
    pub fn did_apply(&mut self) {
        self.applied = self.requested;
    }
    pub fn apply_for_acquired_texture<T, E>(
        &mut self,
        texture: Option<T>,
        apply: impl FnOnce(Viewport) -> Result<(), E>,
    ) -> Result<Option<T>, E> {
        // A skipped surface acquisition must not move hit testing, caret/IME,
        // or semantic geometry away from the image still displayed by the canvas.
        let Some(texture) = texture else {
            return Ok(None);
        };
        if self.needs_apply() {
            apply(self.requested)?;
            self.did_apply();
        }
        Ok(Some(texture))
    }
    pub fn did_present(&mut self) {
        self.presented = Some(self.applied);
        self.outdated_reconfigured = false;
    }
    pub fn needs_present(&self) -> bool {
        !self.suspended() && self.presented != Some(self.requested)
    }
    pub fn suspended(&self) -> bool {
        self.requested.width == 0 || self.requested.height == 0
    }
    pub fn needs_surface_resize(&self, width: u32, height: u32) -> bool {
        !self.suspended() && (width, height) != (self.requested.width, self.requested.height)
    }
    // One recovery configuration per size/outage. Retry acquisition on later
    // display opportunities without clearing an unchanged canvas each time.
    pub fn recover_outdated(&mut self) -> bool {
        !std::mem::replace(&mut self.outdated_reconfigured, true)
    }
    // Winit coordinates use the current physical extent. Until redraw applies
    // it, the browser stretches the existing canvas to that extent. Route input
    // into the corresponding point of the layout still owned by FrameHarness.
    pub fn map_pointer(&self, x: f64, y: f64) -> (f64, f64) {
        if self.suspended() {
            return (x, y);
        }
        (
            x * f64::from(self.applied.width) / f64::from(self.observed.0),
            y * f64::from(self.applied.height) / f64::from(self.observed.1),
        )
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "a ratio the bounds leave alone is the exact value it was given"
)]
mod tests {
    use super::*;

    // Catches a Retina 6K full-screen canvas reaching the renderer at 20 MP.
    #[test]
    fn large_canvases_render_a_bounded_backing_store_at_the_same_logical_size() {
        let retina_6k = Viewport {
            width: 6016,
            height: 3384,
            dpr: 2.0,
        };
        let bounded = retina_6k.bounded();
        assert!(u64::from(bounded.width) * u64::from(bounded.height) <= MAX_SURFACE_PIXELS);
        assert!(bounded.width <= MAX_SURFACE_SIDE && bounded.height <= MAX_SURFACE_SIDE);
        let (lw, lh) = bounded.logical_size();
        assert!(
            lw.abs_diff(3008) <= 1 && lh.abs_diff(1692) <= 1,
            "{lw}x{lh}"
        );

        let zoomed = Viewport {
            width: 2000,
            height: 1000,
            dpr: 4.0,
        }
        .bounded();
        assert_eq!((zoomed.width, zoomed.height, zoomed.dpr), (1000, 500, 2.0));

        let strip = Viewport {
            width: 10000,
            height: 200,
            dpr: 1.0,
        }
        .bounded();
        assert_eq!(strip.width, MAX_SURFACE_SIDE);

        assert_eq!(INITIAL.bounded(), INITIAL);

        let conservative = retina_6k.bounded_by(SurfaceCaps::CONSERVATIVE);
        assert!(u64::from(conservative.width) * u64::from(conservative.height) <= 3024 * 1964);
        assert!(conservative.width <= 3456);
        let (lw, _) = conservative.logical_size();
        assert!(lw.abs_diff(3008) <= 2, "{lw}");

        // A MacBook's full-screen window keeps its Retina scale in Safari.
        let macbook = Viewport {
            width: 2880,
            height: 1800,
            dpr: 2.0,
        }
        .bounded_by(SurfaceCaps::CONSERVATIVE);
        assert_eq!(
            (macbook.width, macbook.height, macbook.dpr),
            (2880, 1800, 2.0)
        );
    }

    // Catches a 1440p window on a 2x display drawing at 1.5x and being
    // stretched by the browser: the whole desktop looked soft on a 5K screen.
    #[test]
    fn a_big_window_on_a_retina_display_draws_at_full_device_resolution() {
        let five_k = Viewport {
            width: 5120,
            height: 2880,
            dpr: 2.0,
        };
        assert_eq!(five_k.bounded(), five_k);
        let (lw, lh) = five_k.bounded().logical_size();
        assert_eq!((lw, lh), (2560, 1440));

        // A laptop's full-screen window too, as every 4K one before.
        for observed in [
            Viewport {
                width: 3024,
                height: 1964,
                dpr: 2.0,
            },
            Viewport {
                width: 3840,
                height: 2160,
                dpr: 1.5,
            },
        ] {
            assert_eq!(observed.bounded(), observed);
        }

        // Past what vello draws in one pass (a 16-inch MacBook's 2560 x 1600
        // at 2x is 260 bins), a frame draws just below the device scale
        // rather than with holes; no extent, however tall or wide, does.
        let sixteen_inch = Viewport {
            width: 5120,
            height: 3200,
            dpr: 2.0,
        }
        .bounded();
        assert!(sixteen_inch.dpr > 1.9, "{sixteen_inch:?}");
        for (width, height) in [
            (5120, 3200),
            (6016, 3384),
            (8000, 4000),
            (4100, 4100),
            (8192, 2100),
            (10000, 200),
            (300, 9000),
        ] {
            let bounded = Viewport {
                width,
                height,
                dpr: 2.0,
            }
            .bounded();
            assert!(
                bounded.width.div_ceil(BIN_SIDE) * bounded.height.div_ceil(BIN_SIDE) <= MAX_BINS,
                "{width}x{height} -> {bounded:?}"
            );
        }

        // A GPU that failed at that size recovers one step smaller, at the
        // same logical size; the smaller caps do not shrink further.
        let reduced = SurfaceCaps::STANDARD.after_gpu_failure();
        assert_eq!(reduced, SurfaceCaps::REDUCED);
        let fallback = five_k.bounded_by(reduced);
        assert!(u64::from(fallback.width) * u64::from(fallback.height) <= 3840 * 2160);
        assert!(fallback.dpr < 2.0 && fallback.dpr > 1.4, "{fallback:?}");
        let (lw, _) = fallback.logical_size();
        assert!(lw.abs_diff(2560) <= 1, "{lw}");
        assert_eq!(reduced.after_gpu_failure(), reduced);
        assert_eq!(
            SurfaceCaps::CONSERVATIVE.after_gpu_failure(),
            SurfaceCaps::CONSERVATIVE
        );

        // A state stepped down mid-life requests the smaller frame next.
        let mut state = ViewportState::new(five_k);
        assert_eq!(state.requested(), five_k);
        state.set_caps(reduced);
        assert!(state.observe((five_k.width, five_k.height), five_k.dpr));
        assert_eq!(state.requested(), fallback);
    }

    #[test]
    fn pointer_input_on_a_bounded_canvas_maps_onto_the_rendered_frame() {
        let observed = Viewport {
            width: 8000,
            height: 4000,
            dpr: 2.0,
        };
        let mut state = ViewportState::new(observed);
        state.did_present();
        let applied = state.applied();
        assert!(applied.width < observed.width);
        let (x, y) = state.map_pointer(4000.0, 2000.0);
        assert!((x / applied.dpr - 2000.0).abs() < 1.0, "{x}");
        assert!((y / applied.dpr - 1000.0).abs() < 1.0, "{y}");
        assert!(
            !state.observe((observed.width, observed.height), observed.dpr),
            "an unchanged extent is not a resize"
        );
    }
    const INITIAL: Viewport = Viewport {
        width: 800,
        height: 600,
        dpr: 1.0,
    };
    fn presented() -> ViewportState {
        let mut state = ViewportState::new(INITIAL);
        state.did_present();
        state
    }

    // Catches applying layout in resize dispatch instead of at the frame boundary.
    #[test]
    fn resize_burst_applies_only_the_latest_viewport_at_redraw() {
        let mut state = presented();
        state.observe((900, 700), 1.0);
        state.observe((1200, 900), 1.0);
        assert_eq!(state.applied(), INITIAL);
        assert!(state.needs_apply());
        assert_eq!(state.requested().logical_size(), (1200, 900));
        state.did_apply();
        assert_eq!(
            state.applied(),
            Viewport {
                width: 1200,
                height: 900,
                dpr: 1.0
            }
        );
        assert!(state.needs_present());
        state.did_present();
        assert!(!state.needs_present());
    }

    #[test]
    fn duplicate_notifications_do_not_schedule_or_reconfigure() {
        let mut state = presented();
        assert!(!state.observe((800, 600), 1.0));
        assert!(!state.needs_apply());
        assert!(!state.needs_surface_resize(800, 600));
        assert!(!state.needs_present());
    }

    // Catches a phone's desk laid out at its Retina pixel width read as CSS
    // pixels: a change of density alone draws the same box at the new one,
    // and the frame follows the density.
    #[test]
    fn a_density_change_alone_keeps_the_layout_box_and_moves_the_extent() {
        let mut state = presented();
        // The browser draws at 2 now, and winit still reports the extent it
        // measured at 1: 800 x 600 CSS pixels.
        assert!(state.observe((800, 600), 2.0));
        assert_eq!(
            state.requested(),
            Viewport {
                width: 1600,
                height: 1200,
                dpr: 2.0
            }
        );
        assert_eq!(state.requested().logical_size(), (800, 600), "the same box");
        assert!(state.needs_apply());
        assert!(state.needs_surface_resize(800, 600), "the frame follows");
        state.did_apply();
        assert!(state.needs_present());

        // Back to 1: the box is the box, whatever extent was measured last.
        assert!(state.observe((1600, 1200), 1.0));
        assert_eq!(state.requested(), INITIAL);
        // Input still lands where the frame drawn at 2 stands, until redraw.
        assert_eq!(state.map_pointer(400.0, 300.0), (800.0, 600.0));

        // A box that really changed is taken as reported, at its own density.
        assert!(state.observe((1200, 900), 1.0));
        assert_eq!(
            state.requested(),
            Viewport {
                width: 1200,
                height: 900,
                dpr: 1.0
            }
        );
        assert!(!state.observe((1200, 900), 1.0), "the same report twice");

        // A fractional density — browser zoom — scales the box too.
        assert!(state.observe((1200, 900), 1.25));
        assert_eq!(
            state.requested(),
            Viewport {
                width: 1500,
                height: 1125,
                dpr: 1.25
            }
        );
        assert_eq!(state.requested().logical_size(), (1200, 900));
    }

    #[test]
    fn zero_size_stops_frame_scheduling_and_restore_keeps_the_last_frame() {
        let mut state = presented();
        state.observe((0, 600), 1.0);
        assert!(state.suspended());
        assert!(!state.needs_present());
        assert!(!state.needs_surface_resize(800, 600));
        assert_eq!(state.applied(), INITIAL);
        state.observe((800, 600), 1.0);
        assert!(!state.suspended());
        assert!(!state.needs_surface_resize(800, 600));
        state.observe((1000, 600), 1.0);
        assert!(state.needs_present());
        assert!(state.needs_surface_resize(800, 600));
    }

    #[test]
    fn skipped_presentations_keep_latest_resize_pending_without_reconfigure_loop() {
        let mut state = presented();
        state.observe((1000, 600), 1.0);
        state.did_apply();
        assert!(state.needs_present()); // Timeout: no acknowledgement.
        assert!(!state.needs_surface_resize(1000, 600));
        assert!(state.recover_outdated());
        assert!(!state.recover_outdated()); // Repeated Outdated: same configured size.
        assert!(state.needs_present());
        state.observe((1200, 600), 1.0);
        state.did_apply();
        assert!(state.needs_surface_resize(1000, 600));
        assert!(state.recover_outdated());
        state.did_present();
        assert!(!state.needs_present());
        assert!(state.recover_outdated()); // A later independent outage can recover.
    }

    #[test]
    fn skipped_acquisition_preserves_displayed_dpr_layout_and_input_until_success() {
        let initial = Viewport {
            width: 1000,
            height: 750,
            dpr: 1.25,
        };
        let mut state = ViewportState::new(initial);
        state.did_present();
        // Another display of twice the density, the box unchanged.
        state.observe((1000, 750), 2.0);
        assert!(state.needs_apply());
        let mut frame_viewport = initial;
        for _ in 0..3 {
            // Timeout/Outdated/Occluded all skip acquisition. The production
            // transaction must leave FrameHarness's layout and input scale alone.
            let result = state
                .apply_for_acquired_texture(None::<()>, |viewport| {
                    frame_viewport = viewport;
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert!(result.is_none());
            assert_eq!(frame_viewport, initial);
            assert_eq!(state.applied(), initial);
            // A pointer at the new extent's corner lands on the corner of the
            // frame still shown.
            assert_eq!(state.map_pointer(1600.0, 1200.0), (1000.0, 750.0));
            assert_eq!(frame_viewport.logical_size(), (800, 600));
            assert!(state.needs_present());
        }
        let result = state
            .apply_for_acquired_texture(Some(()), |viewport| {
                frame_viewport = viewport;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(result.is_some());
        assert_eq!(frame_viewport.logical_size(), (800, 600));
        assert_eq!(state.applied().dpr, 2.0);
        assert_eq!(state.map_pointer(1600.0, 1200.0), (1600.0, 1200.0));
        assert!(state.needs_present());
        state.did_present();
        assert!(!state.needs_present());
    }

    #[test]
    fn replacement_surface_requires_a_frame_and_can_recover_its_own_outage() {
        let mut state = presented();
        assert!(state.recover_outdated());
        state.surface_installed();
        assert!(state.needs_present());
        assert!(!state.needs_surface_resize(800, 600));
        assert!(state.recover_outdated());
        state.did_present();
        assert!(!state.needs_present());
    }

    #[test]
    fn pointer_and_touch_follow_the_scaled_applied_frame_until_redraw() {
        let mut state = ViewportState::new(Viewport {
            width: 1000,
            height: 750,
            dpr: 1.25,
        });
        state.did_present();
        state.observe((1500, 1125), 1.25);
        assert_eq!(state.map_pointer(750.0, 450.0), (500.0, 300.0));
        assert_eq!(state.applied().logical_size(), (800, 600));
        state.did_apply();
        state.did_present();
        assert_eq!(state.map_pointer(750.0, 450.0), (750.0, 450.0));
        assert_eq!(state.applied().logical_size(), (1200, 900));
    }
}
