//! Touch for whatever does not take touch itself: the shell's own chrome and
//! clients that never asked for a `wl_touch`.
//!
//! A contact that a touch-capable client received over `wl_touch` is that
//! client's alone, and a chrome widget that handles raw touch (a text field
//! placing its caret, the window strip panning) reads the raw stream. Every
//! other contact becomes ordinary pointer input, in one of two ways:
//!
//! * On something you drag — a resize edge, a window's grab area — the
//!   finger is the pointer held down: press where it lands, move with it,
//!   release where it lifts. Windows and splits follow the finger.
//! * Everywhere else, the same [`ui::TouchPointerEmulator`] Tilo
//!   applications use: a tap activates, a drag scrolls (and keeps gliding
//!   when thrown), a still hold opens the context menu, and two fingers pan
//!   or pinch.
//!
//! The synthesized events go through the compositor's ordinary pointer path,
//! which hit-tests chrome and touch-less clients alike. They never move the
//! physical pointer; the cursor stays hidden while touch is the input in use
//! (see `client_input::sync_pointer_image`).

use std::collections::HashMap;

use winit::event::{DeviceId, ElementState, MouseButton, Touch, TouchPhase, WindowEvent};
use winit::window::CursorIcon;

/// Who a contact that begins at a point belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchRoute {
    /// A touch-capable client or a raw-touch chrome widget reads it.
    Raw,
    /// Tap, scroll, hold and pinch, as in Tilo applications.
    Gestures,
    /// The pointer held down, following the finger.
    Drag,
}

impl TouchRoute {
    /// The route for a contact that nobody takes raw, from the cursor the
    /// chrome names at that point: what shows a grab or resize cursor under
    /// the mouse is dragged by the finger.
    pub fn for_cursor(cursor: Option<CursorIcon>) -> Self {
        match cursor {
            Some(
                CursorIcon::Grab
                | CursorIcon::Grabbing
                | CursorIcon::Move
                | CursorIcon::AllScroll
                | CursorIcon::ColResize
                | CursorIcon::RowResize
                | CursorIcon::EwResize
                | CursorIcon::NsResize
                | CursorIcon::NeswResize
                | CursorIcon::NwseResize
                | CursorIcon::EResize
                | CursorIcon::WResize
                | CursorIcon::NResize
                | CursorIcon::SResize
                | CursorIcon::NeResize
                | CursorIcon::NwResize
                | CursorIcon::SeResize
                | CursorIcon::SwResize,
            ) => Self::Drag,
            _ => Self::Gestures,
        }
    }
}

/// The pointer input one raw contact update stands for, if any (see
/// [`crate::compositor::touch_pointer`]), for the DRM backend and test
/// control alike. A contact is translated when it begins outside every
/// touch-capable client (`taken_by_client` is
/// [`crate::compositor::state::State::touch_down_taken`]) and on no chrome
/// widget that handles raw touch itself; a shell gesture claiming the
/// stream later withdraws whatever the translation still had pending.
pub(crate) fn pointer_events_for_touch(
    touch_pointer: &mut TouchPointer,
    touch: &winit::event::Touch,
    chrome_handled: bool,
    taken_by_client: bool,
    fb: &crate::compositor::frame::FrameBuilder,
) -> Vec<WindowEvent> {
    if chrome_handled
        && touch.phase == winit::event::TouchPhase::Moved
        && touch_pointer.is_emulated(touch.id)
    {
        return touch_pointer.cancel(touch);
    }
    touch_pointer.events(touch, || {
        touch_route(
            chrome_handled || taken_by_client,
            fb,
            touch.location.x,
            touch.location.y,
        )
    })
}

/// How a contact that begins at `(x, y)` is handled: raw by whoever already
/// took it; as the pointer held down on a shell surface that reads press,
/// drag and release (selecting what to capture, marking up a screenshot);
/// else by what the chrome there does with a pointer.
fn touch_route(
    taken: bool,
    fb: &crate::compositor::frame::FrameBuilder,
    x: f64,
    y: f64,
) -> TouchRoute {
    let point = vello::kurbo::Point::new(x, y);
    let route = if taken || fb.claims_raw_touch_at(point) {
        TouchRoute::Raw
    } else if fb.host().touch_works_as_held_pointer() {
        TouchRoute::Drag
    } else {
        TouchRoute::for_cursor(fb.chrome_cursor_at(point))
    };
    // A finger's route decides whether chrome under it ever sees a click;
    // rare enough to say every time, in the log a device keeps.
    tracing::info!(target: tilo_startup_metrics::INPUT_ROUTE_TARGET, x, y, taken, ?route, "touch contact routed");
    route
}

/// Per-session translation of raw touch into pointer input.
#[derive(Default)]
pub struct TouchPointer {
    emulator: ui::TouchPointerEmulator,
    /// Contacts being translated, by touch id, and how.
    routes: HashMap<u64, TouchRoute>,
}

impl TouchPointer {
    /// Translate one raw contact update. `route` is asked once, when a
    /// contact begins; [`TouchRoute::Raw`] leaves the whole contact to
    /// whoever takes raw touch (a `wl_touch` client or a chrome widget).
    pub fn events(
        &mut self,
        touch: &Touch,
        route: impl FnOnce() -> TouchRoute,
    ) -> Vec<WindowEvent> {
        if touch.phase == TouchPhase::Started {
            // One dragging finger at a time: a second finger never starts
            // another press while the first holds the pointer down.
            let route = match route() {
                TouchRoute::Drag if self.dragging().is_some() => TouchRoute::Gestures,
                route => route,
            };
            if route == TouchRoute::Drag {
                self.routes.insert(touch.id, route);
                return Self::drag_start(touch);
            }
            let mut claimed = false;
            let events = self.emulator.events(touch, |_| {
                claimed = route == TouchRoute::Raw;
                claimed
            });
            if !claimed {
                self.routes.insert(touch.id, TouchRoute::Gestures);
            }
            return Self::window_events(touch.device_id, events);
        }
        if self.routes.get(&touch.id) == Some(&TouchRoute::Drag) {
            return self.drag(touch);
        }
        let events = self.emulator.events(touch, |_| true);
        if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.routes.remove(&touch.id);
            // What the lift turned into: a tap that clicked says so beside its
            // route, and one that did not says what it was instead.
            let clicked = events
                .iter()
                .any(|event| matches!(event, ui::TouchPointerEvent::Press(_)));
            tracing::info!(
                target: tilo_startup_metrics::INPUT_ROUTE_TARGET,
                x = touch.location.x,
                y = touch.location.y,
                phase = ?touch.phase,
                clicked,
                events = events.len(),
                "touch contact lifted"
            );
        }
        Self::window_events(touch.device_id, events)
    }

    /// Withdraw every contact being translated, as if each had been
    /// cancelled: a shell gesture took the stream over mid-way, so a pending
    /// tap, scroll, pinch or drag must not land as well.
    pub fn cancel(&mut self, template: &Touch) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        let mut ids: Vec<u64> = self.routes.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let touch = Touch {
                id,
                phase: TouchPhase::Cancelled,
                ..*template
            };
            out.extend(self.events(&touch, || TouchRoute::Raw));
        }
        out
    }

    /// Whether `id` is being translated into pointer input.
    pub fn is_emulated(&self, id: u64) -> bool {
        self.routes.contains_key(&id)
    }

    /// Fire a long press whose hold time has passed.
    pub fn poll(&mut self, now: std::time::Instant) -> Vec<WindowEvent> {
        self.emulator
            .poll(now)
            .map(|(device_id, events)| {
                tracing::info!(
                    target: tilo_startup_metrics::INPUT_ROUTE_TARGET,
                    events = events.len(),
                    "touch hold became a long press"
                );
                Self::window_events(device_id, events)
            })
            .unwrap_or_default()
    }

    /// When [`Self::poll`] next has something to do.
    pub fn deadline(&self) -> Option<std::time::Instant> {
        self.emulator.long_press_deadline()
    }

    fn dragging(&self) -> Option<u64> {
        self.routes
            .iter()
            .find_map(|(id, route)| (*route == TouchRoute::Drag).then_some(*id))
    }

    fn drag_start(touch: &Touch) -> Vec<WindowEvent> {
        vec![
            WindowEvent::CursorMoved {
                device_id: touch.device_id,
                position: touch.location,
            },
            Self::button(touch.device_id, ElementState::Pressed),
        ]
    }

    fn drag(&mut self, touch: &Touch) -> Vec<WindowEvent> {
        let moved = WindowEvent::CursorMoved {
            device_id: touch.device_id,
            position: touch.location,
        };
        match touch.phase {
            TouchPhase::Started | TouchPhase::Moved => vec![moved],
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.routes.remove(&touch.id);
                let mut out = Vec::new();
                if touch.phase == TouchPhase::Ended {
                    out.push(moved);
                }
                out.push(Self::button(touch.device_id, ElementState::Released));
                out.push(WindowEvent::CursorLeft {
                    device_id: touch.device_id,
                });
                out
            }
        }
    }

    fn button(device_id: DeviceId, state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        }
    }

    fn window_events(device_id: DeviceId, events: Vec<ui::TouchPointerEvent>) -> Vec<WindowEvent> {
        events
            .into_iter()
            // The emulator parks a lifted finger's pointer off-screen so no
            // hover lingers under it. The compositor's pointer path clamps
            // positions to the outputs, which would turn that into a hover
            // at the corner instead; a lifted finger leaves instead.
            .map(|event| match event {
                ui::TouchPointerEvent::Move(position) if position.x < 0.0 && position.y < 0.0 => {
                    WindowEvent::CursorLeft { device_id }
                }
                other => other.window_event(device_id),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::MouseScrollDelta;

    fn touch(id: u64, phase: TouchPhase, x: f64, y: f64) -> Touch {
        Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id,
        }
    }

    fn presses(events: &[WindowEvent]) -> Vec<(ElementState, MouseButton)> {
        events
            .iter()
            .filter_map(|event| match event {
                WindowEvent::MouseInput { state, button, .. } => Some((*state, *button)),
                _ => None,
            })
            .collect()
    }

    fn moves(events: &[WindowEvent]) -> Vec<(f64, f64)> {
        events
            .iter()
            .filter_map(|event| match event {
                WindowEvent::CursorMoved { position, .. } => Some((position.x, position.y)),
                _ => None,
            })
            .collect()
    }

    const GESTURES: fn() -> TouchRoute = || TouchRoute::Gestures;

    #[test]
    fn a_tap_on_chrome_is_one_click_where_the_finger_was() {
        let mut pointer = TouchPointer::default();
        let mut out = pointer.events(&touch(1, TouchPhase::Started, 40.0, 50.0), GESTURES);
        assert!(pointer.is_emulated(1));
        out.extend(pointer.events(&touch(1, TouchPhase::Ended, 41.0, 50.0), GESTURES));
        assert_eq!(
            presses(&out),
            [
                (ElementState::Pressed, MouseButton::Left),
                (ElementState::Released, MouseButton::Left)
            ]
        );
        assert_eq!(moves(&out).first(), Some(&(41.0, 50.0)));
        assert!(
            matches!(out.last(), Some(WindowEvent::CursorLeft { .. })),
            "the lifted finger leaves no hover behind: {out:?}"
        );
        assert!(!pointer.is_emulated(1));
    }

    #[test]
    fn a_contact_a_client_or_widget_takes_produces_no_pointer_input() {
        let mut pointer = TouchPointer::default();
        let mut out = pointer.events(&touch(3, TouchPhase::Started, 40.0, 50.0), || {
            TouchRoute::Raw
        });
        out.extend(pointer.events(&touch(3, TouchPhase::Moved, 140.0, 50.0), GESTURES));
        out.extend(pointer.events(&touch(3, TouchPhase::Ended, 140.0, 50.0), GESTURES));
        assert!(out.is_empty(), "{out:?}");
        assert!(!pointer.is_emulated(3));
    }

    #[test]
    fn a_drag_scrolls_and_a_hold_is_a_secondary_click() {
        let mut pointer = TouchPointer::default();
        let mut out = pointer.events(&touch(1, TouchPhase::Started, 100.0, 300.0), GESTURES);
        out.extend(pointer.events(&touch(1, TouchPhase::Moved, 100.0, 200.0), GESTURES));
        out.extend(pointer.events(&touch(1, TouchPhase::Ended, 100.0, 200.0), GESTURES));
        assert!(presses(&out).is_empty(), "a drag never clicks: {out:?}");
        let scrolled: f64 = out
            .iter()
            .filter_map(|event| match event {
                WindowEvent::MouseWheel {
                    delta: MouseScrollDelta::PixelDelta(delta),
                    ..
                } => Some(delta.y),
                _ => None,
            })
            .sum();
        assert!((scrolled + 100.0).abs() < f64::EPSILON, "{out:?}");
        assert!(out.iter().any(|event| matches!(
            event,
            WindowEvent::MouseWheel {
                phase: TouchPhase::Ended,
                ..
            }
        )));

        let started = std::time::Instant::now();
        pointer.events(&touch(2, TouchPhase::Started, 60.0, 60.0), GESTURES);
        let deadline = pointer.deadline().expect("a still finger arms the hold");
        assert!(deadline >= started);
        let held = pointer.poll(deadline + std::time::Duration::from_millis(1));
        assert_eq!(
            presses(&held),
            [
                (ElementState::Pressed, MouseButton::Right),
                (ElementState::Released, MouseButton::Right)
            ]
        );
        let lifted = pointer.events(&touch(2, TouchPhase::Ended, 60.0, 60.0), GESTURES);
        assert!(
            presses(&lifted).is_empty(),
            "the lift after a hold does nothing"
        );
    }

    #[test]
    fn a_finger_on_a_resize_edge_drags_it() {
        assert_eq!(
            TouchRoute::for_cursor(Some(CursorIcon::EwResize)),
            TouchRoute::Drag
        );
        assert_eq!(
            TouchRoute::for_cursor(Some(CursorIcon::Grab)),
            TouchRoute::Drag
        );
        assert_eq!(
            TouchRoute::for_cursor(Some(CursorIcon::Pointer)),
            TouchRoute::Gestures
        );
        assert_eq!(TouchRoute::for_cursor(None), TouchRoute::Gestures);

        let mut pointer = TouchPointer::default();
        let mut out = pointer.events(&touch(4, TouchPhase::Started, 500.0, 300.0), || {
            TouchRoute::Drag
        });
        out.extend(pointer.events(&touch(4, TouchPhase::Moved, 540.0, 302.0), GESTURES));
        out.extend(pointer.events(&touch(4, TouchPhase::Moved, 620.0, 305.0), GESTURES));
        out.extend(pointer.events(&touch(4, TouchPhase::Ended, 620.0, 305.0), GESTURES));
        assert_eq!(
            presses(&out),
            [
                (ElementState::Pressed, MouseButton::Left),
                (ElementState::Released, MouseButton::Left)
            ],
            "{out:?}"
        );
        assert_eq!(
            moves(&out),
            [
                (500.0, 300.0),
                (540.0, 302.0),
                (620.0, 305.0),
                (620.0, 305.0)
            ],
            "the pointer is held down from where the finger landed to where it lifted"
        );
        assert!(matches!(out.last(), Some(WindowEvent::CursorLeft { .. })));
        assert!(!pointer.is_emulated(4));

        let template = touch(6, TouchPhase::Started, 10.0, 10.0);
        pointer.events(&template, || TouchRoute::Drag);
        let withdrawn = pointer.cancel(&template);
        assert_eq!(
            presses(&withdrawn),
            [(ElementState::Released, MouseButton::Left)]
        );
        assert!(!pointer.is_emulated(6));
    }

    #[test]
    fn a_gesture_taking_over_withdraws_the_pending_tap() {
        let mut pointer = TouchPointer::default();
        let template = touch(5, TouchPhase::Started, 10.0, 10.0);
        pointer.events(&template, GESTURES);
        let withdrawn = pointer.cancel(&template);
        assert!(presses(&withdrawn).is_empty());
        assert!(!pointer.is_emulated(5));
        let after = pointer.events(&touch(5, TouchPhase::Ended, 10.0, 10.0), GESTURES);
        assert!(presses(&after).is_empty(), "{after:?}");
    }
}
