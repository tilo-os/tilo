//! The motion of the switches and checkboxes a host paints itself ([`super::ToggleFace`],
//! [`super::CheckboxFace`]), so a painted switch slides and a painted box fades exactly as
//! the retained [`super::Toggle`] and [`super::Checkbox`] do.

use super::checkbox::{CheckGlyph, CheckMotion, CheckPose};
use super::toggle::{SwitchMotion, SwitchPose};
use crate::motion::Presence;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// The motions of a host's painted switches and checkboxes, by key.
#[derive(Debug, Default)]
pub struct FaceMotions {
    switches: RefCell<HashMap<u64, SwitchMotion>>,
    checks: RefCell<HashMap<u64, CheckMotion>>,
    hovers: RefCell<HashMap<u64, Presence>>,
    painted_moving: Cell<bool>,
}

fn key_of(key: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

impl FaceMotions {
    /// How to draw the switch `key` showing `on` this frame.
    #[track_caller]
    pub fn switch(&self, key: impl Hash, on: bool, motion: EffectiveMotion) -> SwitchPose {
        let now = crate::motion::frame_now();
        let mut switches = self.switches.borrow_mut();
        let switch = switches.entry(key_of(key)).or_default();
        switch.follow_input(on, motion, now);
        let pose = switch.pose(motion, now);
        let moving = switch.is_moving(now);
        drop(switches);
        self.painted(moving, now);
        pose
    }

    /// How to draw the box `key`, checked (or mixed: `glyph`) when `marked`, this frame.
    #[track_caller]
    pub fn check(
        &self,
        key: impl Hash,
        marked: bool,
        glyph: CheckGlyph,
        motion: EffectiveMotion,
    ) -> CheckPose {
        let now = crate::motion::frame_now();
        let mut checks = self.checks.borrow_mut();
        let check = checks.entry(key_of(key)).or_default();
        check.follow_input(marked, glyph, motion, now);
        let pose = check.pose(now);
        let moving = check.is_moving(now);
        drop(checks);
        self.painted(moving, now);
        pose
    }

    /// How much of its hover the control `key` shows this frame: `0` when the pointer is not over
    /// it, `1` once it has come in, part way while it comes or goes. A row the pointer reaches
    /// eases its tint in on the theme's control motion rather than switching it on in one frame.
    #[track_caller]
    pub fn hover(&self, key: impl Hash, hovered: bool, motion: EffectiveMotion) -> f32 {
        let now = crate::motion::frame_now();
        let source = crate::motion::recent_input(now).unwrap_or(MotionSource::Pointer);
        let mut hovers = self.hovers.borrow_mut();
        let presence = hovers.entry(key_of(key)).or_insert_with(Presence::closed);
        presence.show(
            hovered,
            crate::motion::ControlChange::SHOW,
            motion,
            source,
            now,
        );
        let level = presence.progress(now);
        let moving = presence.is_moving(now);
        drop(hovers);
        self.painted(moving, now);
        crate::num::f32_of_f64(level)
    }

    fn painted(&self, moving: bool, now: Instant) {
        if moving {
            self.painted_moving.set(true);
        } else if self.painted_moving.get() && !self.is_moving(now) {
            self.painted_moving.set(false);
        }
    }

    /// Whether the host must paint its controls again at `now`: one is
    /// moving, or the last frame painted one part-way and the frame showing
    /// it at rest is owed. The host's frames come from the motion clock.
    pub fn needs_redraw(&self, now: Instant) -> bool {
        self.painted_moving.get() || self.is_moving(now)
    }

    /// Whether any control is still on its way at `now`: the host draws frames while it is,
    /// and none once everything has settled.
    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.switches
            .borrow()
            .values()
            .any(|switch| switch.is_moving(now))
            || self
                .checks
                .borrow()
                .values()
                .any(|check| check.is_moving(now))
            || self
                .hovers
                .borrow()
                .values()
                .any(|hover| hover.is_moving(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::testing::with_monotonic_time;
    use crate::tokens::{Motion, MotionSource};

    #[test]
    fn a_painted_switch_and_box_move_on_a_click_land_on_a_key_and_then_rest() {
        let motion = EffectiveMotion::new(Motion::default(), false);
        let faces = FaceMotions::default();
        let t0 = crate::time::now();
        let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
        with_monotonic_time(t0, || {
            assert_eq!(
                faces.switch("wifi", false, motion),
                SwitchPose::at_rest(false)
            );
            assert_eq!(
                faces.check("done", false, CheckGlyph::Check, motion),
                CheckPose::at_rest(false, CheckGlyph::Check)
            );
            assert!(!faces.is_moving(t0), "first seen: at rest, no frames");
            crate::motion::note_source(MotionSource::Pointer);
            faces.switch("wifi", true, motion);
            faces.check("done", true, CheckGlyph::Check, motion);
        });
        with_monotonic_time(at(40), || {
            let pose = faces.switch("wifi", true, motion);
            assert!(
                pose.knob > 0.0 && pose.knob < 1.0,
                "a click slides: {pose:?}"
            );
            let check = faces.check("done", true, CheckGlyph::Check, motion);
            assert!(check.level > 0.0 && check.level < 1.0);
            assert!(faces.is_moving(at(40)));
            assert!(
                crate::motion::frames_wanted(at(40)),
                "a moving control asks the motion clock for its frames"
            );
        });
        with_monotonic_time(at(1_000), || {
            assert!(!faces.is_moving(at(1_000)), "settled: no frames");
            assert!(
                faces.needs_redraw(at(1_000)),
                "the last frame painted them part-way: the rest frame is owed"
            );
            faces.switch("wifi", true, motion);
            faces.check("done", true, CheckGlyph::Check, motion);
            assert!(
                !faces.needs_redraw(at(1_000)),
                "painted at rest: nothing more to paint"
            );
            crate::motion::note_source(MotionSource::Keyboard);
            assert_eq!(
                faces.switch("wifi", false, motion),
                SwitchPose::at_rest(false)
            );
            assert!(!faces.is_moving(at(1_000)), "a key lands");
        });
    }

    #[test]
    fn a_hover_comes_in_and_goes_out_with_the_pointer() {
        let motion = EffectiveMotion::new(Motion::default(), false);
        let faces = FaceMotions::default();
        let t0 = crate::time::now();
        let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
        with_monotonic_time(t0, || {
            assert_eq!(faces.hover("row", false, motion), 0.0, "at rest, no tint");
            crate::motion::note_source(MotionSource::Pointer);
            assert_eq!(
                faces.hover("row", true, motion),
                0.0,
                "the tint comes in from nothing"
            );
        });
        with_monotonic_time(at(30), || {
            let level = faces.hover("row", true, motion);
            assert!(level > 0.0 && level < 1.0, "part way in: {level}");
            assert!(
                crate::motion::frames_wanted(at(30)),
                "a hover on its way asks the motion clock for its frames"
            );
        });
        with_monotonic_time(at(1_000), || {
            assert_eq!(faces.hover("row", true, motion), 1.0, "the tint arrived");
            assert!(!faces.is_moving(at(1_000)), "settled: no frames");
        });
        with_monotonic_time(at(1_010), || {
            assert_eq!(
                faces.hover("row", false, motion),
                1.0,
                "the pointer left: the tint goes from where it stood"
            );
        });
        with_monotonic_time(at(1_040), || {
            let level = faces.hover("row", false, motion);
            assert!(level > 0.0 && level < 1.0, "part way out: {level}");
        });
        with_monotonic_time(at(3_000), || {
            assert_eq!(faces.hover("row", false, motion), 0.0, "gone");
            assert!(!faces.is_moving(at(3_000)), "settled: no frames");
        });
    }
}
