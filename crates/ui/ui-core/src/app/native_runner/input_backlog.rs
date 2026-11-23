//! Input the platform delivered faster than the window handles it. The event loop hands over a
//! wake's events before the window may draw, so keys that come faster than the app takes them (a
//! paste typed out key by key) would show nothing until the queue is empty. Input is routed in order
//! until one refresh of the display went to it since the last frame; the rest waits for the next
//! wake, so typed text shows up frame by frame. Without a frame for a few refreshes (an occluded
//! window, a renderer retrying), input goes on without one. Wheel deltas of one device and kind in
//! a gesture are summed while they wait, and a delta last in its wake waits for the wake's end, so
//! the next can join it. Events that are not input never wait.

use std::collections::VecDeque;

use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};
use winit::window::WindowId;

use crate::time::{Duration, Instant};

/// How many refreshes without a frame before input goes on without one.
const REFRESHES_WITHOUT_A_FRAME: u32 = 4;

/// Input waiting for its turn, and the time input took since the last frame.
#[derive(Debug, Default)]
pub(super) struct InputBacklog {
    /// Each event with the serial of the input it is, when it is input: what the shell is told
    /// the window handled once the event is routed, never while it waits.
    queue: VecDeque<(WindowId, WindowEvent, Option<u32>)>,
    spent: Duration,
    since: Option<Instant>,
}

impl InputBacklog {
    /// Whether `event` is input, which takes its place behind input that came before it.
    pub(super) fn waits_in_line(event: &WindowEvent) -> bool {
        !matches!(
            event,
            WindowEvent::RedrawRequested
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Occluded(_)
                | WindowEvent::ThemeChanged(_)
                | WindowEvent::Moved(_)
                | WindowEvent::Destroyed
        )
    }

    /// Queue `event` for `window` behind the input before it; a wheel delta
    /// that continues the one last in line joins it.
    pub(super) fn push(&mut self, window: WindowId, event: WindowEvent, serial: Option<u32>) {
        if let Some((last_window, last, _)) = self.queue.back_mut()
            && *last_window == window
            && merge(last, &event)
        {
            return;
        }
        self.queue.push_back((window, event, serial));
    }

    /// Route waiting input in order while the frame's `budget` lasts, by the clock `now`. Before
    /// the wake's end (`wake_ends` false) a wheel delta last in line stays for the next to join.
    pub(super) fn route(
        &mut self,
        budget: Duration,
        wake_ends: bool,
        now: impl Fn() -> Instant,
        mut route: impl FnMut(WindowId, WindowEvent, Option<u32>),
    ) {
        let start = now();
        if self.since.is_none_or(|since| {
            start.saturating_duration_since(since)
                >= budget.saturating_mul(REFRESHES_WITHOUT_A_FRAME)
        }) {
            self.frame_started(start);
        }
        while self.spent < budget {
            if !wake_ends
                && self.queue.len() == 1
                && matches!(
                    self.queue[0].1,
                    WindowEvent::MouseWheel {
                        phase: TouchPhase::Moved,
                        ..
                    }
                )
            {
                break;
            }
            let Some((window, event, serial)) = self.queue.pop_front() else {
                break;
            };
            let started = now();
            route(window, event, serial);
            self.spent = self
                .spent
                .saturating_add(now().saturating_duration_since(started));
        }
    }

    /// The window began a frame at `now`: the next refresh's worth of input may go.
    pub(super) fn frame_started(&mut self, now: Instant) {
        self.spent = Duration::ZERO;
        self.since = Some(now);
    }

    /// Whether input waits for the next frame.
    pub(super) fn waiting(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Whether input was routed since the last frame began: what the person
    /// typed is on its way to the frame that shows it.
    pub(super) fn answered_since_frame(&self) -> bool {
        self.waiting() || self.spent > Duration::ZERO
    }

    /// When waiting input goes on even if no frame came.
    pub(super) fn wake_at(&self, budget: Duration) -> Option<Instant> {
        self.since
            .filter(|_| self.waiting())
            .and_then(|since| since.checked_add(budget.saturating_mul(REFRESHES_WITHOUT_A_FRAME)))
    }
}

/// Add `next` to `into` when both are wheel deltas inside a gesture (or a
/// detented wheel's notches) from the same device, of the same kind; returns
/// whether it did.
fn merge(into: &mut WindowEvent, next: &WindowEvent) -> bool {
    let (
        WindowEvent::MouseWheel {
            device_id,
            delta,
            phase: TouchPhase::Moved,
        },
        WindowEvent::MouseWheel {
            device_id: next_device,
            delta: next_delta,
            phase: TouchPhase::Moved,
        },
    ) = (into, next)
    else {
        return false;
    };
    if device_id != next_device {
        return false;
    }
    match (delta, next_delta) {
        (MouseScrollDelta::LineDelta(x, y), MouseScrollDelta::LineDelta(nx, ny)) => {
            *x += nx;
            *y += ny;
            true
        }
        (MouseScrollDelta::PixelDelta(at), MouseScrollDelta::PixelDelta(next)) => {
            at.x += next.x;
            at.y += next.y;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::headless::FrameHarness;
    use crate::widget::{TextInput, Widget as _};
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState};
    use winit::keyboard::Key;

    fn pixels(dy: f64, phase: TouchPhase) -> WindowEvent {
        let delta = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy));
        let device_id = DeviceId::dummy();
        WindowEvent::MouseWheel {
            device_id,
            delta,
            phase,
        }
    }

    fn lines(dy: f32) -> WindowEvent {
        let delta = MouseScrollDelta::LineDelta(0.0, dy);
        let device_id = DeviceId::dummy();
        WindowEvent::MouseWheel {
            device_id,
            delta,
            phase: TouchPhase::Moved,
        }
    }

    /// Everything that may go now, in order, as wheel distances (`None`: not a wheel).
    fn routed(backlog: &mut InputBacklog, wake_ends: bool) -> Vec<Option<f64>> {
        let mut out = Vec::new();
        backlog.route(Duration::MAX, wake_ends, crate::time::now, |_, event, _| {
            out.push(match event {
                WindowEvent::MouseWheel {
                    delta: MouseScrollDelta::PixelDelta(at),
                    ..
                } => Some(at.y),
                WindowEvent::MouseWheel {
                    delta: MouseScrollDelta::LineDelta(_, y),
                    ..
                } => Some(f64::from(y)),
                _ => None,
            });
        });
        out
    }

    tilo_test_env::scenario! {
        fn wheel_deltas_in_one_wake_are_summed_in_order;

        phase deltas_in_one_wake_are_handed_on_as_their_sum;
        {
            let mut backlog = InputBacklog::default();
            for dy in [-3.0, -4.0, -5.0] {
                backlog.push(WindowId::dummy(), pixels(dy, TouchPhase::Moved), None);
                assert!(routed(&mut backlog, false).is_empty(), "the sum waits for the wake's end");
            }
            assert_eq!(routed(&mut backlog, true), [Some(-12.0)]);
            backlog.push(WindowId::dummy(), lines(-1.0), None);
            backlog.push(WindowId::dummy(), lines(-1.0), None);
            assert_eq!(routed(&mut backlog, true), [Some(-2.0)]);
        }

        phase another_event_or_kind_hands_the_sum_on_first_and_keeps_the_order;
        {
            let mut backlog = InputBacklog::default();
            backlog.push(WindowId::dummy(), pixels(-3.0, TouchPhase::Moved), None);
            backlog.push(WindowId::dummy(), pixels(-4.0, TouchPhase::Moved), None);
            backlog.push(WindowId::dummy(), pixels(0.0, TouchPhase::Ended), None);
            assert_eq!(routed(&mut backlog, false), [Some(-7.0), Some(0.0)]);
            backlog.push(WindowId::dummy(), pixels(-3.0, TouchPhase::Moved), None);
            backlog.push(WindowId::dummy(), lines(-1.0), None);
            assert_eq!(routed(&mut backlog, false), [Some(-3.0)], "lines do not join pixels");
            assert_eq!(routed(&mut backlog, true), [Some(-1.0)]);
            backlog.push(WindowId::dummy(), lines(-1.0), None);
            backlog.push(WindowId::dummy(), WindowEvent::Focused(false), None);
            assert_eq!(routed(&mut backlog, false), [Some(-1.0), None]);
        }

        phase a_gesture_start_is_never_summed;
        {
            let mut backlog = InputBacklog::default();
            backlog.push(WindowId::dummy(), pixels(-2.0, TouchPhase::Started), None);
            assert_eq!(routed(&mut backlog, false), [Some(-2.0)], "a start routes at once");
            backlog.push(WindowId::dummy(), pixels(-3.0, TouchPhase::Moved), None);
            assert!(routed(&mut backlog, false).is_empty());
        }

        phase a_key_routed_since_the_last_frame_waits_for_the_frame_that_shows_it;
        // What the person typed since the last frame began is owed a frame:
        // a display that does not send that frame's callback robs them of
        // the answer, so the host may paint it without one.
        {
            let start = crate::time::now();
            let clock = Cell::new(start);
            let mut backlog = InputBacklog::default();
            assert!(!backlog.answered_since_frame(), "nothing typed yet");
            backlog.push(
                WindowId::dummy(),
                crate::synthetic_input::key_input(
                    &Key::Character("a".into()),
                    ElementState::Pressed,
                ),
                None,
            );
            backlog.route(Duration::MAX, true, || clock.get(), |_, _, _| {
                clock.set(clock.get() + Duration::from_millis(1));
            });
            assert!(
                backlog.answered_since_frame(),
                "the key went into the window and its frame is owed"
            );
            backlog.frame_started(start + Duration::from_millis(2));
            assert!(
                !backlog.answered_since_frame(),
                "the frame that shows the key took it"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_burst_of_typing_shows_frame_by_frame_in_order;

        phase every_frame_answers_at_most_a_refresh_of_input;
        {

            const KEYS: usize = 2_000;
            // Each key costs the app a millisecond: the platform delivers them faster than that.
            let per_event = Duration::from_millis(1);
            let budget = Duration::from_millis(16);
            let start = crate::time::now();
            let clock = Cell::new(start);
            let now = || clock.get();
            let frame = RefCell::new(FrameHarness::for_root(
                crate::State::new(),
                TextInput::local(),
                (400, 80),
                crate::Theme::dark(),
            ));
            frame.borrow_mut().set_coalesce_input_frames(true);
            frame.borrow_mut().root_mut::<TextInput>().set_focus(true);
            let route = |_: WindowId, event: WindowEvent, _: Option<u32>| {
                frame.borrow_mut().send_host_event(&event);
                clock.set(clock.get() + per_event);
            };

            // The platform read every press and release at once: one wake hands them all over
            // before the window may draw.
            let typed: String = "the quick brown fox ".chars().cycle().take(KEYS).collect();
            let mut backlog = InputBacklog::default();
            for letter in typed.chars() {
                for state in [ElementState::Pressed, ElementState::Released] {
                    let key = Key::Character(letter.to_string().into());
                    backlog.push(
                        WindowId::dummy(),
                        crate::synthetic_input::key_input(&key, state),
                        None,
                    );
                    backlog.route(budget, false, now, route);
                }
            }
            assert!(now() - start <= budget, "the first frame waited {:?}", now() - start);

            // Then each wake draws a frame and routes the next refresh's worth.
            let (mut frames, mut shown, mut last) = (0_u32, String::new(), start);
            loop {
                assert!(now() - last <= budget, "frame {frames} came {:?} late", now() - last);
                last = now();
                backlog.frame_started(last);
                frame.borrow_mut().poll_frame().unwrap();
                frames += 1;
                let value = frame.borrow().root::<TextInput>().value().to_owned();
                assert!(
                    value.len() >= shown.len() && typed.starts_with(&value),
                    "frame {frames} shows the text in the order it was typed"
                );
                shown = value;
                if !backlog.waiting() {
                    break;
                }
                backlog.route(budget, true, now, route);
            }
            assert_eq!(shown, typed, "every key arrived");
        }
    }
}
