//! Sharing a window live: one app shows another app's view of a resource,
//! and the desktop says which windows there are to share.
//!
//! A message that shares a window (Chat's) shows the window's resource as
//! it is now. It is not a picture of the sharer's screen: every reader's
//! desktop runs the resource's own app on its own copy of the record, and
//! draws that app's view, scaled down, where the message asked for it. The
//! record syncs like any other, so the miniature follows the sharer's work;
//! what a reader may see of it is what their grants let their copy hold.
//!
//! - **Embeds.** An app declares, on the window its host shows it in, where
//!   it wants which resource drawn ([`declare`], one [`Embed`] each) and
//!   reads back how each is coming along ([`state`]). The host draws them
//!   over the app's own content, clipped to [`Embed::clip`], and never
//!   hands them input: opening the resource is the app's own action. A
//!   window no host draws embeds for ([`drawn`] is false) shows what it
//!   draws itself in their place.
//! - **Windows to share.** The desktop lists the windows its person has
//!   open on a record ([`publish_windows`]), so a "Share a window" picker
//!   names real windows ([`windows`]); a window's own menu asks the sharing
//!   app to share it ([`ask_to_share`], [`take_ask`]).
//! - **What is shared.** The sharing app says which windows it shares
//!   ([`publish_shared`]), so the desktop marks them and offers "Stop
//!   sharing" on them, which reaches the app the same way ([`Ask::Stop`]).
//!
//! An app its host runs in the host's own process (every app of the
//! browser desktop) reaches all of it on the host's thread. An app in a
//! process of its own has no host for it yet: it sees no windows and draws
//! its own stand-in for each embed.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use vello::kurbo::Rect;

/// The most embeds one window may ask for at once: a host runs an app for
/// each, so a long scroll of shares shows the ones in view.
pub const MAX_EMBEDS: usize = 4;
/// The most asks a host keeps for an app that has not read them yet.
const MAX_ASKS: usize = 8;

/// The app that shares windows into conversations and takes the desktop's
/// asks ([`take_ask`]): the desktop brings it forward when a window's menu
/// asks to share.
pub const SHARING_APP: &str = "so.tilo.chat";

/// A resource as one app's view shows it: the app, the record it keeps the
/// resource in and the resource itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ResourceView {
    /// The app showing it, by package id.
    pub app: String,
    /// The record it lives in.
    pub record: String,
    /// The resource, by its published id.
    pub resource: String,
}

/// One place an app wants a resource drawn by its own app.
#[derive(Clone, Debug, PartialEq)]
pub struct Embed {
    /// Names the embed across frames (a message's id).
    pub key: String,
    pub view: ResourceView,
    /// Where it goes, in the window's layout coordinates. The host keeps the
    /// view's own proportions inside it.
    pub rect: Rect,
    /// What of it may show: the part of the app's content it is scrolled
    /// into.
    pub clip: Rect,
}

/// How an embed is coming along, as its host reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbedState {
    /// The app's code or the record is on its way.
    Loading,
    /// The view is drawn.
    Showing,
    /// It cannot be shown: the record is gone, or it may not be read here.
    Unavailable(String),
}

/// A window its person has open on a record, as the desktop lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenWindow {
    /// The desktop's id of its tab.
    pub tab: u64,
    /// The app, by package id.
    pub app: String,
    /// The app's name, as the desktop names it.
    pub app_title: String,
    /// What its tab says.
    pub title: String,
    /// The record it shows, as its route names it: empty for the app's own
    /// record of the workspace.
    pub record: String,
    /// The kind of the record's resource.
    pub kind: String,
}

impl OpenWindow {
    /// The resource the window shows, as one app's view of it.
    #[must_use]
    pub fn view(&self) -> ResourceView {
        ResourceView {
            app: self.app.clone(),
            record: self.record.clone(),
            // The app's own record of the workspace has no id of its own;
            // its kind names the resource it is.
            resource: if self.record.is_empty() {
                self.kind.clone()
            } else {
                self.record.clone()
            },
        }
    }
}

/// A window the sharing app shares, as it tells the desktop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedWindow {
    /// The message that shares it.
    pub message: String,
    pub app: String,
    pub record: String,
    /// Where it is shared, as people read it ("#general").
    pub place: String,
}

/// What the desktop asks of the sharing app on its person's behalf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Share this window: the app asks where, and with which access.
    Share(OpenWindow),
    /// Stop sharing what this message shares.
    Stop { message: String },
}

#[derive(Default)]
struct Desktop {
    windows: Vec<OpenWindow>,
    shared: Vec<SharedWindow>,
    asks: VecDeque<Ask>,
    /// Whether a host draws embeds on this thread.
    draws: bool,
}

thread_local! {
    static DESKTOP: RefCell<Desktop> = RefCell::new(Desktop::default());
}

// --- the app's side

/// Ask for `embeds` in the window the app is shown in, at most
/// [`MAX_EMBEDS`] (the first ones). Cheap when nothing changed.
pub fn declare(mut embeds: Vec<Embed>) {
    embeds.truncate(MAX_EMBEDS);
    if let Some(window) = crate::window_toolbar::window() {
        window.declare_embeds(embeds);
    }
}

/// How the host says `key`'s embed is coming along; `None` before it said.
#[must_use]
pub fn state(key: &str) -> Option<EmbedState> {
    crate::window_toolbar::window().and_then(|window| window.embed_state(key))
}

/// Whether a host draws this window's embeds. Without one, the app draws
/// its own stand-in where each would be.
#[must_use]
pub fn drawn() -> bool {
    crate::window_toolbar::window().is_some() && DESKTOP.with_borrow(|desktop| desktop.draws)
}

/// The windows the person has open on a record, as the desktop last listed
/// them.
#[must_use]
pub fn windows() -> Vec<OpenWindow> {
    DESKTOP.with_borrow(|desktop| desktop.windows.clone())
}

/// Tell the desktop which windows this app shares now.
pub fn publish_shared(shared: Vec<SharedWindow>) {
    DESKTOP.with_borrow_mut(|desktop| desktop.shared = shared);
}

/// Whether the desktop asked the sharing app something it has not taken
/// yet.
#[must_use]
pub fn asked() -> bool {
    DESKTOP.with_borrow(|desktop| !desktop.asks.is_empty())
}

/// The oldest thing the desktop asked of the sharing app and it has not
/// taken yet.
#[must_use]
pub fn take_ask() -> Option<Ask> {
    DESKTOP.with_borrow_mut(|desktop| desktop.asks.pop_front())
}

// --- the host's side

/// The host draws embeds on this thread (`true`), or no longer does.
pub fn set_drawn(draws: bool) {
    DESKTOP.with_borrow_mut(|desktop| desktop.draws = draws);
}

/// List the windows the person has open on a record. Returns whether the
/// list changed.
pub fn publish_windows(windows: Vec<OpenWindow>) -> bool {
    DESKTOP.with_borrow_mut(|desktop| {
        let changed = desktop.windows != windows;
        desktop.windows = windows;
        changed
    })
}

/// The windows the sharing app says it shares.
#[must_use]
pub fn shared() -> Vec<SharedWindow> {
    DESKTOP.with_borrow(|desktop| desktop.shared.clone())
}

/// Ask the sharing app to share `window`.
pub fn ask_to_share(window: OpenWindow) {
    push(Ask::Share(window));
}

/// Ask the sharing app to stop sharing what `message` shares.
pub fn ask_to_stop(message: impl Into<String>) {
    push(Ask::Stop {
        message: message.into(),
    });
}

fn push(ask: Ask) {
    DESKTOP.with_borrow_mut(|desktop| {
        if desktop.asks.len() >= MAX_ASKS {
            desktop.asks.pop_front();
        }
        desktop.asks.push_back(ask);
    });
}

/// The embeds of one window, with how each is coming along: what a
/// [`crate::hosted_window::HostedWindow`] keeps.
#[derive(Clone, Debug, Default)]
pub(crate) struct Embeds {
    pub(crate) declared: Vec<Embed>,
    pub(crate) states: HashMap<String, EmbedState>,
}

impl Embeds {
    /// Take `embeds` as declared; whether they changed.
    pub(crate) fn declare(&mut self, embeds: Vec<Embed>) -> bool {
        if self.declared == embeds {
            return false;
        }
        self.states
            .retain(|key, _| embeds.iter().any(|embed| embed.key == *key));
        self.declared = embeds;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosted_window::HostedWindow;

    fn embed(key: &str) -> Embed {
        Embed {
            key: key.into(),
            view: ResourceView {
                app: "so.tilo.notes".into(),
                record: "plan".into(),
                resource: "plan".into(),
            },
            rect: Rect::new(10.0, 20.0, 330.0, 220.0),
            clip: Rect::new(0.0, 0.0, 400.0, 400.0),
        }
    }

    #[test]
    fn an_app_declares_embeds_on_its_window_and_reads_back_what_the_host_said() {
        let window = HostedWindow::new();
        {
            let _shown = crate::window_toolbar::enter(Some(&window));
            set_drawn(true);
            assert!(drawn());
            declare((0..6).map(|n| embed(&format!("m{n}"))).collect());
        }
        let declared = window.embeds();
        assert_eq!(
            declared.len(),
            MAX_EMBEDS,
            "a window asks for a few at once"
        );
        window.set_embed_state("m1", EmbedState::Showing);
        {
            let _shown = crate::window_toolbar::enter(Some(&window));
            assert_eq!(state("m1"), Some(EmbedState::Showing));
            assert_eq!(state("m0"), None);
            declare(vec![embed("m0")]);
            assert_eq!(
                state("m1"),
                None,
                "an embed no longer asked for is forgotten"
            );
        }
        set_drawn(false);
        assert!(!drawn(), "outside a hosted window nothing draws embeds");
    }

    #[test]
    fn the_desktop_lists_windows_and_asks_the_sharing_app_in_order() {
        let notes = OpenWindow {
            tab: 7,
            app: "so.tilo.notes".into(),
            app_title: "Notes".into(),
            title: "Plan".into(),
            record: "plan".into(),
            kind: "notes.workspace".into(),
        };
        assert!(publish_windows(vec![notes.clone()]));
        assert!(!publish_windows(vec![notes.clone()]), "unchanged");
        assert_eq!(windows(), vec![notes.clone()]);
        ask_to_share(notes.clone());
        ask_to_stop("m1");
        assert_eq!(take_ask(), Some(Ask::Share(notes)));
        assert_eq!(
            take_ask(),
            Some(Ask::Stop {
                message: "m1".into()
            })
        );
        assert_eq!(take_ask(), None);
        for n in 0..20 {
            ask_to_stop(format!("m{n}"));
        }
        assert_eq!(
            take_ask(),
            Some(Ask::Stop {
                message: "m12".into()
            }),
            "an app that never reads keeps only the latest asks"
        );
    }
}
