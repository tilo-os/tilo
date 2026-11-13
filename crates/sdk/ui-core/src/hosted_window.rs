//! The window around an app its host shows in its own process. An app in a process of its own reaches
//! its window's bar and its tab over its shell connection ([`crate::window_toolbar`],
//! [`crate::shell_tabs`]), which knows the asking app by its process. Apps a host mounts in its own
//! process (the desktop's Terminal tabs) share that process and its thread, so they cannot use those:
//! the host lends each of them the window it shows it in, a [`HostedWindow`] (through
//! [`HostContext::lend`](crate::application::HostContext::lend)).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::window_toolbar::{ToolbarEvent, WindowToolbar};

const MAX_PENDING: usize = 32;

/// One window a host shows an app in, shared by the host and the app.
#[derive(Clone)]
pub struct HostedWindow {
    inner: Arc<Mutex<State>>,
    id: u64,
}

impl Default for HostedWindow {
    fn default() -> Self {
        /// Windows made so far, so that no two of them ever share an identity: a cache keyed by it
        /// never mistakes one window for another.
        static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            inner: Arc::new(Mutex::new(State::default())),
            id: MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
struct State {
    toolbar: Option<WindowToolbar>,
    notice: Option<crate::window_toolbar::ToolbarNotice>,
    notice_presses: Vec<String>,
    reactions: Option<crate::window_toolbar::ToolbarReactions>,
    reaction_presses: Vec<String>,
    /// Whether its person may only look ([`crate::window_toolbar::show_view_only`]).
    view_only: bool,
    edited: bool,
    revision: u64,
    events: VecDeque<ToolbarEvent>,
    close: bool,
    /// The session-supervisor session the tab runs, as the supervisor named it to the app: how the host
    /// knows which tab a program typed in a terminal belongs to.
    session: Option<String>,
    /// The workspace the window is in, as its host knows it: an app that shares its process with other
    /// windows has no workspace of its own.
    workspace: Option<String>,
    floats: bool,
    edit: bool,
    search: Option<crate::window_toolbar::SearchOpening>,
    /// Where the app wants other apps' views of resources drawn, and how each is coming along
    /// ([`crate::window_share`]).
    embeds: crate::window_share::Embeds,
    /// What the host runs when the app declared or asked to close, so it looks again even while nothing
    /// else is happening.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for HostedWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        f.debug_struct("HostedWindow")
            .field("revision", &state.revision)
            .field("events", &state.events.len())
            .field("close", &state.close)
            .finish_non_exhaustive()
    }
}

impl HostedWindow {
    /// A window with no slots, nothing to hear and no close asked.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Which window this is: equal for clones of one window, and for no other window ever, so what a
    /// reader keeps of one window never answers for another
    /// ([`crate::window_toolbar::current_window`]).
    pub fn identity(&self) -> u64 {
        self.id
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // The state stays whole across a panic elsewhere: every write is
        // one assignment.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Declare the window's slots (`None`: none), bounded as [`crate::window_toolbar::declare`] bounds
    /// them.
    pub fn declare(&self, toolbar: Option<WindowToolbar>) {
        let toolbar = toolbar
            .map(crate::window_toolbar::declaration)
            .filter(|toolbar| !toolbar.is_empty());
        let wake = {
            let mut state = self.state();
            if state.toolbar == toolbar {
                return;
            }
            state.toolbar = toolbar;
            state.revision = state.revision.wrapping_add(1);
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Show `notice` in the bar (`None`: take it down), bounded as [`crate::window_toolbar::notify`]
    /// bounds it.
    pub fn notify(&self, notice: Option<crate::window_toolbar::ToolbarNotice>) {
        let wake = {
            let mut state = self.state();
            if state.notice == notice {
                return;
            }
            state.notice = notice;
            state.revision = state.revision.wrapping_add(1);
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Whether the notice's button `id` was pressed since the last call.
    #[must_use]
    pub fn take_notice_press(&self, id: &str) -> bool {
        let mut state = self.state();
        let before = state.notice_presses.len();
        state.notice_presses.retain(|pressed| pressed != id);
        state.notice_presses.len() != before
    }

    /// The notice up.
    #[must_use]
    pub fn notice(&self) -> Option<crate::window_toolbar::ToolbarNotice> {
        self.state().notice.clone()
    }

    /// Show `reactions` at the bar's end (`None`: none), bounded as
    /// [`crate::window_toolbar::show_reactions`] bounds them.
    pub fn show_reactions(&self, reactions: Option<crate::window_toolbar::ToolbarReactions>) {
        let wake = {
            let mut state = self.state();
            if state.reactions == reactions {
                return;
            }
            state.reactions = reactions;
            state.revision = state.revision.wrapping_add(1);
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Say whether the window's person may only look at what it shows.
    pub fn show_view_only(&self, view_only: bool) {
        let mut state = self.state();
        if state.view_only != view_only {
            state.view_only = view_only;
            state.revision = state.revision.wrapping_add(1);
        }
    }

    /// Whether the window's person may only look.
    #[must_use]
    pub fn view_only(&self) -> bool {
        self.state().view_only
    }

    /// Mark the window's document as having work not yet saved.
    pub fn show_edited(&self, edited: bool) {
        let wake = {
            let mut state = self.state();
            if state.edited == edited {
                return;
            }
            state.edited = edited;
            state.revision = state.revision.wrapping_add(1);
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Whether the window marks its document as not yet saved.
    #[must_use]
    pub fn edited(&self) -> bool {
        self.state().edited
    }

    /// The reactions shown.
    #[must_use]
    pub fn reactions(&self) -> Option<crate::window_toolbar::ToolbarReactions> {
        self.state().reactions.clone()
    }

    /// The emoji pressed among the reactions since the last call.
    #[must_use]
    pub fn take_reaction_presses(&self) -> Vec<String> {
        std::mem::take(&mut self.state().reaction_presses)
    }

    /// What the person did with the slots since the last call, oldest first.
    #[must_use]
    pub fn take_events(&self) -> Vec<ToolbarEvent> {
        self.state().events.drain(..).collect()
    }

    /// Close this window's tab, and the window with it when it is the only tab, the way the desktop
    /// closes any tab.
    pub fn close(&self) {
        let wake = {
            let mut state = self.state();
            if state.close {
                return;
            }
            state.close = true;
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Name the session-supervisor session this tab runs (`None`: none now), as the supervisor answered
    /// when it opened.
    pub fn set_session(&self, session: Option<String>) {
        self.state().session = session;
    }

    /// Put the window in `workspace` (a workspace id).
    pub fn set_workspace(&self, workspace: Option<String>) {
        self.state().workspace = workspace;
    }

    /// The workspace the window is in, when its host said.
    #[must_use]
    pub fn workspace(&self) -> Option<String> {
        self.state().workspace.clone()
    }

    /// The slots as last declared.
    #[must_use]
    pub fn declared(&self) -> Option<WindowToolbar> {
        self.state().toolbar.clone()
    }

    /// What the window's bar shows: the slots as declared, with the notice up and the reactions at the
    /// end, which are the window's own ([`Self::notify`], [`Self::show_reactions`]).
    #[must_use]
    pub fn shown(&self) -> Option<WindowToolbar> {
        let state = self.state();
        crate::window_toolbar::with_window_owned(
            state.toolbar.clone(),
            state.notice.clone(),
            state.reactions.clone(),
            state.view_only,
            state.edited,
        )
    }

    /// Bumped by every declaration that changed the slots.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.state().revision
    }

    /// Ask the bar to turn the location into a field, as a click on it does.
    pub fn edit_location(&self) {
        let wake = {
            let mut state = self.state();
            state.edit = true;
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Ask the bar to open the window's search, as a click on its control does.
    pub fn open_search(&self, opening: crate::window_toolbar::SearchOpening) {
        let wake = {
            let mut state = self.state();
            state.search = Some(opening);
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Ask for `embeds` in this window ([`crate::window_share::declare`]).
    pub fn declare_embeds(&self, embeds: Vec<crate::window_share::Embed>) {
        let wake = {
            let mut state = self.state();
            if !state.embeds.declare(embeds) {
                return;
            }
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// How the host said `key`'s embed is coming along.
    #[must_use]
    pub fn embed_state(&self, key: &str) -> Option<crate::window_share::EmbedState> {
        self.state().embeds.states.get(key).cloned()
    }

    /// The embeds the app asks for, as last declared.
    #[must_use]
    pub fn embeds(&self) -> Vec<crate::window_share::Embed> {
        self.state().embeds.declared.clone()
    }

    /// Say how `key`'s embed is coming along.
    pub fn set_embed_state(&self, key: &str, embed: crate::window_share::EmbedState) {
        let mut state = self.state();
        if state
            .embeds
            .declared
            .iter()
            .any(|declared| declared.key == key)
        {
            state.embeds.states.insert(key.to_owned(), embed);
        }
    }

    /// The session-supervisor session the tab runs, as the app named it.
    #[must_use]
    pub fn session(&self) -> Option<String> {
        self.state().session.clone()
    }

    /// Whether `other` is this same window (a clone of it).
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Whether the app asked to edit its location since the last call.
    #[must_use]
    pub fn take_edit_request(&self) -> bool {
        std::mem::take(&mut self.state().edit)
    }

    /// How the app asked to open the window's search since the last call.
    #[must_use]
    pub fn take_search_request(&self) -> Option<crate::window_toolbar::SearchOpening> {
        self.state().search.take()
    }

    /// What the host runs when the app declares new slots or asks to close.
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.state().wake = Some(Arc::new(wake));
    }

    /// The slots and their revision, when the revision is not `seen` (the one the host last showed;
    /// `None`: it showed none yet).
    #[must_use]
    pub fn toolbar_since(&self, seen: Option<u64>) -> Option<(u64, Option<WindowToolbar>)> {
        let revision = self.revision();
        (seen != Some(revision)).then(|| (revision, self.shown()))
    }

    /// Whether the window floats over the desk as a card ([`ToolbarEvent::Floating`]).
    #[must_use]
    pub fn floats(&self) -> bool {
        self.state().floats
    }

    /// Hand the app what the person did with its slots.
    pub fn deliver(&self, event: ToolbarEvent) {
        let mut state = self.state();
        if let ToolbarEvent::Floating { floating } = event {
            state.floats = floating;
            return;
        }
        if let Some(id) = crate::window_toolbar::notice_press(state.notice.as_ref(), &event) {
            if state.notice_presses.len() < MAX_PENDING {
                state.notice_presses.push(id);
            }
            return;
        }
        if let Some(press) = crate::window_toolbar::reaction_press(state.reactions.as_ref(), &event)
        {
            if let crate::window_toolbar::ReactionPress::Emoji(emoji) = press
                && state.reaction_presses.len() < MAX_PENDING
            {
                state.reaction_presses.push(emoji);
            }
            return;
        }
        if state.events.len() < MAX_PENDING {
            state.events.push_back(event.bounded());
        }
    }

    /// Whether the app asked to close its tab since the last call.
    #[must_use]
    pub fn take_close(&self) -> bool {
        std::mem::take(&mut self.state().close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_toolbar::ToolbarLocation;

    fn titled(label: &str) -> WindowToolbar {
        WindowToolbar {
            location: Some(ToolbarLocation {
                label: label.into(),
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }
    }

    #[test]
    fn the_host_sees_each_declaration_once_and_the_app_hears_its_events() {
        let window = HostedWindow::new();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&woken);
        window.set_wake(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let (first, slots) = window.toolbar_since(None).expect("nothing shown yet");
        assert_eq!(slots, None);
        window.declare(Some(titled("Your shell on this device")));
        let (second, slots) = window.toolbar_since(Some(first)).expect("declared");
        assert_eq!(slots, Some(titled("Your shell on this device")));
        window.declare(Some(titled("Your shell on this device")));
        assert_eq!(window.toolbar_since(Some(second)), None, "unchanged");
        window.deliver(ToolbarEvent::Commit {
            text: "target:host".into(),
        });
        assert_eq!(
            window.take_events(),
            vec![ToolbarEvent::Commit {
                text: "target:host".into()
            }]
        );
        assert!(window.take_events().is_empty());
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn a_close_asked_from_another_thread_reaches_the_host_once() {
        let window = HostedWindow::new();
        let app = window.clone();
        std::thread::spawn(move || app.close())
            .join()
            .expect("the reader thread");
        assert!(window.take_close());
        assert!(!window.take_close(), "one close per ask");
    }

    /// Apps sharing one process (the browser's) each learn their window's workspace from it, not from
    /// the process.
    #[test]
    fn an_app_is_in_the_workspace_its_window_is_in() {
        let window = HostedWindow::new();
        window.set_workspace(Some("e787c8a3-e6d5-4b00-90fe-ff1af708b979".into()));
        assert_eq!(crate::window_toolbar::workspace(), None, "outside it");
        let _running = crate::window_toolbar::enter(Some(&window));
        assert_eq!(
            crate::window_toolbar::workspace().as_deref(),
            Some("e787c8a3-e6d5-4b00-90fe-ff1af708b979")
        );
    }
}
