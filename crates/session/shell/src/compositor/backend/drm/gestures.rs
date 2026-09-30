//! Touchpad gestures, and who gets them.
//!
//! libinput reports swipe/pinch/hold on every touchpad, for two consumers: clients (a browser's
//! two-finger pinch-to-zoom, sent over `zwp_pointer_gestures_v1`) and the shell (three- and
//! four-finger gestures navigating the spatial model without the keyboard). A gesture cannot go
//! to both — a three-finger swipe that both switched workspace and scrolled the app underneath
//! is a bug either way. [`claims`] is the single place that decides: 1–2 fingers are the app's,
//! 3+ are the shell's, matching the convention every desktop trains users on.
//!
//! Each shell gesture maps onto an existing spatial-model interaction, with the same direction
//! convention as its keyboard chord. Three fingers work the plane you see and four go one level
//! out to workspaces, the split Super and Ctrl+Super make on the keyboard
//! (`docs/architecture/window-management.md`):
//!
//! | Gesture                   | Same as                                                   |
//! |---------------------------|-----------------------------------------------------------|
//! | 3-finger swipe left/right | directly pan the active strip                             |
//! | 3-finger swipe up / down  | Super+↑ / ↓ — the strip above / below                     |
//! | 4-finger swipe up / down  | Super+Page Up / Page Down — the previous / next workspace |
//! | 4-finger swipe left/right | nothing, deliberately                                     |
//! | 3+-finger pinch in / out  | Super+Tab — open / close the overview                     |
//!
//! Windows along a strip are the horizontal axis; strips and, one level out, workspaces the
//! vertical one. Inventing a third axis (a free 2-D pan) is exactly what the design rules out.
//! `natural_scroll` inverts both, since it's the same finger-follows-content preference as
//! regular scrolling.

/// The finger count at which a gesture stops belonging to the focused client and becomes shell navigation.
pub const SHELL_FINGER_THRESHOLD: u32 = 3;

/// How far the gesture centre must travel, in logical pixels, before a swipe commits — large enough that a rest-and-twitch doesn't switch workspaces, small enough that a flick lands before fingers leave the pad (roughly a third of usable travel).
pub const SWIPE_THRESHOLD: f64 = 100.0;

/// A swipe must be this many times longer on its dominant axis than the other before it counts as horizontal or vertical — otherwise a diagonal drag fires whichever axis crosses first, so the same motion means different things on different attempts.
pub const AXIS_DOMINANCE: f64 = 1.5;

/// Absolute pinch scale below which a pinch reads as "zoom out".
pub const PINCH_IN_SCALE: f64 = 0.8;
/// Absolute pinch scale above which a pinch reads as "zoom in".
pub const PINCH_OUT_SCALE: f64 = 1.25;

/// What the shell should do about a gesture. Every variant maps onto an existing chrome operation; none is a new interaction.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GestureAction {
    /// `chrome_state::switch_strip` — `-1` is the strip above, `+1` below.
    SwitchStrip(i32),
    /// `chrome_state::switch_workspace` — `-1` is up the wheel, `+1` down.
    SwitchWorkspace(i32),
    /// Total signed direct-pan travel since gesture begin. Positive travel
    /// moves toward later groups; every update is emitted after axis lock.
    StripPanUpdate { travel: f64, began: bool },
    /// The direct-pan stream ended or was cancelled. The row decides whether
    /// an elastic overpull needs to settle back to its legal boundary.
    StripPanEnd { cancelled: bool },
    /// Open the workspace overview (what `Super+Tab` toggles on).
    ShowOverview,
    /// Close it.
    HideOverview,
}

/// Whether the shell takes a gesture with this many fingers, leaving the rest for the focused client.
pub fn claims(fingers: u32) -> bool {
    fingers >= SHELL_FINGER_THRESHOLD
}

/// Tuning the tracker reads. Split out so the direction preference can follow the user's `natural_scroll` setting without the tracker knowing where settings come from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GestureConfig {
    /// Invert both swipe axes, matching two-finger scrolling.
    pub natural_scroll: bool,
    pub swipe_threshold: f64,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            swipe_threshold: SWIPE_THRESHOLD,
        }
    }
}

/// Accumulates one gesture and decides, once, what it meant — a gesture is a stream (begin,
/// updates, end) and the action must fire exactly once inside it: firing per update would switch five workspaces on one flick, firing only at `end` would feel a gesture behind the hand.
#[derive(Clone, Debug, Default)]
pub struct GestureTracker {
    config: GestureConfig,
    /// Fingers of the gesture in flight, `None` between gestures.
    fingers: Option<u32>,
    /// Accumulated swipe delta since `swipe_begin`.
    delta: (f64, f64),
    /// Set once this gesture has produced an action.
    fired: bool,
    /// Once horizontal motion wins arbitration, keep the strip attached to
    /// the fingers through reversals and momentary diagonal jitter.
    strip_pan_engaged: bool,
}

impl GestureTracker {
    pub fn new(config: GestureConfig) -> Self {
        Self {
            config,
            fingers: None,
            delta: (0.0, 0.0),
            fired: false,
            strip_pan_engaged: false,
        }
    }

    /// Re-read the direction preference. Called when settings change mid-session, so an in-flight gesture keeps the convention it began with rather than reversing under the finger.
    pub fn set_config(&mut self, config: GestureConfig) {
        self.config = config;
    }

    pub fn config(&self) -> GestureConfig {
        self.config
    }

    pub fn active_fingers(&self) -> Option<u32> {
        self.fingers
    }

    /// True while a gesture the *shell* owns is in flight — the signal the event loop uses to keep forwarding it away from clients even though finger count is only reported on `begin`.
    pub fn shell_owns_current(&self) -> bool {
        self.fingers.is_some_and(claims)
    }

    pub fn swipe_begin(&mut self, fingers: u32) {
        self.fingers = Some(fingers);
        self.delta = (0.0, 0.0);
        self.fired = false;
        self.strip_pan_engaged = false;
    }

    /// Feed one swipe update. Returns the action at the moment the threshold is crossed, and `None` for every update after it.
    pub fn swipe_update(&mut self, dx: f64, dy: f64) -> Option<GestureAction> {
        let fingers = self.fingers?;
        if !claims(fingers) || self.fired {
            return None;
        }
        self.delta.0 += dx;
        self.delta.1 += dy;
        let (horizontal, vertical) = self.delta;
        let (horizontal_magnitude, vertical_magnitude) = (horizontal.abs(), vertical.abs());
        // Horizontal strip navigation is direct manipulation rather than a
        // thresholded one-shot focus command. Axis-lock after a tiny movement
        // so the first visible frame carries the complete finger travel; once
        // locked, reversals remain on the same surface.
        if fingers == 3
            && (self.strip_pan_engaged
                || (horizontal_magnitude >= 8.0
                    && horizontal_magnitude >= vertical_magnitude * AXIS_DOMINANCE))
        {
            let began = !self.strip_pan_engaged;
            self.strip_pan_engaged = true;
            let travel = if self.config.natural_scroll {
                horizontal
            } else {
                -horizontal
            };
            return Some(GestureAction::StripPanUpdate { travel, began });
        }
        let action = swipe_action(fingers, self.delta, &self.config)?;
        self.fired = true;
        Some(action)
    }

    pub fn swipe_end(&mut self) -> Option<GestureAction> {
        self.swipe_end_with_cancel(false)
    }

    pub fn swipe_end_with_cancel(&mut self, cancelled: bool) -> Option<GestureAction> {
        let action = self
            .strip_pan_engaged
            .then_some(GestureAction::StripPanEnd { cancelled });
        self.fingers = None;
        self.delta = (0.0, 0.0);
        self.fired = false;
        self.strip_pan_engaged = false;
        action
    }

    pub fn pinch_begin(&mut self, fingers: u32) {
        self.fingers = Some(fingers);
        self.delta = (0.0, 0.0);
        self.fired = false;
        self.strip_pan_engaged = false;
    }

    /// Feed one pinch update. `scale` is absolute against the gesture's begin, so the thresholds are absolute too.
    pub fn pinch_update(&mut self, scale: f64) -> Option<GestureAction> {
        let fingers = self.fingers?;
        if !claims(fingers) || self.fired {
            return None;
        }
        let action = pinch_action(scale)?;
        self.fired = true;
        Some(action)
    }

    pub fn pinch_end(&mut self) {
        let _ = self.swipe_end();
    }
}

/// The action a swipe of `fingers` fingers and accumulated `delta` means, or `None` while still below threshold or too diagonal to read. Pure — the tracker only handles latching.
pub fn swipe_action(
    fingers: u32,
    delta: (f64, f64),
    config: &GestureConfig,
) -> Option<GestureAction> {
    let (dx, dy) = if config.natural_scroll {
        (-delta.0, -delta.1)
    } else {
        delta
    };
    let (ax, ay) = (dx.abs(), dy.abs());
    let t = config.swipe_threshold;
    // Vertical: strips (3 fingers) or, one level out, workspaces (4).
    if ay >= t && ay >= ax * AXIS_DOMINANCE {
        // Swiping up goes to the strip above and up the wheel, as Super+↑ and Super+Page Up do (dy is negative when fingers move up).
        let dir = if dy < 0.0 { -1 } else { 1 };
        return Some(match fingers {
            3 => GestureAction::SwitchStrip(dir),
            _ => GestureAction::SwitchWorkspace(dir),
        });
    }
    None
}

/// The action a pinch to absolute `scale` means. Pinching in is zooming out, which is what the overview is.
pub fn pinch_action(scale: f64) -> Option<GestureAction> {
    if scale <= PINCH_IN_SCALE {
        Some(GestureAction::ShowOverview)
    } else if scale >= PINCH_OUT_SCALE {
        Some(GestureAction::HideOverview)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GestureConfig {
        GestureConfig::default()
    }

    fn natural() -> GestureConfig {
        GestureConfig {
            natural_scroll: true,
            ..GestureConfig::default()
        }
    }

    // ownership

    #[test]
    fn gesture_ownership_is_decided_by_finger_count() {
        // Phase `one_and_two_finger_gestures_belong_to_the_client`.
        // Pinch-to-zoom in a browser must keep working; the shell taking two-finger gestures would break every app that uses them.
        assert!(!claims(1));
        assert!(!claims(2));
        // Phase `three_and_four_finger_gestures_belong_to_the_shell`.
        assert!(claims(3));
        assert!(claims(4));
        assert!(claims(5));
        // Phase `a_two_finger_swipe_never_produces_a_shell_action`.
        {
            let mut t = GestureTracker::new(cfg());
            t.swipe_begin(2);
            assert_eq!(t.swipe_update(0.0, -500.0), None);
            assert!(
                !t.shell_owns_current(),
                "a two-finger gesture must stay the client's for its whole duration"
            );
        }
        // Phase `the_shell_owns_the_whole_duration_of_a_three_finger_gesture`.
        {
            // The finger count only arrives on `begin`, so the loop needs the in-flight answer to route the updates that follow.
            let mut t = GestureTracker::new(cfg());
            t.swipe_begin(3);
            assert!(t.shell_owns_current());
            t.swipe_update(0.0, -150.0);
            assert!(t.shell_owns_current(), "still ours after it has fired");
            t.swipe_end();
            assert!(!t.shell_owns_current());
        }
        // Phase `a_two_finger_pinch_is_left_to_the_client`.
        {
            // Browser zoom.
            let mut t = GestureTracker::new(cfg());
            t.pinch_begin(2);
            assert_eq!(t.pinch_update(0.3), None);
        }
    }

    // swipe direction policy

    #[test]
    fn swipes_resolve_by_axis_threshold_and_finger_count() {
        // Phase `a_three_finger_swipe_up_focuses_the_strip_above_as_the_chord_does`.
        // Super+↑ is `switch_strip(-1)`. Swiping up must agree, or the two ways of doing the same thing disagree about which way is up.
        assert_eq!(
            swipe_action(3, (0.0, -150.0), &cfg()),
            Some(GestureAction::SwitchStrip(-1))
        );
        assert_eq!(
            swipe_action(3, (0.0, 150.0), &cfg()),
            Some(GestureAction::SwitchStrip(1))
        );
        // Phase `threshold_resolver_leaves_horizontal_motion_to_the_streaming_tracker`.
        assert_eq!(swipe_action(3, (-150.0, 0.0), &cfg()), None);
        assert_eq!(swipe_action(3, (150.0, 0.0), &cfg()), None);
        // Phase `a_four_finger_swipe_switches_workspace_the_same_way_the_chord_does`.
        // Four fingers are one level out, as Ctrl+Super is: Super+Page Up is `switch_workspace(-1)`.
        assert_eq!(
            swipe_action(4, (0.0, -150.0), &cfg()),
            Some(GestureAction::SwitchWorkspace(-1))
        );
        assert_eq!(
            swipe_action(4, (0.0, 150.0), &cfg()),
            Some(GestureAction::SwitchWorkspace(1))
        );
        // Phase `a_four_finger_horizontal_swipe_does_nothing`.
        // Deliberately unassigned: there's no third navigable axis in the spatial model, and inventing one is what the design rules out.
        assert_eq!(swipe_action(4, (-300.0, 0.0), &cfg()), None);
        // Phase `a_swipe_below_threshold_does_nothing`.
        // Three fingers resting on the pad drift. That must not switch a workspace.
        assert_eq!(swipe_action(3, (0.0, -30.0), &cfg()), None);
        assert_eq!(swipe_action(3, (20.0, -20.0), &cfg()), None);
        // Phase `a_diagonal_swipe_does_nothing_until_one_axis_dominates`.
        // Equal on both axes: ambiguous, and picking one would make the same motion do different things on different attempts.
        assert_eq!(swipe_action(3, (150.0, -150.0), &cfg()), None);
        // Clearly vertical now.
        assert_eq!(
            swipe_action(3, (60.0, -150.0), &cfg()),
            Some(GestureAction::SwitchStrip(-1))
        );
    }

    #[test]
    fn horizontal_streaming_and_natural_scroll_direction() {
        // Phase `horizontal_three_finger_motion_streams_total_travel_and_reverses`.
        {
            let mut tracker = GestureTracker::new(cfg());
            tracker.swipe_begin(3);
            assert_eq!(tracker.swipe_update(-4.0, 0.0), None);
            assert_eq!(
                tracker.swipe_update(-8.0, 1.0),
                Some(GestureAction::StripPanUpdate {
                    travel: 12.0,
                    began: true,
                })
            );
            assert_eq!(
                tracker.swipe_update(-20.0, 40.0),
                Some(GestureAction::StripPanUpdate {
                    travel: 32.0,
                    began: false,
                })
            );
            assert_eq!(
                tracker.swipe_update(24.0, 0.0),
                Some(GestureAction::StripPanUpdate {
                    travel: 8.0,
                    began: false,
                })
            );
            assert_eq!(
                tracker.swipe_end(),
                Some(GestureAction::StripPanEnd { cancelled: false })
            );
        }
        // Phase `natural_scroll_reverses_direct_strip_travel`.
        {
            let mut tracker = GestureTracker::new(natural());
            tracker.swipe_begin(3);
            assert_eq!(
                tracker.swipe_update(-16.0, 0.0),
                Some(GestureAction::StripPanUpdate {
                    travel: -16.0,
                    began: true,
                })
            );
            assert_eq!(
                tracker.swipe_end_with_cancel(true),
                Some(GestureAction::StripPanEnd { cancelled: true })
            );
        }
        // Phase `natural_scroll_inverts_both_axes`.
        assert_eq!(
            swipe_action(3, (0.0, -150.0), &natural()),
            Some(GestureAction::SwitchStrip(1))
        );
        assert_eq!(
            swipe_action(4, (0.0, 150.0), &natural()),
            Some(GestureAction::SwitchWorkspace(-1))
        );
        // Phase `changing_the_direction_preference_takes_effect_on_the_next_gesture`.
        {
            let mut t = GestureTracker::new(cfg());
            t.swipe_begin(3);
            assert_eq!(
                t.swipe_update(0.0, -150.0),
                Some(GestureAction::SwitchStrip(-1))
            );
            t.swipe_end();
            t.set_config(natural());
            t.swipe_begin(3);
            assert_eq!(
                t.swipe_update(0.0, -150.0),
                Some(GestureAction::SwitchStrip(1))
            );
        }
    }

    // pinch policy

    #[test]
    fn pinches_toggle_the_overview_once_past_threshold() {
        // Phase `pinching_in_shows_the_overview_and_out_hides_it`.
        assert_eq!(pinch_action(0.5), Some(GestureAction::ShowOverview));
        assert_eq!(pinch_action(2.0), Some(GestureAction::HideOverview));
        // Phase `a_small_pinch_does_nothing`.
        assert_eq!(pinch_action(1.0), None);
        assert_eq!(pinch_action(0.95), None);
        assert_eq!(pinch_action(1.1), None);
        // Phase `a_pinch_fires_once_and_resets`.
        {
            let mut t = GestureTracker::new(cfg());
            t.pinch_begin(3);
            assert_eq!(t.pinch_update(0.9), None);
            assert_eq!(t.pinch_update(0.6), Some(GestureAction::ShowOverview));
            assert_eq!(
                t.pinch_update(0.4),
                None,
                "latched for the rest of the pinch"
            );
            t.pinch_end();
            t.pinch_begin(3);
            assert_eq!(t.pinch_update(0.4), Some(GestureAction::ShowOverview));
        }
    }

    // tracker latching

    #[test]
    fn swipe_actions_fire_once_mid_gesture_and_rearm() {
        // Phase `a_single_flick_fires_exactly_once`.
        {
            // The bug this guards: firing per update would switch a workspace for every millimetre of a long swipe.
            let mut t = GestureTracker::new(cfg());
            t.swipe_begin(4);
            let mut fired = Vec::new();
            for _ in 0..20 {
                if let Some(a) = t.swipe_update(0.0, -20.0) {
                    fired.push(a);
                }
            }
            assert_eq!(fired, vec![GestureAction::SwitchWorkspace(-1)]);
        }
        // Phase `the_action_fires_mid_gesture_not_at_the_end`.
        {
            // Waiting for `end` makes the desktop feel a gesture behind.
            let mut t = GestureTracker::new(cfg());
            t.swipe_begin(4);
            assert_eq!(t.swipe_update(0.0, -40.0), None, "still below threshold");
            assert_eq!(
                t.swipe_update(0.0, -70.0),
                Some(GestureAction::SwitchWorkspace(-1)),
                "the accumulated 110px must fire without waiting for the fingers to lift"
            );
        }
        // Phase `a_second_gesture_after_the_first_fires_again`.
        {
            let mut t = GestureTracker::new(cfg());
            t.swipe_begin(4);
            assert!(t.swipe_update(0.0, -150.0).is_some());
            t.swipe_end();
            t.swipe_begin(4);
            assert_eq!(
                t.swipe_update(0.0, -150.0),
                Some(GestureAction::SwitchWorkspace(-1)),
                "the latch must reset when the gesture ends"
            );
        }
        // Phase `updates_outside_a_gesture_are_ignored`.
        {
            // libinput can deliver an update after a cancelled end if the stream is torn; that must not accumulate into the next gesture.
            let mut t = GestureTracker::new(cfg());
            assert_eq!(t.swipe_update(0.0, -500.0), None);
            assert_eq!(t.active_fingers(), None);
        }
    }
}
