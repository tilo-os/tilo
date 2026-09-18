//! Viewport work is applied by redraw, and acknowledged only after presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Viewport {
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
}
impl Viewport {
    pub fn logical_size(self) -> (u32, u32) {
        (
            (self.width as f64 / self.dpr).max(1.0) as u32,
            (self.height as f64 / self.dpr).max(1.0) as u32,
        )
    }
}

pub(crate) struct ViewportState {
    requested: Viewport,
    applied: Viewport,
    presented: Option<Viewport>,
    outdated_reconfigured: bool,
}
impl ViewportState {
    pub fn new(viewport: Viewport) -> Self {
        Self {
            requested: viewport,
            applied: viewport,
            presented: None,
            outdated_reconfigured: false,
        }
    }
    pub fn request(&mut self, viewport: Viewport) -> bool {
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
            x * self.applied.width as f64 / self.requested.width as f64,
            y * self.applied.height as f64 / self.requested.height as f64,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
