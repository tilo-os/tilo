//! What the session hands the layer and what the layer asks of it.
//!
//! The session knows every activity on this device and the screen it stands
//! on; the layer draws, takes the presses and says what the person did. The
//! session publishes what to show as [`Feed`] — the cards, the corner they
//! dock in, and the screen's keep-clear rects — and takes what came of it as
//! [`Ask`]s, which are the frame's own
//! ([`tilo_desktop_surface::floating_layer::Ask`]): a control pressed on a
//! card, a card closed, folded, dropped, or keyboard focus entering or
//! leaving.
//!
//! What the layer shows for the session's own hit testing comes back
//! as [`Shown`]: the cards it drew on the last frame and where.

use std::cell::RefCell;
use std::rc::Rc;

use ui::floating_layer::{Card, Corner, Placement};

pub use tilo_desktop_surface::floating_layer::{Ask, Shown};

/// What the session shows the layer: its cards on the screen `placement`
/// describes, docked in `corner`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// The screen the layer stands on, in the layer's own coordinates.
    pub placement: Placement,
    /// The corner docked cards gather in.
    pub corner: Corner,
    /// Every activity that shows a card, most recently touched last.
    pub cards: Vec<Card>,
    /// The person pulled the phone-width card out of the bar.
    pub expanded: bool,
}

#[derive(Default)]
struct Shared {
    feed: Feed,
    generation: u64,
    asks: Vec<Ask>,
    shown: Shown,
    wake: Option<ui::WakeHandle>,
}

/// The layer's side of the conversation and the session's, shared between
/// the two in one process.
#[derive(Clone, Default)]
pub struct Link {
    shared: Rc<RefCell<Shared>>,
}

impl Link {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Show `feed`: the session's activities now.
    pub fn publish(&self, feed: Feed) {
        {
            let mut shared = self.shared.borrow_mut();
            shared.feed = feed;
            shared.generation = shared.generation.wrapping_add(1);
        }
        self.wake();
    }

    /// Everything the layer asked since last taken, in order.
    #[must_use]
    pub fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.shared.borrow_mut().asks)
    }

    /// What the layer shows, as of its last frame or input.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shared.borrow().shown.clone()
    }

    /// Wake the layer with `wake` when the session hands it something.
    pub fn wake_with(&self, wake: ui::WakeHandle) {
        self.shared.borrow_mut().wake = Some(wake);
    }

    fn wake(&self) {
        let wake = self.shared.borrow().wake.clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    // The layer's side.

    /// The feed shown now, if it is newer than generation `seen`.
    pub(crate) fn feed_after(&self, seen: u64) -> Option<(u64, Feed)> {
        let shared = self.shared.borrow();
        (shared.generation != seen).then(|| (shared.generation, shared.feed.clone()))
    }

    pub(crate) fn generation(&self) -> u64 {
        self.shared.borrow().generation
    }

    pub(crate) fn ask(&self, ask: Ask) {
        self.shared.borrow_mut().asks.push(ask);
    }

    pub(crate) fn show(&self, shown: Shown) {
        self.shared.borrow_mut().shown = shown;
    }
}

/// The feed a session lends the layer it mounts
/// ([`ui::application::HostContext::lend`]): the session keeps a clone of the
/// link, publishes its activities on it and carries out what they ask.
#[derive(Clone)]
pub struct ActivityService {
    pub link: Link,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_is_new_once_and_asks_are_taken_in_order() {
        let link = Link::new();
        assert!(link.feed_after(0).is_none(), "nothing published yet");
        link.publish(Feed::default());
        let (generation, _) = link.feed_after(0).expect("a new feed");
        assert!(link.feed_after(generation).is_none());
        link.ask(Ask::Seek {
            app: "so.tilo.music".into(),
            activity: "track".into(),
            fraction: 0.5,
        });
        link.ask(Ask::Collapse {
            app: "so.tilo.clocks".into(),
            activity: "timer".into(),
        });
        assert_eq!(
            link.take_asks(),
            [
                Ask::Seek {
                    app: "so.tilo.music".into(),
                    activity: "track".into(),
                    fraction: 0.5,
                },
                Ask::Collapse {
                    app: "so.tilo.clocks".into(),
                    activity: "timer".into()
                }
            ]
        );
        assert!(link.take_asks().is_empty());
    }
}
