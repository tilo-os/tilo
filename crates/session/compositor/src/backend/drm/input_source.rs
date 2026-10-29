//! libinput's events, tablet pads included.
//!
//! smithay's libinput source hands on keyboards, pointers, touch, gestures,
//! pens and switches but drops tablet pads' buttons, rings and strips. This
//! source reads the same libinput context and delivers both: everything
//! smithay models as its [`InputEvent`], pads as [`PadEvent`].

use std::os::fd::AsFd as _;

use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, PointerScrollAxis};
use smithay::reexports::calloop::{
    EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory,
};
use smithay::reexports::input::event::tablet_pad::TabletPadEvent as LibinputPadEvent;
use smithay::reexports::input::event::{self, EventTrait as _};
use smithay::reexports::input::{self as libinput, Libinput};

/// One event of a tablet pad.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PadEvent {
    /// A button (numbered from zero) changed state.
    Button { button: u32, pressed: bool },
    /// A finger on a ring, at `degrees` (clockwise from the top), or lifted
    /// (`None`).
    Ring { ring: u32, degrees: Option<f64> },
    /// A finger on a strip, at `position` (`0.0` top or left to `1.0`), or
    /// lifted.
    Strip { strip: u32, position: Option<f64> },
}

/// What the source delivers.
pub(crate) enum Delivered {
    Device(InputEvent<LibinputInputBackend>),
    Pad {
        device: libinput::Device,
        event: PadEvent,
    },
}

/// The libinput context as a loop source.
pub(crate) struct Source {
    context: Libinput,
    token: Option<Token>,
}

impl Source {
    pub(crate) const fn new(context: Libinput) -> Self {
        Self {
            context,
            token: None,
        }
    }
}

fn pad_event(event: LibinputPadEvent) -> Option<(libinput::Device, PadEvent)> {
    let device = event.device();
    let pad = match event {
        LibinputPadEvent::Button(event) => PadEvent::Button {
            button: event.button_number(),
            pressed: event.button_state() == event::tablet_pad::ButtonState::Pressed,
        },
        LibinputPadEvent::Ring(event) => PadEvent::Ring {
            ring: event.number(),
            degrees: Some(event.position()).filter(|position| *position >= 0.0),
        },
        LibinputPadEvent::Strip(event) => PadEvent::Strip {
            strip: event.number(),
            position: Some(event.position()).filter(|position| *position >= 0.0),
        },
        _ => return None,
    };
    Some((device, pad))
}

/// smithay's model of a libinput event, as its own source makes it.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per libinput event kind, as smithay's source has them"
)]
fn device_event(event: libinput::Event) -> Option<InputEvent<LibinputInputBackend>> {
    use event::gesture::{GestureHoldEvent, GesturePinchEvent, GestureSwipeEvent};
    Some(match event {
        libinput::Event::Device(event::DeviceEvent::Added(event)) => InputEvent::DeviceAdded {
            device: event.device(),
        },
        libinput::Event::Device(event::DeviceEvent::Removed(event)) => InputEvent::DeviceRemoved {
            device: event.device(),
        },
        libinput::Event::Touch(event::TouchEvent::Down(event)) => InputEvent::TouchDown { event },
        libinput::Event::Touch(event::TouchEvent::Motion(event)) => {
            InputEvent::TouchMotion { event }
        }
        libinput::Event::Touch(event::TouchEvent::Up(event)) => InputEvent::TouchUp { event },
        libinput::Event::Touch(event::TouchEvent::Cancel(event)) => {
            InputEvent::TouchCancel { event }
        }
        libinput::Event::Touch(event::TouchEvent::Frame(event)) => InputEvent::TouchFrame { event },
        libinput::Event::Keyboard(event::KeyboardEvent::Key(event)) => {
            InputEvent::Keyboard { event }
        }
        libinput::Event::Pointer(event::PointerEvent::Motion(event)) => {
            InputEvent::PointerMotion { event }
        }
        libinput::Event::Pointer(event::PointerEvent::MotionAbsolute(event)) => {
            InputEvent::PointerMotionAbsolute { event }
        }
        libinput::Event::Pointer(event::PointerEvent::ScrollWheel(event)) => {
            InputEvent::PointerAxis {
                event: PointerScrollAxis::Wheel(event),
            }
        }
        libinput::Event::Pointer(event::PointerEvent::ScrollFinger(event)) => {
            InputEvent::PointerAxis {
                event: PointerScrollAxis::Finger(event),
            }
        }
        libinput::Event::Pointer(event::PointerEvent::ScrollContinuous(event)) => {
            InputEvent::PointerAxis {
                event: PointerScrollAxis::Continuous(event),
            }
        }
        libinput::Event::Pointer(event::PointerEvent::Button(event)) => {
            InputEvent::PointerButton { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Swipe(GestureSwipeEvent::Begin(event))) => {
            InputEvent::GestureSwipeBegin { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Swipe(GestureSwipeEvent::Update(event))) => {
            InputEvent::GestureSwipeUpdate { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Swipe(GestureSwipeEvent::End(event))) => {
            InputEvent::GestureSwipeEnd { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Pinch(GesturePinchEvent::Begin(event))) => {
            InputEvent::GesturePinchBegin { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Pinch(GesturePinchEvent::Update(event))) => {
            InputEvent::GesturePinchUpdate { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Pinch(GesturePinchEvent::End(event))) => {
            InputEvent::GesturePinchEnd { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Hold(GestureHoldEvent::Begin(event))) => {
            InputEvent::GestureHoldBegin { event }
        }
        libinput::Event::Gesture(event::GestureEvent::Hold(GestureHoldEvent::End(event))) => {
            InputEvent::GestureHoldEnd { event }
        }
        libinput::Event::Tablet(event::TabletToolEvent::Axis(event)) => {
            InputEvent::TabletToolAxis { event }
        }
        libinput::Event::Tablet(event::TabletToolEvent::Proximity(event)) => {
            InputEvent::TabletToolProximity { event }
        }
        libinput::Event::Tablet(event::TabletToolEvent::Tip(event)) => {
            InputEvent::TabletToolTip { event }
        }
        libinput::Event::Tablet(event::TabletToolEvent::Button(event)) => {
            InputEvent::TabletToolButton { event }
        }
        libinput::Event::Switch(event::SwitchEvent::Toggle(event)) => {
            InputEvent::SwitchToggle { event }
        }
        _ => return None,
    })
}

impl EventSource for Source {
    type Event = Delivered;
    type Metadata = ();
    type Ret = ();
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        _readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> std::io::Result<PostAction>
    where
        F: FnMut(Delivered, &mut ()),
    {
        if Some(token) != self.token {
            return Ok(PostAction::Continue);
        }
        self.context.dispatch()?;
        for event in &mut self.context {
            let delivered = match event {
                libinput::Event::TabletPad(pad) => {
                    pad_event(pad).map(|(device, event)| Delivered::Pad { device, event })
                }
                other => device_event(other).map(Delivered::Device),
            };
            if let Some(delivered) = delivered {
                callback(delivered, &mut ());
            }
        }
        Ok(PostAction::Continue)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        factory: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        let token = factory.token();
        self.token = Some(token);
        // SAFETY: the descriptor is the context's, which this source owns
        // and which outlives the registration (it is unregistered before
        // the source drops).
        unsafe { poll.register(self.context.as_fd(), Interest::READ, Mode::Level, token) }
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        factory: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        let token = factory.token();
        self.token = Some(token);
        poll.reregister(self.context.as_fd(), Interest::READ, Mode::Level, token)
    }

    fn unregister(&mut self, poll: &mut Poll) -> smithay::reexports::calloop::Result<()> {
        self.token = None;
        poll.unregister(self.context.as_fd())
    }
}

/// Turns a ring's or strip's movement into whole steps of an action (one
/// scroll notch).
#[derive(Debug, Default)]
pub(crate) struct DialSteps {
    /// The last position per control, while a finger is on it.
    last: std::collections::HashMap<(bool, u32), f64>,
    /// Movement not yet a whole step.
    carry: std::collections::HashMap<(bool, u32), f64>,
}

/// Steps in one full turn of a ring: one every 15°.
pub(crate) const RING_STEPS_PER_TURN: u32 = 24;
/// How far along a strip one step is.
pub(crate) const STRIP_STEP: f64 = 0.1;

impl DialSteps {
    /// The whole steps the control moved, positive clockwise or down/right;
    /// a lifted finger ends the movement.
    pub(crate) fn steps(&mut self, event: &PadEvent) -> i32 {
        let (key, position, unit) = match *event {
            PadEvent::Ring { ring, degrees } => (
                (true, ring),
                degrees,
                360.0 / f64::from(RING_STEPS_PER_TURN),
            ),
            PadEvent::Strip { strip, position } => ((false, strip), position, STRIP_STEP),
            PadEvent::Button { .. } => return 0,
        };
        let Some(position) = position else {
            self.last.remove(&key);
            self.carry.remove(&key);
            return 0;
        };
        let Some(previous) = self.last.insert(key, position) else {
            return 0;
        };
        let mut delta = position - previous;
        if key.0 {
            // The shorter way round: 350° to 10° is +20°, not -340°.
            if delta > 180.0 {
                delta -= 360.0;
            } else if delta < -180.0 {
                delta += 360.0;
            }
        }
        let carry = self.carry.entry(key).or_insert(0.0);
        *carry += delta / unit;
        let whole = carry.trunc();
        *carry -= whole;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "steps from one event's movement are a handful"
        )]
        let steps = whole as i32;
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(degrees: Option<f64>) -> PadEvent {
        PadEvent::Ring { ring: 0, degrees }
    }

    #[test]
    fn a_ring_steps_every_fifteen_degrees_the_short_way_round() {
        let mut dial = DialSteps::default();
        assert_eq!(dial.steps(&ring(Some(350.0))), 0, "touching is no movement");
        assert_eq!(dial.steps(&ring(Some(5.0))), 1, "350° to 5° is +15°");
        assert_eq!(dial.steps(&ring(Some(12.5))), 0);
        assert_eq!(dial.steps(&ring(Some(20.0))), 1, "the carried 7.5° counts");
        assert_eq!(dial.steps(&ring(Some(350.0))), -2);
        assert_eq!(dial.steps(&ring(None)), 0);
        assert_eq!(
            dial.steps(&ring(Some(90.0))),
            0,
            "a new touch starts afresh"
        );
    }

    #[test]
    fn a_strip_steps_every_tenth_of_its_length() {
        let mut dial = DialSteps::default();
        let strip = |position| PadEvent::Strip { strip: 1, position };
        assert_eq!(dial.steps(&strip(Some(0.2))), 0);
        assert_eq!(dial.steps(&strip(Some(0.55))), 3);
        assert_eq!(dial.steps(&strip(Some(0.3))), -2);
    }
}
