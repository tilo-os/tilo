//! Touchpad gestures, and who gets them.
//!
//! libinput reports swipe/pinch/hold on every touchpad, for two consumers: clients (a browser's
//! two-finger pinch-to-zoom, sent over `zwp_pointer_gestures_v1`) and the shell (three- and
//! four-finger gestures navigating the spatial model without the keyboard). A gesture cannot go
//! to both — a three-finger swipe that both switched workspace and scrolled the app underneath
//! is a bug either way. [`claims`] is the single place that decides: 1–2 fingers are the app's,
//! 3+ are the shell's, matching the convention every desktop trains users on.
//!
//! Each shell gesture maps onto an existing spatial-model interaction, with the same direction convention as its keyboard chord:
//!
//! | Gesture                  | Same as                    |
//! |--------------------------|----------------------------|
//! | 3-finger swipe up / down | Super+ArrowUp / `ArrowDown` — switch workspace |
//! | 3-finger swipe left/right| Super+ArrowLeft / `ArrowRight` — step along the strip |
//! | 4-finger swipe up / down | Ctrl+Tab — open / close the overview |
//! | 3+-finger pinch in / out | open / close the overview |
//!
//! Workspaces are the vertical axis, windows along a strip the horizontal one; inventing a
//! third axis (a free 2-D pan) is exactly what the design rules out. `natural_scroll` inverts
//! both, since it's the same finger-follows-content preference as regular scrolling.

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
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GestureAction {
    /// `chrome_state::switch_workspace` — `-1` is up the wheel, `+1` down.
    SwitchWorkspace(i32),
    /// `ShellOverlayHost::step_focus_along_strip` — `-1` is the window to the left, the same step the Super+Left/Right chord and a horizontal scroll make; a swipe is a third way to ask for it, not a fourth definition of it.
    StepAlongStrip(i32),
    /// Open the workspace overview (what `Ctrl+Tab` toggles on).
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
}

impl GestureTracker {
    pub fn new(config: GestureConfig) -> Self {
        Self {
            config,
            fingers: None,
            delta: (0.0, 0.0),
            fired: false,
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
    }

    /// Feed one swipe update. Returns the action at the moment the threshold is crossed, and `None` for every update after it.
    pub fn swipe_update(&mut self, dx: f64, dy: f64) -> Option<GestureAction> {
        let fingers = self.fingers?;
        if !claims(fingers) || self.fired {
            return None;
        }
        self.delta.0 += dx;
        self.delta.1 += dy;
        let action = swipe_action(fingers, self.delta, &self.config)?;
        self.fired = true;
        Some(action)
    }

    pub fn swipe_end(&mut self) {
        self.fingers = None;
        self.delta = (0.0, 0.0);
        self.fired = false;
    }

    pub fn pinch_begin(&mut self, fingers: u32) {
        self.fingers = Some(fingers);
        self.delta = (0.0, 0.0);
        self.fired = false;
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
        self.swipe_end();
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
    // Vertical: workspaces (3 fingers) or the overview (4).
    if ay >= t && ay >= ax * AXIS_DOMINANCE {
        // Swiping up moves *up* the wheel, the same direction Super+ArrowUp goes (dy is negative when fingers move up).
        let dir = if dy < 0.0 { -1 } else { 1 };
        return Some(match fingers {
            3 => GestureAction::SwitchWorkspace(dir),
            _ => {
                if dir < 0 {
                    GestureAction::ShowOverview
                } else {
                    GestureAction::HideOverview
                }
            }
        });
    }
    // Horizontal: the tab dimension of the active group, three fingers only — a four-finger horizontal swipe has no second axis to mean anything on.
    if ax >= t && ax >= ay * AXIS_DOMINANCE && fingers == 3 {
        let dir = if dx < 0.0 { 1 } else { -1 };
        return Some(GestureAction::StepAlongStrip(dir));
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
    fn one_and_two_finger_gestures_belong_to_the_client() {
        // Pinch-to-zoom in a browser must keep working; the shell taking two-finger gestures would break every app that uses them.
        assert!(!claims(1));
        assert!(!claims(2));
    }

    #[test]
    fn three_and_four_finger_gestures_belong_to_the_shell() {
        assert!(claims(3));
        assert!(claims(4));
        assert!(claims(5));
    }

    // swipe direction policy

    #[test]
    fn a_three_finger_swipe_up_switches_workspace_the_same_way_the_chord_does() {
        // Super+ArrowUp is `switch_workspace(-1)`. Swiping up must agree, or the two ways of doing the same thing disagree about which way is up.
        assert_eq!(
            swipe_action(3, (0.0, -150.0), &cfg()),
            Some(GestureAction::SwitchWorkspace(-1))
        );
        assert_eq!(
            swipe_action(3, (0.0, 150.0), &cfg()),
            Some(GestureAction::SwitchWorkspace(1))
        );
    }

    #[test]
    fn a_three_finger_swipe_sideways_travels_along_the_strip() {
        // Swiping left (dx negative) travels forward along the strip: the fingers move the way the content does, the same sense a two-finger horizontal scroll already has.
        assert_eq!(
            swipe_action(3, (-150.0, 0.0), &cfg()),
            Some(GestureAction::StepAlongStrip(1))
        );
        assert_eq!(
            swipe_action(3, (150.0, 0.0), &cfg()),
            Some(GestureAction::StepAlongStrip(-1))
        );
    }

    #[test]
    fn a_four_finger_swipe_opens_and_closes_the_overview() {
        assert_eq!(
            swipe_action(4, (0.0, -150.0), &cfg()),
            Some(GestureAction::ShowOverview)
        );
        assert_eq!(
            swipe_action(4, (0.0, 150.0), &cfg()),
            Some(GestureAction::HideOverview)
        );
    }

    #[test]
    fn a_four_finger_horizontal_swipe_does_nothing() {
        // Deliberately unassigned: there's no third navigable axis in the spatial model, and inventing one is what the design rules out.
        assert_eq!(swipe_action(4, (-300.0, 0.0), &cfg()), None);
    }

    #[test]
    fn natural_scroll_inverts_both_axes() {
        assert_eq!(
            swipe_action(3, (0.0, -150.0), &natural()),
            Some(GestureAction::SwitchWorkspace(1))
        );
        assert_eq!(
            swipe_action(3, (-150.0, 0.0), &natural()),
            Some(GestureAction::StepAlongStrip(-1))
        );
        assert_eq!(
            swipe_action(4, (0.0, 150.0), &natural()),
            Some(GestureAction::ShowOverview)
        );
    }

    #[test]
    fn a_swipe_below_threshold_does_nothing() {
        // Three fingers resting on the pad drift. That must not switch a workspace.
        assert_eq!(swipe_action(3, (0.0, -30.0), &cfg()), None);
        assert_eq!(swipe_action(3, (20.0, -20.0), &cfg()), None);
    }

    #[test]
    fn a_diagonal_swipe_does_nothing_until_one_axis_dominates() {
        // Equal on both axes: ambiguous, and picking one would make the same motion do different things on different attempts.
        assert_eq!(swipe_action(3, (150.0, -150.0), &cfg()), None);
        // Clearly vertical now.
        assert_eq!(
            swipe_action(3, (60.0, -150.0), &cfg()),
            Some(GestureAction::SwitchWorkspace(-1))
        );
    }

    // pinch policy

    #[test]
    fn pinching_in_shows_the_overview_and_out_hides_it() {
        assert_eq!(pinch_action(0.5), Some(GestureAction::ShowOverview));
        assert_eq!(pinch_action(2.0), Some(GestureAction::HideOverview));
    }

    #[test]
    fn a_small_pinch_does_nothing() {
        assert_eq!(pinch_action(1.0), None);
        assert_eq!(pinch_action(0.95), None);
        assert_eq!(pinch_action(1.1), None);
    }

    // tracker latching

    #[test]
    fn a_single_flick_fires_exactly_once() {
        // The bug this guards: firing per update would switch a workspace for every millimetre of a long swipe.
        let mut t = GestureTracker::new(cfg());
        t.swipe_begin(3);
        let mut fired = Vec::new();
        for _ in 0..20 {
            if let Some(a) = t.swipe_update(0.0, -20.0) {
                fired.push(a);
            }
        }
        assert_eq!(fired, vec![GestureAction::SwitchWorkspace(-1)]);
    }

    #[test]
    fn the_action_fires_mid_gesture_not_at_the_end() {
        // Waiting for `end` makes the desktop feel a gesture behind.
        let mut t = GestureTracker::new(cfg());
        t.swipe_begin(3);
        assert_eq!(t.swipe_update(0.0, -40.0), None, "still below threshold");
        assert_eq!(
            t.swipe_update(0.0, -70.0),
            Some(GestureAction::SwitchWorkspace(-1)),
            "the accumulated 110px must fire without waiting for the fingers to lift"
        );
    }

    #[test]
    fn a_second_gesture_after_the_first_fires_again() {
        let mut t = GestureTracker::new(cfg());
        t.swipe_begin(3);
        assert!(t.swipe_update(0.0, -150.0).is_some());
        t.swipe_end();
        t.swipe_begin(3);
        assert_eq!(
            t.swipe_update(0.0, -150.0),
            Some(GestureAction::SwitchWorkspace(-1)),
            "the latch must reset when the gesture ends"
        );
    }

    #[test]
    fn a_two_finger_swipe_never_produces_a_shell_action() {
        let mut t = GestureTracker::new(cfg());
        t.swipe_begin(2);
        assert_eq!(t.swipe_update(0.0, -500.0), None);
        assert!(
            !t.shell_owns_current(),
            "a two-finger gesture must stay the client's for its whole duration"
        );
    }

    #[test]
    fn updates_outside_a_gesture_are_ignored() {
        // libinput can deliver an update after a cancelled end if the stream is torn; that must not accumulate into the next gesture.
        let mut t = GestureTracker::new(cfg());
        assert_eq!(t.swipe_update(0.0, -500.0), None);
        assert_eq!(t.active_fingers(), None);
    }

    #[test]
    fn the_shell_owns_the_whole_duration_of_a_three_finger_gesture() {
        // The finger count only arrives on `begin`, so the loop needs the in-flight answer to route the updates that follow.
        let mut t = GestureTracker::new(cfg());
        t.swipe_begin(3);
        assert!(t.shell_owns_current());
        t.swipe_update(0.0, -150.0);
        assert!(t.shell_owns_current(), "still ours after it has fired");
        t.swipe_end();
        assert!(!t.shell_owns_current());
    }

    #[test]
    fn a_pinch_fires_once_and_resets() {
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

    #[test]
    fn a_two_finger_pinch_is_left_to_the_client() {
        // Browser zoom.
        let mut t = GestureTracker::new(cfg());
        t.pinch_begin(2);
        assert_eq!(t.pinch_update(0.3), None);
    }

    #[test]
    fn changing_the_direction_preference_takes_effect_on_the_next_gesture() {
        let mut t = GestureTracker::new(cfg());
        t.swipe_begin(3);
        assert_eq!(
            t.swipe_update(0.0, -150.0),
            Some(GestureAction::SwitchWorkspace(-1))
        );
        t.swipe_end();
        t.set_config(natural());
        t.swipe_begin(3);
        assert_eq!(
            t.swipe_update(0.0, -150.0),
            Some(GestureAction::SwitchWorkspace(1))
        );
    }
}
