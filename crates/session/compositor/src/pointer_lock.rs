//! Pointer lock and confinement (`zwp_pointer_constraints_v1`) plus
//! relative motion (`zwp_relative_pointer_v1`), which games and remote
//! desktop clients need.
//!
//! The compositor, not the client, decides when a constraint takes effect:
//! only for the surface that holds keyboard focus *and* sits under the local
//! pointer, and never while the session is locked or a hosted surface owns
//! input. Losing either focus releases it. Ctrl+Alt+Escape always releases it
//! too, and the surface is not granted again until focus moves on, so a
//! misbehaving game cannot trap the pointer.
//!
//! A locked pointer stays where it is: its client reads the deltas through
//! `zwp_relative_pointer_v1` and sees no `wl_pointer.motion`. A confined one
//! moves, but never past the edge of its region.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "surface and region geometry in logical pixels, far from f64 limits"
)]

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::input::pointer::{PointerHandle, RelativeMotionEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, SERIAL_COUNTER};
use smithay::wayland::compositor::RectangleKind;
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};

use crate::state::State;

/// What a granted constraint lets the pointer do.
#[derive(Clone, Debug, Default, PartialEq)]
enum Region {
    /// It moves as it always does.
    #[default]
    Free,
    /// It stays where it is; only relative motion is reported.
    Locked,
    /// It stays inside the union of these `(x0, y0, x1, y1)` rects.
    Confined(Vec<(f64, f64, f64, f64)>),
}

impl Region {
    /// Where motion from `current` towards `requested` ends up, or `None`
    /// when the pointer must not move at all.
    fn constrain(&self, current: (f64, f64), requested: (f64, f64)) -> Option<(f64, f64)> {
        match self {
            Self::Free => Some(requested),
            Self::Locked => None,
            Self::Confined(rects) => {
                let inside = |(x, y): (f64, f64), &(x0, y0, x1, y1): &(f64, f64, f64, f64)| {
                    x >= x0 && x < x1 && y >= y0 && y < y1
                };
                if rects.iter().any(|rect| inside(requested, rect)) {
                    return Some(requested);
                }
                // Slide along the edge of the rect the pointer is in (or the
                // first one, if the region changed under it).
                let &(x0, y0, x1, y1) = rects
                    .iter()
                    .find(|rect| inside(current, rect))
                    .or_else(|| rects.first())?;
                Some((
                    requested.0.clamp(x0, (x1 - 1.0).max(x0)),
                    requested.1.clamp(y0, (y1 - 1.0).max(y0)),
                ))
            }
        }
    }
}

/// The surface a constraint was granted for, and where it starts.
#[derive(Clone, Debug)]
struct Active {
    surface: WlSurface,
    origin: Point<f64, Logical>,
}

/// The constraint the compositor granted: whose it is, and how far the
/// pointer may move.
#[derive(Debug, Default)]
pub(crate) struct PointerLock {
    active: Option<Active>,
    /// What the pointer may do while [`PointerLock::active`] holds.
    region: Region,
    /// The surface the person released with the chord; not granted again
    /// until the pointer-and-focus target moves elsewhere.
    released: Option<WlSurface>,
}

impl PointerLock {
    /// A lock or confinement is in effect.
    pub(crate) fn is_engaged(&self) -> bool {
        self.active.is_some()
    }

    /// Where motion from `current` towards `requested` ends up, or `None`
    /// when the pointer stays put.
    pub(crate) fn constrain(
        &self,
        current: (f64, f64),
        requested: (f64, f64),
    ) -> Option<(f64, f64)> {
        self.region.constrain(current, requested)
    }
}

/// The only surface that may hold the pointer this moment, and where it sits
/// in the layout (`x0, y0, x1, y1`, global).
struct GrantTarget {
    surface: WlSurface,
    origin: Point<f64, Logical>,
    rect: (f64, f64, f64, f64),
}

impl State {
    /// The surface that may hold the pointer now: the one with keyboard
    /// focus that is also under the pointer, while the session is unlocked
    /// and no hosted surface is taking input.
    fn constraint_target(&self) -> Option<GrantTarget> {
        if self.locked() || self.hosts.iter().any(|host| host.wants_keyboard()) {
            return None;
        }
        let (surface, origin) = self.pointer_surface_under(self.pointer)?;
        if !surface.alive() {
            return None;
        }
        let pointer = self.seat.get_pointer()?;
        if pointer.current_focus().as_ref() != Some(&surface) {
            return None;
        }
        let keyboard = self.seat.get_keyboard()?;
        if keyboard.current_focus().as_ref() != Some(&surface) {
            return None;
        }
        let size = with_renderer_surface_state(&surface, |state| state.surface_size()).flatten()?;
        let rect = (
            origin.x,
            origin.y,
            origin.x + f64::from(size.w),
            origin.y + f64::from(size.h),
        );
        Some(GrantTarget {
            surface,
            origin,
            rect,
        })
    }

    /// Grant, adjust or withdraw the pointer constraint for this moment.
    /// Called every turn, after the clients' requests were handled, so a
    /// constraint a client asked for takes effect once it is focused and
    /// under the pointer, and goes away the moment either changes.
    pub(crate) fn sync_pointer_constraint(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let target = self.constraint_target();
        let target_surface = target.as_ref().map(|target| target.surface.clone());
        if self.pointer_lock.released.is_some() && self.pointer_lock.released != target_surface {
            self.pointer_lock.released = None;
        }
        let target =
            target.filter(|target| self.pointer_lock.released.as_ref() != Some(&target.surface));

        if let Some(active) = self.pointer_lock.active.clone()
            && target_surface.as_ref() != Some(&active.surface)
        {
            self.withdraw(&pointer, &active);
            return;
        }
        let Some(target) = target else {
            return;
        };
        let requested = with_pointer_constraint(&target.surface, &pointer, |constraint| {
            constraint.map(|constraint| {
                let region = match &*constraint {
                    PointerConstraint::Locked(_) => Region::Locked,
                    PointerConstraint::Confined(_) => {
                        Region::Confined(confinement(constraint.region(), target.rect))
                    }
                };
                (constraint.is_active(), region)
            })
        });
        let Some((client_sees_active, region)) = requested else {
            // The client destroyed its constraint, or a one-shot ended.
            if self.pointer_lock.active.is_some() {
                self.forget(None);
            }
            return;
        };
        if !client_sees_active {
            with_pointer_constraint(&target.surface, &pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
        self.pointer_lock.region = region;
        self.pointer_lock.active = Some(Active {
            surface: target.surface,
            origin: target.origin,
        });
    }

    /// The release chord: end the constraint now, and keep it off until the
    /// pointer-and-focus target moves elsewhere. `false` when nothing was
    /// engaged.
    pub(crate) fn release_pointer_lock(&mut self) -> bool {
        let Some(active) = self.pointer_lock.active.clone() else {
            return false;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        self.pointer_lock.released = Some(active.surface.clone());
        self.withdraw(&pointer, &active);
        true
    }

    /// Hand one local relative motion to the pointer-focused client's
    /// `zwp_relative_pointer_v1` objects: what a game under a lock reads,
    /// and what any client with the protocol reads the rest of the time.
    pub(crate) fn forward_relative_motion(&mut self, dx: f64, dy: f64, time_usec: u64) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.relative_motion(
            self,
            None,
            &RelativeMotionEvent {
                delta: (dx, dy).into(),
                delta_unaccel: (dx, dy).into(),
                utime: time_usec,
            },
        );
        pointer.frame(self);
    }

    /// End an active constraint, putting the pointer where a lock's cursor
    /// position hint asks for. A surface that is gone takes its constraint
    /// with it, so there is nothing left to read from it.
    fn withdraw(&mut self, pointer: &PointerHandle<Self>, active: &Active) {
        if !active.surface.alive() {
            self.forget(None);
            return;
        }
        let hint = with_pointer_constraint(&active.surface, pointer, |constraint| {
            let constraint = constraint?;
            let hint = match &*constraint {
                PointerConstraint::Locked(locked) => locked.cursor_position_hint(),
                PointerConstraint::Confined(_) => None,
            };
            if constraint.is_active() {
                constraint.deactivate();
            }
            hint
        });
        self.forget(hint.map(|hint| active.origin + hint));
    }

    fn forget(&mut self, warp_to: Option<Point<f64, Logical>>) {
        self.pointer_lock.active = None;
        self.pointer_lock.region = Region::Free;
        if let Some(to) = warp_to {
            let serial = SERIAL_COUNTER.next_serial();
            self.pointer_motion(to, serial, self.now_ms());
        }
    }
}

/// The global rects a confined pointer may reach: the client's region
/// clipped to its surface, or the whole surface without one. Subtracted
/// rectangles are not carved out; the pointer may still enter them, which
/// only ever gives it more room, never less.
fn confinement(
    region: Option<&smithay::wayland::compositor::RegionAttributes>,
    surface: (f64, f64, f64, f64),
) -> Vec<(f64, f64, f64, f64)> {
    let (sx0, sy0, sx1, sy1) = surface;
    let rects: Vec<_> = region
        .into_iter()
        .flat_map(|region| region.rects.iter())
        .filter(|(kind, _)| matches!(kind, RectangleKind::Add))
        .filter_map(|(_, rect)| {
            let x0 = (sx0 + f64::from(rect.loc.x)).max(sx0);
            let y0 = (sy0 + f64::from(rect.loc.y)).max(sy0);
            let x1 = (sx0 + f64::from(rect.loc.x) + f64::from(rect.size.w)).min(sx1);
            let y1 = (sy0 + f64::from(rect.loc.y) + f64::from(rect.size.h)).min(sy1);
            (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
        })
        .collect();
    if rects.is_empty() {
        vec![surface]
    } else {
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confinement_clips_the_region_to_the_surface() {
        use smithay::utils::Rectangle;
        let surface = (100.0, 50.0, 500.0, 350.0);
        assert_eq!(confinement(None, surface), vec![surface]);
        let region = smithay::wayland::compositor::RegionAttributes {
            rects: vec![
                (
                    RectangleKind::Add,
                    Rectangle::new((10, 20).into(), (50, 60).into()),
                ),
                (
                    RectangleKind::Add,
                    Rectangle::new((350, 250).into(), (500, 500).into()),
                ),
                (
                    RectangleKind::Subtract,
                    Rectangle::new((0, 0).into(), (5, 5).into()),
                ),
            ],
        };
        assert_eq!(
            confinement(Some(&region), surface),
            vec![(110.0, 70.0, 160.0, 130.0), (450.0, 300.0, 500.0, 350.0)]
        );
    }

    #[test]
    fn a_locked_pointer_stays_and_a_confined_one_stops_at_the_edge() {
        let free = Region::Free;
        assert_eq!(free.constrain((5.0, 5.0), (90.0, 90.0)), Some((90.0, 90.0)));
        let locked = Region::Locked;
        assert_eq!(locked.constrain((5.0, 5.0), (90.0, 90.0)), None);
        let confined = Region::Confined(vec![(10.0, 10.0, 60.0, 60.0)]);
        // Inside is untouched, outside slides along the edge.
        assert_eq!(
            confined.constrain((30.0, 30.0), (40.0, 50.0)),
            Some((40.0, 50.0))
        );
        assert_eq!(
            confined.constrain((30.0, 30.0), (200.0, 200.0)),
            Some((59.0, 59.0))
        );
        assert_eq!(
            confined.constrain((30.0, 30.0), (5.0, 40.0)),
            Some((10.0, 40.0))
        );
        // Between two rects, the pointer slides along the one it is in.
        let two = Region::Confined(vec![(0.0, 0.0, 10.0, 10.0), (20.0, 20.0, 30.0, 30.0)]);
        assert_eq!(two.constrain((25.0, 25.0), (0.0, 25.0)), Some((20.0, 25.0)));
    }
}
