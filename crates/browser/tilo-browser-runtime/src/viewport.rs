//! Viewport work is applied by redraw, and acknowledged only after presentation.

/// Widest or tallest canvas backing store the renderer draws.
pub(crate) const MAX_SURFACE_SIDE: u32 = 4096;
/// Pixels one frame renders: a 4K screen. A full-screen canvas on a 6K
/// Retina display would otherwise be 20 million pixels per compute frame.
pub(crate) const MAX_SURFACE_PIXELS: u64 = 3840 * 2160;
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
    /// `WebKit`'s GPU process holds every surface and binding of a frame
    /// resident on top of a large fixed cost, under Safari's 600 MB limit.
    pub const CONSERVATIVE: Self = Self {
        max_side: 1920,
        max_pixels: 1920 * 1200,
        max_dpr: MAX_DPR,
    };
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
            });
        if scale >= 1.0 {
            return self;
        }
        Self {
            width: ((width * scale).floor() as u32).max(1),
            height: ((height * scale).floor() as u32).max(1),
            dpr: self.dpr * scale,
        }
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
            caps,
            requested: bounded,
            applied: bounded,
            presented: None,
            outdated_reconfigured: false,
        }
    }
    pub fn request(&mut self, observed: Viewport) -> bool {
        self.observed = (observed.width, observed.height);
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
        assert!(u64::from(conservative.width) * u64::from(conservative.height) <= 1920 * 1200);
        assert!(conservative.width <= 1920);
        let (lw, _) = conservative.logical_size();
        assert!(lw.abs_diff(3008) <= 2, "{lw}");
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
            !state.request(observed),
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
        state.request(Viewport {
            width: 900,
            height: 700,
            dpr: 1.25,
        });
        state.request(Viewport {
            width: 1200,
            height: 900,
            dpr: 1.5,
        });
        assert_eq!(state.applied(), INITIAL);
        assert!(state.needs_apply());
        assert_eq!(state.requested().logical_size(), (800, 600));
        state.did_apply();
        assert_eq!(
            state.applied(),
            Viewport {
                width: 1200,
                height: 900,
                dpr: 1.5
            }
        );
        assert!(state.needs_present());
        state.did_present();
        assert!(!state.needs_present());
    }

    #[test]
    fn duplicate_notifications_do_not_schedule_or_reconfigure() {
        let mut state = presented();
        assert!(!state.request(INITIAL));
        assert!(!state.needs_apply());
        assert!(!state.needs_surface_resize(800, 600));
        assert!(!state.needs_present());
    }

    #[test]
    fn dpr_only_change_updates_layout_without_reconfiguring_same_size() {
        let mut state = presented();
        assert!(state.request(Viewport {
            dpr: 1.25,
            ..INITIAL
        }));
        assert!(state.needs_apply());
        assert_eq!(state.requested().logical_size(), (640, 480));
        assert!(!state.needs_surface_resize(800, 600));
        state.did_apply();
        assert!(state.needs_present());
    }

    #[test]
    fn zero_size_stops_frame_scheduling_and_restore_keeps_the_last_frame() {
        let mut state = presented();
        state.request(Viewport {
            width: 0,
            ..INITIAL
        });
        assert!(state.suspended());
        assert!(!state.needs_present());
        assert!(!state.needs_surface_resize(800, 600));
        assert_eq!(state.applied(), INITIAL);
        state.request(INITIAL);
        assert!(!state.suspended());
        assert!(!state.needs_surface_resize(800, 600));
        state.request(Viewport {
            width: 1000,
            ..INITIAL
        });
        assert!(state.needs_present());
        assert!(state.needs_surface_resize(800, 600));
    }

    #[test]
    fn skipped_presentations_keep_latest_resize_pending_without_reconfigure_loop() {
        let mut state = presented();
        state.request(Viewport {
            width: 1000,
            ..INITIAL
        });
        state.did_apply();
        assert!(state.needs_present()); // Timeout: no acknowledgement.
        assert!(!state.needs_surface_resize(1000, 600));
        assert!(state.recover_outdated());
        assert!(!state.recover_outdated()); // Repeated Outdated: same configured size.
        assert!(state.needs_present());
        state.request(Viewport {
            width: 1200,
            ..INITIAL
        });
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
        state.request(Viewport {
            dpr: 2.0,
            ..initial
        });
        assert!(!state.needs_surface_resize(1000, 750));
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
            let pointer = state.map_pointer(500.0, 375.0);
            assert_eq!(
                (
                    pointer.0 / frame_viewport.dpr,
                    pointer.1 / frame_viewport.dpr
                ),
                (400.0, 300.0)
            );
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
        assert_eq!(frame_viewport.logical_size(), (500, 375));
        assert_eq!(state.applied().dpr, 2.0);
        let pointer = state.map_pointer(500.0, 375.0);
        assert_eq!(
            (
                pointer.0 / frame_viewport.dpr,
                pointer.1 / frame_viewport.dpr
            ),
            (250.0, 187.5)
        );
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
        state.request(Viewport {
            width: 1500,
            height: 900,
            dpr: 1.5,
        });
        assert_eq!(state.map_pointer(750.0, 450.0), (500.0, 375.0));
        assert_eq!(state.applied().logical_size(), (800, 600));
        state.did_apply();
        state.did_present();
        assert_eq!(state.map_pointer(750.0, 450.0), (750.0, 450.0));
        assert_eq!(state.applied().logical_size(), (1000, 600));
    }
}
