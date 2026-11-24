//! The desktop's reading of the touchpad: the three- and four-finger gestures the person's
//! settings give the desktop, read into what they ask of it. Both window policies read them (the
//! strips' and the tilolang shell's wells), so a gesture means the same on either.

use tilo_compositor::GestureEvent;
use tilo_compositor::gestures::{
    GestureAction, GestureConfig, GestureTracker, SWIPE_THRESHOLD, claims,
};

/// What a touchpad gesture the desktop took asks of it, once the gesture
/// has said what it means: the same steps the desktop's chords take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShellGesture {
    /// Three fingers carrying the shown strip: `travel` is their total
    /// signed travel since they landed, positive toward later windows.
    StripDrag { travel: f64, began: bool },
    /// Those fingers lifted (or the hand was taken back): the strip coasts
    /// on the velocity they let go at, or stays where it was.
    StripDragEnd { cancelled: bool },
    /// Three fingers up or down lifted past the threshold: the strip
    /// `steps` on, sliding as a finger's change does.
    StepStrip(i32),
    /// Four fingers up or down: the workspace `delta` on.
    SwitchWorkspace(i32),
    /// A pinch in opens the overview (`true`), out closes it.
    Overview(bool),
}

/// What reading one touchpad event came to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Reading {
    /// Not the desktop's: the client under the pointer keeps it.
    Clients,
    /// The desktop took it; the fingers say nothing yet.
    Taken,
    /// The desktop took it and it asks this.
    Asked(ShellGesture),
}

/// The desktop's reading of the touchpad's gestures: the three- and four-finger
/// streams the person's settings give the desktop, followed until the fingers say
/// what they mean (`tilo_compositor::gestures` turns the stream into those
/// actions). Both window policies read them — the strips' and the shell's
/// wells — so a gesture means the same on either desktop.
#[derive(Clone, Debug, Default)]
pub(crate) struct GestureReader {
    tracker: GestureTracker,
    /// How far the fingers have carried the strip, or the strip deck, in the
    /// gesture in flight.
    travel: f64,
}

impl GestureReader {
    /// Whether the desktop takes `gesture`, handing what it asks to `asked`.
    pub(crate) fn take(
        &mut self,
        gesture: &GestureEvent,
        config: Option<GestureConfig>,
        asked: impl FnOnce(ShellGesture),
    ) -> bool {
        match self.read(gesture, config) {
            Reading::Clients => false,
            Reading::Taken => true,
            Reading::Asked(desktop) => {
                asked(desktop);
                true
            }
        }
    }

    /// `gesture` as the desktop's, when `config` reads gestures (the person asked
    /// for them) and it has three or more fingers.
    pub(crate) fn read(
        &mut self,
        gesture: &GestureEvent,
        config: Option<GestureConfig>,
    ) -> Reading {
        let action = match *gesture {
            GestureEvent::SwipeBegin { fingers } | GestureEvent::PinchBegin { fingers } => {
                let Some(config) = config.filter(|_| claims(fingers)) else {
                    return Reading::Clients;
                };
                self.tracker.set_config(config);
                self.tracker.swipe_begin(fingers);
                self.travel = 0.0;
                None
            }
            GestureEvent::SwipeUpdate { dx, dy } => self.tracker.swipe_update(dx, dy),
            GestureEvent::SwipeEnd { cancelled } | GestureEvent::PinchEnd { cancelled } => {
                self.tracker.swipe_end_with_cancel(cancelled)
            }
            GestureEvent::PinchUpdate { scale, .. } => self.tracker.pinch_update(scale),
            // A hold asks nothing of the desktop: the client keeps it.
            GestureEvent::HoldBegin { .. } | GestureEvent::HoldEnd { .. } => {
                return Reading::Clients;
            }
        };
        let step = |travel: f64| {
            if travel >= SWIPE_THRESHOLD {
                Some(1)
            } else if travel <= -SWIPE_THRESHOLD {
                Some(-1)
            } else {
                None
            }
        };
        let asked = match action {
            // Up and down, the travel decides at the lift; sideways, every
            // update carries the shown strip with the fingers.
            Some(GestureAction::StripDeckUpdate { travel, .. }) => {
                self.travel = travel;
                None
            }
            Some(GestureAction::StripPanUpdate { travel, began }) => {
                self.travel = travel;
                Some(ShellGesture::StripDrag { travel, began })
            }
            Some(GestureAction::StripPanEnd { cancelled }) => {
                Some(ShellGesture::StripDragEnd { cancelled })
            }
            Some(GestureAction::StripDeckEnd { cancelled: false }) => {
                step(self.travel).map(ShellGesture::StepStrip)
            }
            Some(GestureAction::SwitchWorkspace(delta)) => {
                Some(ShellGesture::SwitchWorkspace(delta))
            }
            Some(GestureAction::ShowOverview) => Some(ShellGesture::Overview(true)),
            Some(GestureAction::HideOverview) => Some(ShellGesture::Overview(false)),
            // A cancelled deck drag is the hand coming back: nothing moves.
            Some(GestureAction::StripDeckEnd { cancelled: true }) | None => None,
        };
        match asked {
            Some(asked) => Reading::Asked(asked),
            None => Reading::Taken,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three fingers moving sideways carry the shown strip: the desktop reads
    /// every report as the hand's total travel, from the first one on, and
    /// the lift as its release, rather than stepping the strip once the
    /// travel passes a threshold.
    #[test]
    fn three_fingers_sideways_carry_the_strip() {
        let mut reader = GestureReader::default();
        // Natural scrolling, the touchpad default: the strip goes with the
        // fingers, so their leftward travel goes toward later windows.
        let config = Some(GestureConfig {
            natural_scroll: true,
            ..GestureConfig::default()
        });
        assert_eq!(
            reader.read(&GestureEvent::SwipeBegin { fingers: 3 }, config),
            Reading::Taken,
            "the desktop takes three fingers"
        );
        assert_eq!(
            reader.read(&GestureEvent::SwipeUpdate { dx: -20.0, dy: 0.0 }, config),
            Reading::Asked(ShellGesture::StripDrag {
                travel: 20.0,
                began: true,
            }),
            "the first report already carries the strip"
        );
        assert_eq!(
            reader.read(&GestureEvent::SwipeUpdate { dx: -10.0, dy: 0.0 }, config),
            Reading::Asked(ShellGesture::StripDrag {
                travel: 30.0,
                began: false,
            }),
            "the gesture reports its total"
        );
        assert_eq!(
            reader.read(&GestureEvent::SwipeEnd { cancelled: false }, config),
            Reading::Asked(ShellGesture::StripDragEnd { cancelled: false })
        );
    }
}
