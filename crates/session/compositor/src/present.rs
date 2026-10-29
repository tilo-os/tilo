//! Telling clients about frames: frame callbacks when a frame is drawn,
//! presentation feedback when it reaches the screen.
//!
//! A backend calls [`State::frame_drawn`] after composing an output's frame
//! (clients may draw their next one) and keeps the [`Presentation`] it
//! returns until the display shows the frame (the vertical blank, or the
//! host's present event), then calls [`Presentation::shown`] with the time
//! and counter the display reported. A frame that never reaches the screen
//! is dropped, which tells its clients it was discarded.

use std::time::Duration;

use smithay::backend::renderer::element::RenderElementStates;
use smithay::desktop::utils::{
    OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
use smithay::utils::{Clock, Monotonic, Time};
use smithay::wayland::presentation::Refresh;

use crate::output::refresh_interval;
use crate::state::State;

/// How a display reports that a frame was shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// At a vertical blank, timed and counted by the display hardware.
    Vblank {
        /// The vblank's time on the monotonic clock.
        time: Duration,
        /// The display's frame counter.
        sequence: u32,
    },
    /// When the host (another compositor or X server) said so: synced to
    /// its refresh, timed by us.
    Host,
    /// Not on a display at all (headless): as soon as it is drawn.
    Immediately,
}

impl Shown {
    /// The time, counter and flags `wp_presentation` reports.
    fn report(self, now: Time<Monotonic>) -> (Time<Monotonic>, u64, Kind) {
        match self {
            Self::Vblank { time, sequence } => (
                Time::from(time),
                u64::from(sequence),
                Kind::Vsync | Kind::HwClock | Kind::HwCompletion,
            ),
            Self::Host => (now, 0, Kind::Vsync),
            Self::Immediately => (now, 0, Kind::empty()),
        }
    }
}

/// The presentation feedback of one drawn frame, waiting for the display.
#[derive(Debug)]
pub struct Presentation {
    feedback: OutputPresentationFeedback,
    refresh: Refresh,
}

impl Presentation {
    /// The frame reached the screen as `shown` says: answer every client
    /// that asked.
    pub(crate) fn shown(mut self, shown: Shown) {
        let (time, sequence, flags) = shown.report(Clock::<Monotonic>::new().now());
        self.feedback
            .presented(time, self.refresh, sequence, flags);
    }
}

impl State {
    /// A frame of `output` was drawn: send its surfaces' frame callbacks and
    /// collect their presentation feedback. `states` says how each element
    /// was shown (scanned out directly or composited), when the backend
    /// knows.
    pub(crate) fn frame_drawn(
        &self,
        output: &Output,
        states: Option<&RenderElementStates>,
    ) -> Presentation {
        let time = self.now();
        let mut feedback = OutputPresentationFeedback::new(output);
        for window in self.space.elements_for_output(output) {
            window.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
            window.take_presentation_feedback(
                &mut feedback,
                |_, _| Some(output.clone()),
                |surface, _| {
                    states.map_or_else(Kind::empty, |states| {
                        surface_presentation_feedback_flags_from_states(surface, states)
                    })
                },
            );
        }
        let refresh = output
            .current_mode()
            .filter(|mode| mode.refresh > 0)
            .map_or(Refresh::Unknown, |mode| {
                Refresh::Fixed(refresh_interval(mode.refresh))
            });
        Presentation { feedback, refresh }
    }

    /// A frame of `output` was drawn and is on screen at once (no display
    /// to wait for).
    pub(crate) fn frame_shown(&self, output: &Output) {
        self.frame_drawn(output, None).shown(Shown::Immediately);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vblanks_report_the_hardware_time_and_counter() {
        let now = Time::from(Duration::from_secs(9));
        let (time, sequence, flags) = Shown::Vblank {
            time: Duration::from_millis(1500),
            sequence: 42,
        }
        .report(now);
        assert_eq!(Duration::from(time), Duration::from_millis(1500));
        assert_eq!(sequence, 42);
        assert_eq!(flags, Kind::Vsync | Kind::HwClock | Kind::HwCompletion);
    }

    #[test]
    fn host_and_headless_frames_are_timed_now() {
        let now = Time::from(Duration::from_secs(9));
        let (time, sequence, flags) = Shown::Host.report(now);
        assert_eq!((Duration::from(time), sequence, flags), (Duration::from_secs(9), 0, Kind::Vsync));
        let (time, _, flags) = Shown::Immediately.report(now);
        assert_eq!((Duration::from(time), flags), (Duration::from_secs(9), Kind::empty()));
    }
}
