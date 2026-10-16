//! Pointer lock and confinement (`zwp_pointer_constraints_v1`) plus
//! relative motion (`zwp_relative_pointer_v1`), which games and remote
//! desktop clients need.
//!
//! The compositor, not the client, decides when a constraint takes effect:
//! only for the surface that holds keyboard focus *and* sits under the local
//! pointer, and never while the session is locked or shell chrome owns
//! input. Losing either focus releases it. The release chord
//! ([`release_chord_label`]) always
//! releases it too, and the surface is not granted again until focus moves
//! on, so a misbehaving game cannot trap the pointer.
//!
//! Remote peers never reach this: grants follow only the local pointer
//! router, and the remote-input firewall refuses peer pointer motion while a
//! constraint is engaged, so a guest can neither take a lock nor break one.

use smithay::input::pointer::{PointerHandle, RelativeMotionEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::RectangleKind;
use smithay::wayland::pointer_constraints::{
    PointerConstraint, PointerConstraintsHandler, with_pointer_constraint,
};

use crate::compositor::backend::PointerConstraintRegion;
use crate::compositor::state::State;

/// The chord that always releases a locked or confined pointer. Super+Esc
/// is the remote-input kill switch, so the release uses the other
/// long-standing "get me out" chord; it is intercepted only while a
/// constraint is engaged and otherwise reaches the focused client. Named
/// in the legends of the active keyboard layout (`Strg+Alt+Esc` on German).
pub fn release_chord_label() -> String {
    super::release_chord_label()
}

/// Whether `event` is the release chord, on key press.
pub fn is_release_chord(
    event: &winit::event::WindowEvent,
    modifiers: winit::event::Modifiers,
) -> bool {
    use winit::event::{ElementState, WindowEvent};
    use winit::keyboard::{Key, NamedKey};
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    let state = modifiers.state();
    event.state == ElementState::Pressed
        && state.control_key()
        && state.alt_key()
        && !state.super_key()
        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
}

#[derive(Default)]
pub struct PointerLock {
    active: Option<Active>,
    /// The region handed to the backend for the active constraint.
    region: PointerConstraintRegion,
    /// The surface released with the chord; not granted again until the
    /// grant target moves elsewhere.
    released: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct Active {
    surface_id: u64,
    locked: bool,
    origin: (f64, f64),
}

impl PointerLock {
    /// A lock or confinement is in effect.
    pub fn is_engaged(&self) -> bool {
        self.active.is_some()
    }

    /// The pointer is held still (as opposed to confined).
    pub fn is_locked(&self) -> bool {
        self.active.is_some_and(|active| active.locked)
    }
}

/// The only surface that may hold the pointer this tick: keyboard-focused,
/// under the local pointer, at `rect` (`x0, y0, x1, y1`, global).
#[derive(Clone, Copy, Debug)]
pub struct GrantTarget {
    pub surface_id: u64,
    pub rect: (f64, f64, f64, f64),
}

/// What changed, for the backend and the on-screen hint.
#[derive(Clone, Debug, PartialEq)]
pub struct ConstraintChange {
    pub region: PointerConstraintRegion,
    /// A constraint was just granted: tell the person how to get out.
    pub engaged: bool,
    /// A lock ended with a cursor position hint: put the pointer there.
    pub warp_to: Option<(f64, f64)>,
}

impl ConstraintChange {
    fn released(warp_to: Option<(f64, f64)>) -> Self {
        Self {
            region: PointerConstraintRegion::Free,
            engaged: false,
            warp_to,
        }
    }
}

impl State {
    /// Grant, adjust or withdraw the pointer constraint for this tick's
    /// `target` (see [`GrantTarget`]). `None` when nothing changed.
    pub fn sync_pointer_constraint(
        &mut self,
        target: Option<GrantTarget>,
    ) -> Option<ConstraintChange> {
        let pointer = self.seat.get_pointer()?;
        let target_id = target.map(|target| target.surface_id);
        if self.pointer_lock.released.is_some() && self.pointer_lock.released != target_id {
            self.pointer_lock.released = None;
        }
        let target = target.filter(|target| self.pointer_lock.released != Some(target.surface_id));

        if let Some(active) = self.pointer_lock.active
            && target.map(|target| target.surface_id) != Some(active.surface_id)
        {
            return Some(self.withdraw(&pointer, active));
        }
        let target = target?;
        let Some(surface) = self.client_input.surface(target.surface_id).cloned() else {
            return self
                .pointer_lock
                .active
                .map(|active| self.withdraw(&pointer, active));
        };
        let origin = (target.rect.0, target.rect.1);
        let requested = with_pointer_constraint(&surface, &pointer, |constraint| {
            constraint.map(|constraint| {
                let region = match &*constraint {
                    PointerConstraint::Locked(_) => PointerConstraintRegion::Locked,
                    PointerConstraint::Confined(_) => PointerConstraintRegion::Confined(
                        confinement(constraint.region(), target.rect),
                    ),
                };
                (constraint.is_active(), region)
            })
        });
        let Some((client_sees_active, region)) = requested else {
            // The client destroyed its constraint (or a one-shot ended).
            return self.pointer_lock.active.map(|_| self.forget_constraint());
        };
        let engaged = !client_sees_active;
        if engaged {
            with_pointer_constraint(&surface, &pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
        let changed = engaged || self.pointer_lock.region != region;
        self.pointer_lock.active = Some(Active {
            surface_id: target.surface_id,
            locked: region == PointerConstraintRegion::Locked,
            origin,
        });
        self.pointer_lock.region = region.clone();
        changed.then_some(ConstraintChange {
            region,
            engaged,
            warp_to: None,
        })
    }

    /// The release chord: end the constraint now and keep it off until the
    /// person moves focus. `None` when nothing was engaged.
    pub fn release_pointer_constraint(&mut self) -> Option<ConstraintChange> {
        let active = self.pointer_lock.active?;
        let pointer = self.seat.get_pointer()?;
        self.pointer_lock.released = Some(active.surface_id);
        Some(self.withdraw(&pointer, active))
    }

    fn withdraw(&mut self, pointer: &PointerHandle<Self>, active: Active) -> ConstraintChange {
        let warp_to = self
            .client_input
            .surface(active.surface_id)
            .cloned()
            .and_then(|surface| deactivate(&surface, pointer))
            .map(|hint| (active.origin.0 + hint.x, active.origin.1 + hint.y));
        self.forget_constraint_with(warp_to)
    }

    fn forget_constraint(&mut self) -> ConstraintChange {
        self.forget_constraint_with(None)
    }

    fn forget_constraint_with(&mut self, warp_to: Option<(f64, f64)>) -> ConstraintChange {
        self.pointer_lock.active = None;
        self.pointer_lock.region = PointerConstraintRegion::Free;
        ConstraintChange::released(warp_to)
    }

    /// Hand one local relative motion to the pointer-focused client's
    /// `zwp_relative_pointer_v1` objects. `false` when no client has the
    /// pointer.
    pub fn forward_client_relative_motion(
        &mut self,
        delta: (f64, f64),
        delta_unaccel: (f64, f64),
        time_usec: u64,
    ) -> bool {
        let Some(surface_id) = self.client_input.pointer_focus() else {
            return false;
        };
        let Some(surface) = self.client_input.surface(surface_id).cloned() else {
            return false;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let origin = self.client_input.pointer_focus_origin().unwrap_or_default();
        pointer.relative_motion(
            self,
            Some((surface, Point::from(origin))),
            &RelativeMotionEvent {
                delta: delta.into(),
                delta_unaccel: delta_unaccel.into(),
                utime: time_usec,
            },
        );
        pointer.frame(self);
        true
    }
}

/// End an active constraint, returning a lock's surface-local cursor
/// position hint.
fn deactivate(surface: &WlSurface, pointer: &PointerHandle<State>) -> Option<Point<f64, Logical>> {
    with_pointer_constraint(surface, pointer, |constraint| {
        let constraint = constraint?;
        let hint = match &*constraint {
            PointerConstraint::Locked(locked) => locked.cursor_position_hint(),
            PointerConstraint::Confined(_) => None,
        };
        if constraint.is_active() {
            constraint.deactivate();
        }
        hint
    })
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

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        // Granted (or not) by the next tick's `sync_pointer_constraint`,
        // which knows what is focused and visible.
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
        // Read when the lock ends; the pointer stays put until then.
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
    fn the_release_chord_is_ctrl_alt_escape_and_not_the_kill_switch() {
        use winit::event::{ElementState, Modifiers};
        use winit::keyboard::{Key, ModifiersState, NamedKey};
        let escape = |pressed: bool| {
            let event = ui::synthetic_input::key_event(
                Key::Named(NamedKey::Escape),
                None,
                if pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
            );
            ui::synthetic_input::keyboard_input(event)
        };
        let ctrl_alt = Modifiers::from(ModifiersState::CONTROL | ModifiersState::ALT);
        assert!(is_release_chord(&escape(true), ctrl_alt));
        assert!(!is_release_chord(&escape(false), ctrl_alt));
        assert!(!is_release_chord(
            &escape(true),
            Modifiers::from(ModifiersState::CONTROL)
        ));
        assert!(!is_release_chord(
            &escape(true),
            Modifiers::from(ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER)
        ));
        assert!(!crate::shell::input_policy::is_remote_input_kill_switch(
            &escape(true),
            ctrl_alt
        ));
    }
}
