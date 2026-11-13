//! Sharing a window live: one app shows another app's view of a resource, and the desktop
//! says which windows there are to share. A message that shares a window shows the
//! window's resource as it is now — not a picture of the sharer's screen: every reader's
//! desktop runs the resource's own app on its own copy of the record and draws that app's
//! view, scaled down, where the message asked for it.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use vello::kurbo::Rect;

#[path = "window_share_host.rs"]
pub mod host;

/// The most embeds one window may ask for at once: a host runs an app for each, so a long
/// scroll of shares shows the ones in view.
pub const MAX_EMBEDS: usize = 4;
const MAX_ASKS: usize = 8;
/// The most windows a desktop lists for the sharing app's picker.
pub const MAX_WINDOWS: usize = 64;
/// The most windows one app says it shares.
pub const MAX_SHARED: usize = 64;
/// Longest id, title or place the connection carries, in bytes: longer text is cut at a
/// character boundary.
const MAX_TEXT_BYTES: usize = 1024;

/// The app that shares windows into conversations and takes the desktop's asks
/// ([`take_ask`]): the desktop brings it forward when a window's menu asks to share.
pub const SHARING_APP: &str = "so.tilo.chat";

/// A resource as one app's view shows it: the app, the record it keeps the resource in
/// and the resource itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceView {
    pub app: String,
    pub record: String,
    pub resource: String,
}

/// One place an app wants a resource drawn by its own app.
#[derive(Clone, Debug, PartialEq)]
pub struct Embed {
    pub key: String,
    pub view: ResourceView,
    pub rect: Rect,
    pub clip: Rect,
}

/// How an embed is coming along, as its host reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum EmbedState {
    Loading,
    Showing,
    Unavailable(String),
}

/// A window its person has open on a record, as the desktop lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenWindow {
    pub tab: u64,
    pub app: String,
    pub app_title: String,
    pub title: String,
    pub record: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

impl OpenWindow {
    /// The resource the window shows, as one app's view of it.
    #[must_use]
    pub fn view(&self) -> ResourceView {
        ResourceView {
            app: self.app.clone(),
            record: self.record.clone(),
            resource: if let Some(item) = &self.item {
                item.clone()
            } else if self.record.is_empty() {
                self.kind.clone()
            } else {
                self.record.clone()
            },
        }
    }
}

/// A window the sharing app shares, as it tells the desktop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedWindow {
    pub message: String,
    pub app: String,
    pub record: String,
    pub place: String,
}

/// What the desktop asks of the sharing app on its person's behalf.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum Ask {
    Share(OpenWindow),
    Stop { message: String },
}

/// An [`Embed`] as the shell-ui connection carries it: its geometry as plain
/// logical-pixel arrays (`[x0, y0, x1, y1]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeclaredEmbed {
    pub key: String,
    pub view: ResourceView,
    pub rect: [f64; 4],
    pub clip: [f64; 4],
}

impl From<&Embed> for DeclaredEmbed {
    fn from(embed: &Embed) -> Self {
        let array = |rect: Rect| [rect.x0, rect.y0, rect.x1, rect.y1];
        Self {
            key: embed.key.clone(),
            view: embed.view.clone(),
            rect: array(embed.rect),
            clip: array(embed.clip),
        }
    }
}

impl DeclaredEmbed {
    /// The embed it declares, bounded; `None` for geometry that is not finite or has no
    /// area, which nothing could be drawn into.
    #[must_use]
    pub fn embed(&self) -> Option<Embed> {
        let rect = |[x0, y0, x1, y1]: [f64; 4]| {
            ([x0, y0, x1, y1].iter().all(|value| value.is_finite()) && x1 > x0 && y1 > y0)
                .then(|| Rect::new(x0, y0, x1, y1))
        };
        Some(Embed {
            key: bounded(&self.key),
            view: self.view.bounded(),
            rect: rect(self.rect)?,
            clip: rect(self.clip)?,
        })
    }
}

/// How the embed `key` is coming along, as the shell tells its app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyedEmbedState {
    pub key: String,
    pub state: EmbedState,
}

/// `text` cut to the most the connection carries of an id, a title or a place, at a
/// character boundary.
#[must_use]
pub fn bounded(text: &str) -> String {
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = text.to_owned();
    text.truncate(end);
    text
}

impl ResourceView {
    /// The same view with every name bounded as the connection bounds it.
    #[must_use]
    pub fn bounded(&self) -> Self {
        Self {
            app: bounded(&self.app),
            record: bounded(&self.record),
            resource: bounded(&self.resource),
        }
    }
}

impl OpenWindow {
    /// The same window with every name bounded as the connection bounds it.
    #[must_use]
    pub fn bounded(&self) -> Self {
        Self {
            tab: self.tab,
            app: bounded(&self.app),
            app_title: bounded(&self.app_title),
            title: bounded(&self.title),
            record: bounded(&self.record),
            kind: bounded(&self.kind),
            item: self.item.as_deref().map(bounded),
        }
    }
}

impl SharedWindow {
    /// The same share with every name bounded as the connection bounds it.
    #[must_use]
    pub fn bounded(&self) -> Self {
        Self {
            message: bounded(&self.message),
            app: bounded(&self.app),
            record: bounded(&self.record),
            place: bounded(&self.place),
        }
    }
}

impl Ask {
    /// The same ask with every name bounded as the connection bounds it.
    #[must_use]
    pub fn bounded(&self) -> Self {
        match self {
            Self::Share(window) => Self::Share(window.bounded()),
            Self::Stop { message } => Self::Stop {
                message: bounded(message),
            },
        }
    }
}

#[derive(Default)]
struct Desktop {
    windows: Vec<OpenWindow>,
    shared: Vec<SharedWindow>,
    shared_revision: u64,
    asks: VecDeque<Ask>,
    /// Whether a host draws embeds on this thread.
    draws: bool,
    own: Embeds,
    own_revision: u64,
}

thread_local! {
    static DESKTOP: RefCell<Desktop> = RefCell::new(Desktop::default());
}

/// Ask for `embeds` in the window the app is shown in, at most [`MAX_EMBEDS`] (the first
/// ones).
pub fn declare(mut embeds: Vec<Embed>) {
    embeds.truncate(MAX_EMBEDS);
    if let Some(window) = crate::window_toolbar::window() {
        window.declare_embeds(embeds);
        return;
    }
    DESKTOP.with_borrow_mut(|desktop| {
        if desktop.own.declare(embeds) {
            desktop.own_revision = desktop.own_revision.wrapping_add(1);
        }
    });
}

/// How the host says `key`'s embed is coming along; `None` before it said.
#[must_use]
pub fn state(key: &str) -> Option<EmbedState> {
    match crate::window_toolbar::window() {
        Some(window) => window.embed_state(key),
        None => DESKTOP.with_borrow(|desktop| desktop.own.states.get(key).cloned()),
    }
}

/// Whether a host draws this window's embeds.
#[must_use]
pub fn drawn() -> bool {
    DESKTOP.with_borrow(|desktop| desktop.draws)
}

/// The windows the person has open on a record, as the desktop last listed them.
#[must_use]
pub fn windows() -> Vec<OpenWindow> {
    DESKTOP.with_borrow(|desktop| desktop.windows.clone())
}

/// Tell the desktop which windows this app shares now.
pub fn publish_shared(shared: Vec<SharedWindow>) {
    DESKTOP.with_borrow_mut(|desktop| {
        if desktop.shared != shared {
            desktop.shared = shared;
            desktop.shared_revision = desktop.shared_revision.wrapping_add(1);
        }
    });
}

/// Whether the desktop asked the sharing app something it has not taken yet.
#[must_use]
pub fn asked() -> bool {
    DESKTOP.with_borrow(|desktop| !desktop.asks.is_empty())
}

/// The oldest thing the desktop asked of the sharing app and it has not taken yet.
#[must_use]
pub fn take_ask() -> Option<Ask> {
    DESKTOP.with_borrow_mut(|desktop| desktop.asks.pop_front())
}

/// The host draws embeds on this thread (`true`), or no longer does.
pub fn set_drawn(draws: bool) {
    DESKTOP.with_borrow_mut(|desktop| desktop.draws = draws);
}

/// List the windows the person has open on a record.
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

/// Hand the sharing app `ask`: the runner of an app in a process of its own, as the shell
/// asked it.
pub fn deliver_ask(ask: Ask) {
    push(ask);
}

fn push(ask: Ask) {
    DESKTOP.with_borrow_mut(|desktop| {
        if desktop.asks.len() >= MAX_ASKS {
            desktop.asks.pop_front();
        }
        desktop.asks.push_back(ask);
    });
}

/// The process window's embeds and their revision, when the revision is not `seen` (the
/// one last sent to the shell; 0 before any).
#[must_use]
pub fn own_embeds_since(seen: u64) -> Option<(u64, Vec<Embed>)> {
    DESKTOP.with_borrow(|desktop| {
        (desktop.own_revision != seen).then(|| (desktop.own_revision, desktop.own.declared.clone()))
    })
}

/// What the app shares and its revision, when the revision is not `seen` (the one last
/// sent to the shell; 0 before any).
#[must_use]
pub fn shared_since(seen: u64) -> Option<(u64, Vec<SharedWindow>)> {
    DESKTOP.with_borrow(|desktop| {
        (desktop.shared_revision != seen).then(|| (desktop.shared_revision, desktop.shared.clone()))
    })
}

/// How the shell says the process window's embeds are coming along: every one it draws,
/// so an embed it no longer names has no state.
pub fn set_own_states(states: Vec<KeyedEmbedState>) -> bool {
    DESKTOP.with_borrow_mut(|desktop| {
        let declared = &desktop.own.declared;
        let next: HashMap<String, EmbedState> = states
            .into_iter()
            .filter(|keyed| declared.iter().any(|embed| embed.key == keyed.key))
            .map(|keyed| (keyed.key, keyed.state))
            .collect();
        let changed = desktop.own.states != next;
        desktop.own.states = next;
        changed
    })
}

/// The shell that drew the embeds and listed the windows went away: a new one is told
/// everything afresh.
pub fn host_lost() {
    set_drawn(false);
    publish_windows(Vec::new());
    set_own_states(Vec::new());
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
            item: None,
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
