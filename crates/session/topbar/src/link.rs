//! What the bar asks of the desktop that shows it, and what it hands the
//! bar.
//!
//! The bar draws, the desktop knows. The desktop hands the bar plain data:
//! the desktop's workspaces and the people in them, its status, the time,
//! the running applications' tray items and what the bar's menus offer
//! ([`BarFeed`]), and where each display's bar stands ([`Placement`]). The
//! bar puts what the person (or anyone acting through its published
//! actions) did as [`Ask`]s, and says what it shows for the desktop's hit
//! testing, focus and assistive technology ([`Shown`]).
//!
//! Both sides hold one [`Link`] in one process. Input that only the desktop
//! sees (a window maximizing under the bar, the person's chord moving focus
//! into it) reaches the bar as [`Command`]s, taken before anything else the
//! bar does.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use tilo_desktop::chrome::{
    AppTrayItem, AvatarAsk, ChromeIntent, ClockSnapshot, DesktopChromeSnapshot,
};
use tilo_desktop::profile_card::{ProfileChoice, ProfileModel};
use tilo_desktop::workspace_menu::{WorkspaceMenuChoice, WorkspaceMenuModel};
use tilo_desktop_model::DesktopTree;
use ui::time::Instant;
use ui::vello::kurbo::Rect;

/// Everything the bar shows, as the desktop knows it now.
#[derive(Clone, Default)]
pub struct BarFeed {
    /// The desktop's workspaces, strips and the people in them.
    pub tree: DesktopTree,
    /// Who sits at this desktop, as the roster names them.
    pub local_user: String,
    /// Status, overlays, presence, notices: what the bar reads each frame.
    /// Its revision is the feed's own; the desktop need not set it.
    pub snapshot: DesktopChromeSnapshot,
    /// The time to show; `None` reads the wall clock. A capture pins it so
    /// every frame holds still.
    pub clock: Option<ClockSnapshot>,
    /// The offer to continue a tab from another device, as its label.
    pub continuity: Option<String>,
    /// The ask waiting on the person's own avatar.
    pub avatar_ask: Option<AvatarAsk>,
    /// The running applications' tray items, in the desktop's order.
    pub app_tray: Vec<AppTrayItem>,
    /// Changes whenever the tray items change.
    pub app_tray_revision: u64,
    /// Unread notifications per verified application id.
    pub app_badges: HashMap<String, u32>,
    /// What the workspace menu offers when it opens.
    pub workspace_menu: WorkspaceMenuModel,
    /// Who the profile card is about and what it offers.
    pub profile: ProfileModel,
}

/// One display's bar, and where the bars go: the desktop places them, the
/// bar draws them (see [`tilo_desktop::topbar`]).
pub use tilo_desktop::topbar::{BarPlace, Placement};

/// A media transport command for what is playing on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

/// What the bar asks of the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// What a control on the bar asked for.
    Chrome(ChromeIntent),
    /// What the person chose in the workspace menu.
    WorkspaceMenu(WorkspaceMenuChoice),
    /// What the person did in the profile card.
    Profile(ProfileChoice),
    /// Open the notification centre.
    Notifications,
    /// Send a transport command to what is playing.
    Media(MediaCommand),
    /// The bar opened a menu or card: whatever else the desktop has open
    /// over it closes.
    Opened,
}

/// Input the desktop routes to the bar outside its own window.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Move keyboard focus into the full bar.
    Focus,
    /// Give keyboard focus back to the person's window.
    ReleaseKeyboard,
    /// A window maximizing over the full bar's display (`shown` false) or
    /// restoring (`shown` true), moved by `source`: the bar goes with it.
    MoveWithWindow {
        shown: bool,
        motion: ui::tokens::EffectiveMotion,
        source: ui::tokens::MotionSource,
    },
    /// Close the menu or card the bar opened.
    ClosePopovers,
}

/// What the bar shows, for the desktop.
#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact about the last paint the desktop reads on its own"
)]
pub struct Shown {
    /// Each display's bar and where it is drawn; `true` for the full one.
    pub bars: Vec<(u32, Rect, bool)>,
    /// The full bar's workspace name: its menu opens under it.
    pub workspace_name: Rect,
    /// The full bar's own avatar: the profile card opens under it.
    pub own_avatar: Rect,
    /// The full bar's faces of the people here.
    pub people: Rect,
    /// The full bar's status cluster.
    pub tray: Rect,
    /// The floating clock, while it floats.
    pub clock: Rect,
    /// The open menu or card; zero when none is open.
    pub popover: Rect,
    /// The workspace menu's rows as shown, while it is open.
    pub workspace_menu: Option<Vec<Option<String>>>,
    /// The profile card is open.
    pub profile: bool,
    /// Something of the bar is open over the desktop and takes the next
    /// press, wherever it lands (a menu or card).
    pub modal: bool,
    /// Keyboard focus is in the bar.
    pub keyboard: bool,
    /// The bar must stay shown though the person hides it: the keyboard is
    /// in it, or it opened something.
    pub holds_shown: bool,
    /// A press begun on the bar owns the next release.
    pub captures_pointer: bool,
    /// The bar is moving or fading.
    pub animating: bool,
    /// When the bar next needs to run with nothing waking it.
    pub deadline: Option<Instant>,
}

#[derive(Default)]
struct Shared {
    feed: BarFeed,
    generation: u64,
    placement: Placement,
    asks: Vec<Ask>,
    commands: VecDeque<Command>,
    shown: Shown,
    wake: Option<ui::WakeHandle>,
}

/// The bar's side of the conversation and the desktop's, shared between
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

    /// Show `feed`: what the desktop knows now.
    pub fn publish(&self, feed: BarFeed) {
        {
            let mut shared = self.shared.borrow_mut();
            shared.feed = feed;
            shared.generation = shared.generation.wrapping_add(1);
        }
        self.wake();
    }

    /// Put the bars at `placement`.
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

    /// Route `command` to the bar, taken the next time it runs.
    pub fn command(&self, command: Command) {
        self.shared.borrow_mut().commands.push_back(command);
        self.wake();
    }

    /// Everything the bar asked since last taken, in order.
    pub fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.shared.borrow_mut().asks)
    }

    /// What the bar shows, as of its last frame or input.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shared.borrow().shown.clone()
    }

    /// Wake the bar with `wake` when the desktop hands it something.
    pub fn wake_with(&self, wake: ui::WakeHandle) {
        self.shared.borrow_mut().wake = Some(wake);
    }

    pub(crate) fn wake(&self) {
        let wake = self.shared.borrow().wake.clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    // The bar's side.

    /// The feed shown now, if it is newer than generation `seen`.
    pub(crate) fn feed_after(&self, seen: u64) -> Option<(u64, BarFeed)> {
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

/// The feed a desktop lends the bar it mounts
/// ([`ui::application::HostContext::lend`]): the desktop keeps a clone of
/// the link, publishes what it knows on it and carries out what the bar
/// asks.
#[derive(Clone)]
pub struct BarService {
    pub link: Link,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_is_new_once_and_asks_are_taken_in_order() {
        let link = Link::new();
        assert!(link.feed_after(0).is_none(), "nothing published yet");
        link.publish(BarFeed::default());
        let (generation, _) = link.feed_after(0).expect("a new feed");
        assert!(link.feed_after(generation).is_none());
        link.ask(Ask::Notifications);
        link.ask(Ask::Media(MediaCommand::Next));
        assert_eq!(
            link.take_asks(),
            [Ask::Notifications, Ask::Media(MediaCommand::Next)]
        );
        assert!(link.take_asks().is_empty());
        link.command(Command::Focus);
        assert!(link.has_commands());
        assert_eq!(link.next_command(), Some(Command::Focus));
        assert!(!link.has_commands());
    }
}
