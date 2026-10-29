//! What the banners ask of the desktop that shows them, and what it hands
//! them.
//!
//! The banners draw, the desktop knows. The desktop hands the banners a copy
//! of its live notifications ([`BannerFeed`]) and where they go on its
//! displays ([`Placement`]); the banners put what the person (or anyone
//! acting through their published actions) did to the desktop as [`Ask`]s,
//! and say what they show for the desktop's own hit testing, focus and
//! assistive technology ([`Shown`]).
//!
//! Both sides hold one [`Link`] in one process. The desktop sends the input
//! that reaches it rather than the banners (a key while the banners hold the
//! keyboard, the pointer moving over a window under them) as [`Command`]s,
//! which the banners take before anything else they do.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_desktop::notifications::{BannerAsk, BannerFeed};
use ui::a11y::AccessNode;
use ui::time::Instant;
use ui::vello::kurbo::{Point, Rect};
use ui::winit::keyboard::Key;

/// A display other than the one in use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OtherDisplay {
    /// The whole display: a ringing call gives it a quiet edge.
    pub display: Rect,
    /// The display under its top bar: banners about the whole machine
    /// repeat there.
    pub viewport: Rect,
}

/// Where the banners go, in the banners' own coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placement {
    /// The display in use, under its top bar: the stack pins itself into its
    /// top-right corner. Zero: the banners' own area.
    pub viewport: Rect,
    /// Everything above this stays clear (the follow bar while it is up).
    pub clear_top: f64,
    /// The windows' bars on screen: a banner never covers one.
    pub window_bars: Vec<Rect>,
    /// The displays other than the one in use.
    pub others: Vec<OtherDisplay>,
}

/// What the banners ask of the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// What the person did on the stack: open, close or choose on a banner,
    /// open the centre, or what is on screen and held.
    Banner(BannerAsk),
    /// Send `text` to banner `id`'s sender as its inline reply.
    Reply { id: u64, text: String },
    /// Read banner `id` in full: it goes to the notification centre, which
    /// opens on it.
    Read(u64),
    /// Answer the call banner `id` rings for.
    AnswerCall(u64),
    /// Decline the call banner `id` rings for.
    DeclineCall(u64),
}

/// Input the desktop routes to the banners outside their own window.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// The pointer is at this point (`None`: it left the displays), also
    /// while a window under it takes the motion: reaching for the stack
    /// holds the banners.
    Pointer(Option<Point>),
    /// Move keyboard focus into the newest banner (the Notifications chord).
    FocusNewest,
    /// A key while the banners hold the keyboard.
    Key { key: Key, shift: bool },
    /// Give keyboard focus back to the person's window.
    ReleaseKeyboard,
    /// Something in front of the banners took a press begun on them.
    CancelPress,
    /// Press the control assistive technology names by `target`.
    Activate(String),
}

/// What the banners show, for the desktop.
#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact about the last paint the desktop reads on its own"
)]
pub struct Shown {
    /// What the stack covers; zero when it shows nothing.
    pub stack: Rect,
    /// Each banner on the stack and where it is drawn, newest first.
    pub banners: Vec<(u64, Rect)>,
    /// The machine-wide banners on the other displays.
    pub machine: Vec<Rect>,
    /// The displays with a ringing call's edge.
    pub call_edges: Vec<Rect>,
    /// Keyboard focus is in a banner.
    pub keyboard: bool,
    /// There is a banner the Notifications chord can move focus into.
    pub focusable: bool,
    /// A press begun on a banner owns the next release.
    pub captures_pointer: bool,
    /// A banner on screen is arriving, leaving or moving.
    pub animating: bool,
    /// When a banner on screen next starts leaving or expires.
    pub deadline: Option<Instant>,
    /// The stack as assistive technology reads it: a live region announcing
    /// the newest banner, with its controls.
    pub accessibility: Option<AccessNode>,
}

/// What takes the desktop's commands and feed the moment they are routed:
/// the banners' surface, while it is not busy with something else.
pub(crate) trait TakesCommands {
    fn take(&mut self, command: Command);
    /// Show the feed published now.
    fn take_published(&mut self);
}

#[derive(Default)]
struct Shared {
    feed: BannerFeed,
    generation: u64,
    placement: Placement,
    asks: Vec<Ask>,
    commands: VecDeque<Command>,
    shown: Shown,
    wake: Option<ui::WakeHandle>,
    taker: Option<std::rc::Weak<RefCell<dyn TakesCommands>>>,
}

/// The banners' side of the conversation and the desktop's, shared between
/// the two in one process.
#[derive(Clone, Default)]
pub struct Link {
    shared: Rc<RefCell<Shared>>,
}

impl Link {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show `feed`: the desktop's live banners now. Idle banners take it at
    /// once, so what they say they show (their live region announcing the
    /// newest) is this feed before they next run.
    pub fn publish(&self, feed: BannerFeed) {
        {
            let mut shared = self.shared.borrow_mut();
            shared.feed = feed;
            shared.generation = shared.generation.wrapping_add(1);
        }
        if let Some(taker) = self.taker()
            && let Ok(mut taker) = taker.try_borrow_mut()
        {
            taker.take_published();
        }
        self.wake();
    }

    fn taker(&self) -> Option<Rc<RefCell<dyn TakesCommands>>> {
        self.shared
            .borrow()
            .taker
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
    }

    /// Put the banners at `placement`.
    pub fn place(&self, placement: Placement) {
        {
            let mut shared = self.shared.borrow_mut();
            if shared.placement == placement {
                return;
            }
            shared.placement = placement;
        }
        self.wake();
    }

    /// Route `command` to the banners: taken at once while they are idle,
    /// else as soon as they run.
    pub fn command(&self, command: Command) {
        let taker = self.taker();
        let queued = self.shared.borrow().commands.is_empty();
        match taker {
            Some(taker) if queued => match taker.try_borrow_mut() {
                Ok(mut taker) => taker.take(command),
                Err(_) => self.shared.borrow_mut().commands.push_back(command),
            },
            _ => self.shared.borrow_mut().commands.push_back(command),
        }
        self.wake();
    }

    /// Everything the banners asked since last taken, in order.
    pub fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.shared.borrow_mut().asks)
    }

    /// What the banners show, as of their last frame or input.
    pub fn shown(&self) -> Shown {
        self.shared.borrow().shown.clone()
    }

    /// Wake the banners with `wake` when the desktop hands them something.
    pub fn wake_with(&self, wake: ui::WakeHandle) {
        self.shared.borrow_mut().wake = Some(wake);
    }

    fn wake(&self) {
        let wake = self.shared.borrow().wake.clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    // The banners' side.

    pub(crate) fn take_commands_with(&self, taker: std::rc::Weak<RefCell<dyn TakesCommands>>) {
        self.shared.borrow_mut().taker = Some(taker);
    }

    /// The feed shown now, if it is newer than generation `seen`.
    pub(crate) fn feed_after(&self, seen: u64) -> Option<(u64, BannerFeed)> {
        let shared = self.shared.borrow();
        (shared.generation != seen).then(|| (shared.generation, shared.feed.clone()))
    }

    pub(crate) fn generation(&self) -> u64 {
        self.shared.borrow().generation
    }

    pub(crate) fn placement(&self) -> Placement {
        self.shared.borrow().placement.clone()
    }

    pub(crate) fn has_commands(&self) -> bool {
        !self.shared.borrow().commands.is_empty()
    }

    pub(crate) fn next_command(&self) -> Option<Command> {
        self.shared.borrow_mut().commands.pop_front()
    }

    pub(crate) fn ask(&self, ask: Ask) {
        self.shared.borrow_mut().asks.push(ask);
    }

    pub(crate) fn show(&self, shown: Shown) {
        self.shared.borrow_mut().shown = shown;
    }
}

/// The feed a desktop lends the banners it mounts
/// ([`ui::application::HostContext::lend`]): the desktop keeps a clone of
/// the link, publishes its banners on it and carries out what they ask.
#[derive(Clone)]
pub struct BannerService {
    pub link: Link,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_is_new_once_and_asks_are_taken_in_order() {
        let link = Link::new();
        assert!(link.feed_after(0).is_none(), "nothing published yet");
        link.publish(BannerFeed::default());
        let (generation, _) = link.feed_after(0).expect("a new feed");
        assert!(link.feed_after(generation).is_none());
        link.ask(Ask::Read(1));
        link.ask(Ask::AnswerCall(2));
        assert_eq!(link.take_asks(), [Ask::Read(1), Ask::AnswerCall(2)]);
        assert!(link.take_asks().is_empty());
        link.command(Command::FocusNewest);
        assert!(link.has_commands());
        assert_eq!(link.next_command(), Some(Command::FocusNewest));
        assert!(!link.has_commands());
    }
}
