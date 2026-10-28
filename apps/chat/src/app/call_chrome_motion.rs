use ui::theme::Theme;

/// One sampled motion value owns the call huddle's participant reveal. The
/// panel silhouette, controls, hit regions, compositor presentation and native
/// window allocation all read this same value, so a content-driven resize can
/// never leave those layers a frame apart.
#[derive(Debug)]
pub(crate) struct CallChromeMotion {
    /// The participants disclosing (`ui::motion::Disclose`).
    transition: ui::motion::Presence,
    target_open: bool,
    initialized: bool,
    pub(crate) sampled: f32,
    pub(crate) animating: bool,
    /// Deterministic visual-review hook; production leaves this `None`.
    pinned: Option<f32>,
}

impl Default for CallChromeMotion {
    fn default() -> Self {
        Self {
            transition: ui::motion::Presence::opened(),
            target_open: true,
            initialized: false,
            sampled: 1.0,
            animating: false,
            pinned: None,
        }
    }
}

impl CallChromeMotion {
    pub(crate) fn reconcile(&mut self, open: bool, theme: &Theme) {
        if let Some(value) = self.pinned {
            self.sampled = value;
            self.animating = false;
            return;
        }
        if !self.initialized {
            self.transition = if open {
                ui::motion::Presence::opened()
            } else {
                ui::motion::Presence::closed()
            };
            self.target_open = open;
            self.initialized = true;
        } else if open != self.target_open {
            self.transition.show(
                open,
                ui::motion::Disclose::SPEC,
                theme.motion(),
                ui::tokens::MotionSource::Pointer,
                ui::time::now(),
            );
            self.target_open = open;
        }
        let now = ui::time::now();
        self.sampled = ui::motion::unit_opacity(self.transition.progress(now));
        self.animating = self.transition.is_moving(now);
    }

    pub(crate) fn reset(&mut self) {
        if self.pinned.is_none() {
            self.initialized = false;
            self.animating = false;
        }
    }

    pub(crate) fn pin_for_visual_review(&mut self, value: f32) {
        self.pinned = Some(value.clamp(0.0, 1.0));
        self.sampled = self.pinned.unwrap_or(0.0);
        self.animating = false;
    }
}

#[cfg(test)]
mod tests {
    use super::CallChromeMotion;

    #[test]
    fn call_chrome_motion_lands_without_animating_when_reduced_or_pinned() {
        // Phase: reduced_motion_snaps_the_same_geometry_driver_to_its_target.
        {
            let mut motion = CallChromeMotion::default();
            motion.reconcile(true, &ui::theme::Theme::dark());
            motion.reconcile(false, &ui::theme::Theme::dark().with_reduce_motion(true));
            assert_eq!(motion.sampled, 0.0);
            assert!(!motion.animating);
        }

        // Phase: visual_review_can_pin_a_true_mid_morph_geometry.
        {
            let mut motion = CallChromeMotion::default();
            motion.pin_for_visual_review(0.5);
            motion.reconcile(false, &ui::theme::Theme::dark());
            assert_eq!(motion.sampled, 0.5);
            assert!(!motion.animating);
        }
    }
}
