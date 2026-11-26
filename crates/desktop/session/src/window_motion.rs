//! Windows coming and going: a window the compositor maps comes out of the control the person pressed
//! (the dock icon or the search row) and grows into place, and one that closes flies back into it from
//! its last frame — or, for a window asked for by a shortcut or an agent, fades in and grows where it
//! stands and shrinks away again — on the toolkit's window motion ([`WindowMotion::OPEN`]), the curve
//! and pace a window's tile runs inside the shell. The compositor draws them through window effects;
//! input lands where a window stands.

use std::collections::BTreeMap;
use std::time::Instant;

use tilo_compositor::{Compositor, WindowEffect};

use ui::kurbo::{Point, Rect};
use tilo_ui_base::{motion::Direction, motion::WindowMotion};
use tilo_ui_base::{tokens::EffectiveMotion, tokens::MotionSource};

/// How far a window `elapsed` into its motion `direction` has come.
fn progress(motion: EffectiveMotion, direction: Direction, elapsed: std::time::Duration) -> f64 {
    WindowMotion::OPEN.progress_after(direction, motion, MotionSource::Pointer, elapsed)
}

/// The control a window asked for now comes out of, when the person's last press left the pointer
/// where the dock icon or the search row stood. `None` when a shortcut, an agent or a sync asked for
/// it (no recent press), which land or grow in place.
fn control(asked: Option<MotionSource>, pointer: (f64, f64)) -> Option<Point> {
    asked
        .filter(|source| !source.lands_at_once())
        .map(|_| Point::new(pointer.0, pointer.1))
}

/// A window's place from the compositor's own reading of it.
fn rect_of((x, y, width, height): (i32, i32, i32, i32)) -> Rect {
    Rect::new(
        f64::from(x),
        f64::from(y),
        f64::from(x.saturating_add(width)),
        f64::from(y.saturating_add(height)),
    )
}

/// The windows opening and closing, by id, and when each began.
pub(crate) struct WindowMotions {
    motion: EffectiveMotion,
    /// Every window seen mapped, and when it opened while it still does.
    known: BTreeMap<u64, Option<Instant>>,
    closing: BTreeMap<u64, Instant>,
    /// The control each window was asked for from, by id: what its arrival comes out of and its
    /// close goes back into, for as long as the window lives.
    controls: BTreeMap<u64, Point>,
}

impl WindowMotions {
    pub(crate) const fn new(motion: EffectiveMotion) -> Self {
        Self {
            motion,
            known: BTreeMap::new(),
            closing: BTreeMap::new(),
            controls: BTreeMap::new(),
        }
    }

    /// The window drawn `openness` (0 gone, 1 in place) of the way open, in `rect`: out of `control`
    /// when one asked for it, where it stands otherwise.
    fn effect(&self, openness: f64, rect: Option<Rect>, control: Option<Point>) -> WindowEffect {
        if let (Some(rect), Some(control)) = (rect, control)
            && rect.width() > 0.0
            && rect.height() > 0.0
        {
            let pose = WindowMotion::flying_pose(openness, rect, control, self.motion);
            let drawn = pose.transform.transform_rect_bbox(rect);
            return WindowEffect {
                offset: (
                    drawn.center().x - rect.center().x,
                    drawn.center().y - rect.center().y,
                ),
                scale: drawn.width() / rect.width(),
                opacity: pose.opacity,
            };
        }
        let pose = WindowMotion::content_pose(openness, Rect::ZERO, self.motion);
        WindowEffect {
            offset: (0.0, 0.0),
            scale: pose.transform.as_coeffs()[0],
            opacity: pose.opacity,
        }
    }

    /// Whether a window is still opening or closing: the shell is moving.
    pub(crate) fn animating(&self) -> bool {
        self.known.values().any(Option::is_some) || !self.closing.is_empty()
    }

    /// Draw each opening and closing window as far as it has come at `now`,
    /// and let go of the last frame of each that finished closing.
    pub(crate) fn step(&mut self, compositor: &mut Compositor, now: Instant) {
        let windows = compositor.windows();
        self.known
            .retain(|id, _| windows.iter().any(|(info, _)| info.id == *id));
        let mut rects: BTreeMap<u64, Rect> = BTreeMap::new();
        for (info, at) in &windows {
            rects.insert(info.id, rect_of(*at));
            if let std::collections::btree_map::Entry::Vacant(appearing) = self.known.entry(info.id)
            {
                appearing.insert(Some(now));
                if let Some(control) =
                    control(tilo_ui_base::motion::recent_input(now), compositor.pointer_location())
                {
                    self.controls.insert(info.id, control);
                }
            }
        }
        let closing = compositor.closing_windows();
        self.closing
            .retain(|id, _| closing.iter().any(|(info, _)| info.id == *id));
        for (info, at) in &closing {
            rects.insert(info.id, rect_of(*at));
            self.closing.entry(info.id).or_insert(now);
        }
        // A control is remembered while its window stands, opening or closing: what its close flies
        // back into.
        let mut live: std::collections::BTreeSet<u64> = self.known.keys().copied().collect();
        live.extend(self.closing.keys().copied());
        self.controls.retain(|id, _| live.contains(id));

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
            let effect = openness.map(|openness| {
                self.effect(
                    openness,
                    rects.get(&id).copied(),
                    self.controls.get(&id).copied(),
                )
            });
            compositor.set_window_effect(id, effect);
        }
        for id in gone {
            self.closing.remove(&id);
            self.controls.remove(&id);
            compositor.release_snapshot(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window standing well clear of the dock, and the icon the person pressed for it.
    fn window() -> Rect {
        Rect::new(400.0, 100.0, 1000.0, 500.0)
    }
    const CONTROL: Point = Point::new(40.0, 900.0);

    tilo_test_env::scenario! {
        fn a_window_flies_out_of_the_control_that_asked_for_it_and_back_into_it;

        phase a_window_the_person_asks_for_comes_out_of_the_icon_they_pressed;
        {
            let motions = WindowMotions::new(EffectiveMotion::default());
            let start = motions.effect(0.0, Some(window()), Some(CONTROL));
            // Where it starts is a mark at the control: small, faint, and away toward the dock.
            assert!(start.opacity < 0.01, "{start:?}");
            assert!(start.scale < 0.4, "it starts as small as the control: {start:?}");
            assert!(start.offset.0 < -400.0 && start.offset.1 > 400.0, "{start:?}");
            let half = motions.effect(0.5, Some(window()), Some(CONTROL));
            assert!(half.scale > start.scale && half.scale < 1.0, "{half:?}");
            assert!(
                half.offset.0 > start.offset.0 && half.offset.1 < start.offset.1,
                "it is on its way from the dock to its frame: {half:?}"
            );
            assert_eq!(
                motions.effect(1.0, Some(window()), Some(CONTROL)),
                WindowEffect::default()
            );
        }

        phase a_window_asked_for_by_an_agent_grows_where_it_stands;
        {
            let motions = WindowMotions::new(EffectiveMotion::default());
            let start = motions.effect(0.0, Some(window()), None);
            assert_eq!(start.offset, (0.0, 0.0), "it grows in place: {start:?}");
            assert!(start.scale < 1.0 && start.opacity < 0.01, "{start:?}");
        }

        phase a_window_that_closes_flies_back_into_its_control;
        {
            let motions = WindowMotions::new(EffectiveMotion::default());
            // Closing draws the same flight the other way: whole where it stood, then away toward
            // the control, smaller and fainter on the way.
            assert_eq!(
                motions.effect(1.0, Some(window()), Some(CONTROL)),
                WindowEffect::default(),
                "the last frame it drew is where its flight starts"
            );
            let leaving = motions.effect(0.3, Some(window()), Some(CONTROL));
            assert!(
                leaving.offset.0 < -200.0 && leaving.offset.1 > 200.0,
                "it is on its way back to the dock: {leaving:?}"
            );
            assert!(leaving.scale > 0.2 && leaving.scale < 1.0, "{leaving:?}");
            let almost_gone = motions.effect(0.05, Some(window()), Some(CONTROL));
            assert!(
                almost_gone.opacity < leaving.opacity && almost_gone.scale < leaving.scale,
                "it fades and shrinks into the control: {almost_gone:?}"
            );
        }

        phase only_a_press_asks_for_the_flight;
        {
            // The dock icon the pointer was on.
            assert_eq!(
                control(Some(MotionSource::Pointer), (40.0, 900.0)),
                Some(Point::new(40.0, 900.0))
            );
            assert_eq!(
                control(Some(MotionSource::Gesture), (40.0, 900.0)),
                Some(Point::new(40.0, 900.0))
            );
            for source in [MotionSource::Keyboard, MotionSource::Action] {
                assert_eq!(
                    control(Some(source), (40.0, 900.0)),
                    None,
                    "{source:?} lands or grows where the window stands"
                );
            }
            assert_eq!(
                control(None, (40.0, 900.0)),
                None,
                "a window nothing just asked for grows in place"
            );
        }

        phase reduced_motion_only_fades_in_place;
        {
            let motions = WindowMotions::new(EffectiveMotion::new(tilo_ui_base::tokens::Motion::default(), true));
            let half = motions.effect(0.5, Some(window()), Some(CONTROL));
            assert_eq!(half.offset, (0.0, 0.0), "it does not travel: {half:?}");
            assert!((half.scale - 1.0).abs() < f64::EPSILON, "{half:?}");
            assert!(half.opacity > 0.0 && half.opacity < 1.0, "{half:?}");
        }
    }
}
