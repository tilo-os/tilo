//! The motion of the switches and checkboxes a host paints itself
//! ([`super::ToggleFace`], [`super::CheckboxFace`]), so a painted switch
//! slides and a painted box fades exactly as the retained [`super::Toggle`]
//! and [`super::Checkbox`] do.
//!
//! A host keeps one [`FaceMotions`] beside what it paints, asks it for each
//! control's pose by a key that names the control (not its place, which
//! moves with a scroll). Each motion asks the frame pump for its own frames
//! (`motion::want_frames_until`), so a host that reads the motion clock
//! paints them with nothing more; [`FaceMotions::is_moving`] says whether
//! any is on its way, and [`FaceMotions::needs_redraw`] whether the host's
//! paint is stale: one is moving, or the last pose painted was part-way and
//! the frame at rest has not been painted yet. A click or a finger moves a control; a key, an assistive action
//! or a value nobody just caused lands. The first value a control is seen
//! with rests. Settled, nothing asks for frames.

use super::checkbox::{CheckGlyph, CheckMotion, CheckPose};
use super::toggle::{SwitchMotion, SwitchPose};
use crate::time::Instant;
use crate::tokens::EffectiveMotion;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// The motions of a host's painted switches and checkboxes, by key. Read
/// while painting, through a shared reference.
#[derive(Debug, Default)]
pub struct FaceMotions {
    switches: RefCell<HashMap<u64, SwitchMotion>>,
    checks: RefCell<HashMap<u64, CheckMotion>>,
    /// The last pose painted was part-way: the frame showing every control
    /// at rest is still owed, though nothing is moving any more.
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

    /// How to draw the box `key`, checked (or mixed: `glyph`) when
    /// `marked`, this frame.
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

    /// A pose was read for painting at `now`, part-way when `moving`. Once
    /// a frame paints with nothing moving, the rest frame is drawn.
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

    /// Whether any control is still on its way at `now`: the host draws
    /// frames while it is, and none once everything has settled.
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
}
