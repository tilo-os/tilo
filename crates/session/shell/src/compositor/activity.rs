//! What keeps the session awake: the person using any input device.
//!
//! The idle timers behind dim, blank, lock and suspend
//! ([`crate::compositor::idle::SessionManager`]) restart on activity. Activity
//! is decided here, once per batch of backend events, before the batch is
//! routed. Routing decides who receives an event (the shell, a Wayland
//! client, nobody on the lock screen); none of that changes whether the
//! person was there. Counting it at the delivery sites instead missed every
//! kind that goes straight to an app: a pen drawing in a tablet-aware app and
//! a two-finger pinch or swipe forwarded over `zwp_pointer_gestures_v1` let
//! the screen dim and lock under the person using them.

use crate::compositor::backend::BackendEvent;
use crate::compositor::idle::{SessionManager, SessionTransition};
use std::time::Instant;

/// Restart the idle period at `now` if `events` holds any person input
/// ([`BackendEvent::is_person_input`]), returning the transitions that wakes
/// (undim, panel on). A batch of only bookkeeping events leaves the timers.
pub fn note_person_input<'a>(
    session: &mut SessionManager,
    events: impl IntoIterator<Item = &'a BackendEvent>,
    now: Instant,
) -> Vec<SessionTransition> {
    if events.into_iter().any(BackendEvent::is_person_input) {
        session.note_activity(now)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::backend::drm::gestures::GestureAction;
    use crate::compositor::backend::{
        GestureEvent, PadAction, PadEvent, TabletDevice, TabletInput, TabletPhase, TabletTool,
        TabletToolKind, TouchEvent,
    };
    use crate::compositor::idle::IdleConfig;
    use std::time::Duration;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, WindowEvent};

    fn pen(phase: TabletPhase) -> BackendEvent {
        BackendEvent::Tablet(TabletInput {
            device: TabletDevice {
                name: "Wacom Intuos".into(),
                usb_id: Some((0x056a, 0x0374)),
                syspath: None,
            },
            tool: TabletTool {
                kind: TabletToolKind::Pen,
                serial: 7,
                wacom_id: 0x0802,
                pressure: true,
                distance: true,
                tilt: true,
                rotation: false,
                slider: false,
                wheel: false,
            },
            phase,
            position: (400.0, 300.0),
            pressure: 0.4,
            distance: 0.0,
            tilt: (0.0, 0.0),
            rotation: 0.0,
            slider: 0.0,
            wheel: (0.0, 0),
            time_msec: 1,
        })
    }

    /// Every kind of input a person makes, each as it reaches the loop.
    fn person_inputs() -> Vec<(&'static str, BackendEvent)> {
        vec![
            (
                "pointer",
                BackendEvent::Input(WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: PhysicalPosition::new(10.0, 10.0),
                }),
            ),
            (
                "relative pointer under a game's pointer lock",
                BackendEvent::RelativePointerMotion {
                    dx: 1.0,
                    dy: 0.0,
                    dx_unaccel: 1.0,
                    dy_unaccel: 0.0,
                    time_usec: 1,
                },
            ),
            ("pen moving over an app", pen(TabletPhase::Motion)),
            ("pen tip down in an app", pen(TabletPhase::TipDown)),
            (
                "tablet pad button",
                BackendEvent::TabletPad(PadEvent {
                    pad: "pad".into(),
                    action: PadAction::Button {
                        button: 0,
                        pressed: true,
                    },
                    time_msec: 1,
                    fallback: Vec::new(),
                }),
            ),
            (
                "touch to an app",
                BackendEvent::Touch(TouchEvent::Motion {
                    slot: 0,
                    x: 5.0,
                    y: 5.0,
                }),
            ),
            (
                "two-finger pinch to an app",
                BackendEvent::Gesture(GestureEvent::PinchUpdate {
                    dx: 0.0,
                    dy: 0.0,
                    scale: 1.1,
                    rotation: 0.0,
                }),
            ),
            (
                "two-finger swipe to an app",
                BackendEvent::Gesture(GestureEvent::SwipeUpdate { dx: 3.0, dy: 0.0 }),
            ),
            (
                "touchpad hold to an app",
                BackendEvent::Gesture(GestureEvent::HoldBegin { fingers: 2 }),
            ),
            (
                "three-finger gesture the shell takes",
                BackendEvent::ShellGesture(GestureAction::ShowOverview),
            ),
            ("folding into tablet mode", BackendEvent::TabletMode(true)),
        ]
    }

    const BLANK: u64 = 600;
    const LOCK: u64 = 900;
    const SUSPEND: u64 = 1200;

    fn session(t0: Instant) -> SessionManager {
        SessionManager::new(
            IdleConfig {
                blank_timeout: Some(Duration::from_secs(BLANK)),
                lock_timeout: Some(Duration::from_secs(LOCK)),
                suspend_timeout: Some(Duration::from_secs(SUSPEND)),
            },
            t0,
        )
    }

    #[test]
    fn every_kind_of_input_restarts_dim_blank_lock_and_suspend() {
        for (kind, event) in person_inputs() {
            let t0 = Instant::now();
            let at = |secs| t0 + Duration::from_secs(secs);
            let mut session = session(t0);
            // Drawing or gesturing for almost ten minutes: the warning dim is up.
            assert_eq!(session.tick(at(BLANK - 5)), vec![SessionTransition::Dimmed]);
            let woke = note_person_input(&mut session, [&event], at(BLANK - 5));
            assert_eq!(
                woke,
                vec![SessionTransition::Undimmed],
                "{kind}: input lifts the dim"
            );
            assert_eq!(session.idle_for(at(BLANK - 5)), Duration::ZERO, "{kind}");
            for (deadline, due) in [
                (BLANK, SessionTransition::Blanked),
                (LOCK, SessionTransition::Locked),
                (SUSPEND, SessionTransition::Suspend),
            ] {
                let out = session.tick(at(deadline));
                assert!(
                    !out.contains(&due),
                    "{kind}: {due:?} counts from the input, not from {deadline} s after the start"
                );
            }
            assert!(!session.is_locked(), "{kind}: still unlocked");
        }
    }

    #[test]
    fn input_on_a_blanked_lock_wakes_the_panel_and_keeps_the_lock() {
        for (kind, event) in person_inputs() {
            let t0 = Instant::now();
            let mut session = session(t0);
            session.tick(t0 + Duration::from_secs(SUSPEND - 1));
            assert!(session.is_locked() && session.is_blanked());
            let woke = note_person_input(&mut session, [&event], t0 + Duration::from_secs(SUSPEND));
            assert!(woke.contains(&SessionTransition::Woke), "{kind}: {woke:?}");
            assert!(session.is_locked() && !session.is_blanked(), "{kind}");
        }
    }

    #[test]
    fn bookkeeping_without_input_leaves_the_idle_timer_alone() {
        let t0 = Instant::now();
        let mut session = session(t0);
        let later = t0 + Duration::from_secs(60);
        let events = [
            BackendEvent::Redraw,
            BackendEvent::OutputsChanged,
            BackendEvent::Focus(true),
            BackendEvent::InputStateReset,
            BackendEvent::TabletPadRemoved("pad".into()),
            BackendEvent::InputDeviceRemoved { id: "kbd".into() },
            BackendEvent::PointerMotionUsed,
        ];
        assert!(note_person_input(&mut session, &events, later).is_empty());
        assert_eq!(session.idle_for(later), Duration::from_secs(60));
        // One input anywhere in the batch counts.
        let mixed = [BackendEvent::Redraw, pen(TabletPhase::ProximityIn)];
        note_person_input(&mut session, &mixed, later);
        assert_eq!(session.idle_for(later), Duration::ZERO);
    }
}
