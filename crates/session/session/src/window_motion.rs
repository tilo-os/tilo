//! Windows coming and going: a window the compositor maps fades in and
//! grows into place, and one that closes shrinks and fades from its last
//! frame, on the toolkit's window motion ([`WindowMotion::OPEN`]), the
//! curve and pace a window's tile runs inside the desk. The compositor
//! draws them through window effects; input lands where a window stands.

use std::collections::BTreeMap;
use std::time::Instant;

use tilo_compositor::{Compositor, WindowEffect};
use ui::kurbo::Rect;
use ui::motion::{Direction, WindowMotion};
use ui::tokens::{EffectiveMotion, MotionSource};

/// How far a window `elapsed` into its motion `direction` has come.
fn progress(motion: EffectiveMotion, direction: Direction, elapsed: std::time::Duration) -> f64 {
    WindowMotion::OPEN.progress_after(direction, motion, MotionSource::Pointer, elapsed)
}

/// The windows opening and closing, by id, and when each began.
pub(crate) struct WindowMotions {
    motion: EffectiveMotion,
    /// Every window seen mapped, and when it opened while it still does.
    known: BTreeMap<u64, Option<Instant>>,
    closing: BTreeMap<u64, Instant>,
}

impl WindowMotions {
    pub(crate) const fn new(motion: EffectiveMotion) -> Self {
        Self {
            motion,
            known: BTreeMap::new(),
            closing: BTreeMap::new(),
        }
    }

    /// The window drawn `openness` (0 gone, 1 in place) of the way open.
    fn effect(&self, openness: f64) -> WindowEffect {
        let pose = WindowMotion::content_pose(openness, Rect::ZERO, self.motion);
        WindowEffect {
            offset: (0.0, 0.0),
            scale: pose.transform.as_coeffs()[0],
            opacity: pose.opacity,
        }
    }

    /// Draw each opening and closing window as far as it has come at `now`,
    /// and let go of the last frame of each that finished closing.
    pub(crate) fn step(&mut self, compositor: &mut Compositor, now: Instant) {
        let windows = compositor.windows();
        self.known
            .retain(|id, _| windows.iter().any(|(info, _)| info.id == *id));
        for (info, _) in &windows {
            self.known.entry(info.id).or_insert(Some(now));
        }
        let closing = compositor.closing_windows();
        self.closing
            .retain(|id, _| closing.iter().any(|(info, _)| info.id == *id));
        for (info, _) in &closing {
            self.closing.entry(info.id).or_insert(now);
        }

        let mut effects = Vec::new();
        for (&id, opened) in &mut self.known {
            let Some(at) = *opened else { continue };
            let progress = progress(
                self.motion,
                Direction::Enter,
                now.saturating_duration_since(at),
            );
            if progress >= 1.0 {
                *opened = None;
                effects.push((id, None));
            } else {
                effects.push((id, Some(progress)));
            }
        }
        let mut gone = Vec::new();
        for (&id, &at) in &self.closing {
            let progress = progress(
                self.motion,
                Direction::Exit,
                now.saturating_duration_since(at),
            );
            if progress >= 1.0 {
                gone.push(id);
            } else {
                effects.push((id, Some(1.0 - progress)));
            }
        }
        for (id, openness) in effects {
            compositor.set_window_effect(id, openness.map(|openness| self.effect(openness)));
        }
        for id in gone {
            self.closing.remove(&id);
            compositor.release_snapshot(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_opens_from_faint_and_small_to_whole() {
        let motions = WindowMotions::new(EffectiveMotion::default());
        let start = motions.effect(0.0);
        assert!(start.opacity < 0.01 && start.scale < 1.0, "{start:?}");
        assert_eq!(motions.effect(1.0), WindowEffect::default());
        let half = motions.effect(0.5);
        assert!(half.scale > start.scale && half.scale < 1.0, "{half:?}");
    }

    #[test]
    fn reduced_motion_only_fades() {
        let motions = WindowMotions::new(EffectiveMotion::new(ui::tokens::Motion::default(), true));
        let half = motions.effect(0.5);
        assert!((half.scale - 1.0).abs() < f64::EPSILON, "{half:?}");
        assert!(half.opacity > 0.0 && half.opacity < 1.0, "{half:?}");
    }
}
