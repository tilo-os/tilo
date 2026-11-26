//! Desktop search on the surface: the shared [`DesktopSearch`] palette over what this desktop has
//! open, drawn by the chrome above the windows.
//! Search opens from the same chords as the native shell's (Super+K and Super+Space; Ctrl+K where
//! the platform's command key is Ctrl), from a `global-search` /`commands` navigation command, or
//! from [`DesktopHandle::toggle_search`]. A window's new-tab control opens it over the applications
//! only, and the one chosen opens as a tab of that window, as the native shell's does. The shell
//! keeps what search stands over and what it found; the chrome, which stands above the windows,
//! draws the card there, dims what it covers, routes the person's input to it and closes it
//! ([`DesktopHandle::search_palette`], [`DesktopHandle::search_area`]). Choosing an application
//! launches it, a window focuses it, and an application's action or content runs through the
//! palette's control invocation, which the frame host resolves against the live resource tree like
//! any bound control. Search over everything also answers what is not a name, as the native shell's
//! does: a sum or a unit conversion is copied, an address or a web search opens in a new browser
//! tab.

use tilo_desktop_model::{GroupId, Layout, StripId, TabId, TileContent};
use tilo_desktop_search::Target;
use tilo_desktop_search::providers::{CommandSpec, WindowEntry};
use ui::kurbo::Rect;
use ui::search::{AppScope, Scope};
use ui::winit::event::{ElementState, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use super::DesktopHandle;
use crate::desktop::navigation;

/// The search the windows of strip `strip` asked for: a new-tab control asks which application to
/// show there, a window's search control (or an app's own search button) asks what that window
/// shows, narrowed to the app that window has in front.
pub fn search_asked(
    tree: &mut Layout,
    strip: StripId,
    new_tab: Option<GroupId>,
    search_in: Option<(GroupId, ui::widget::search_request::SearchRequest)>,
) -> Option<SearchRequest> {
    let mut asked = new_tab.map(|group| SearchRequest::Open {
        scope: Scope::WindowGroup(Some(group)),
        query: None,
    });
    if let Some((group, request)) = search_in {
        let part = request.part().cloned();
        let app = super::find_group(tree, strip, group)
            .and_then(|window| window.tabs.iter().find(|tab| tab.id == window.active_tab))
            .and_then(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile))
            .map(String::from);
        asked = Some(SearchRequest::Open {
            scope: Scope::InApp(AppScope { app, part }),
            query: request.query,
        });
    }
    asked
}

/// What a search reaches, as the shell publishes it: `workspace`,
/// `everywhere`, `app:<package id>` for one app's own search, or `new-tab`
/// for the apps a window group can show.
pub fn scope_name(scope: &Scope) -> String {
    match scope {
        Scope::Workspace => "workspace".to_owned(),
        Scope::Everywhere => "everywhere".to_owned(),
        Scope::InApp(app) => app
            .app
            .as_deref()
            .map_or_else(|| "window".to_owned(), |app| format!("app:{app}")),
        Scope::WindowGroup(_) => "new-tab".to_owned(),
    }
}

impl DesktopHandle {
    /// Offer these host actions from now on, in place of earlier ones. Returns the ids no longer
    /// offered: a banner still asking for one of them asks for something already done, so the host
    /// withdraws it ([`crate::desktop::state::notices::DesktopNotifications::withdraw_host_action`]).
    pub fn set_host_actions(&self, actions: Vec<HostAction>) -> Vec<String> {
        let mut shared = self.shared.borrow_mut();
        let withdrawn: Vec<String> = shared
            .host_actions
            .iter()
            .filter(|old| actions.iter().all(|new| new.id != old.id))
            .map(|old| old.id.clone())
            .collect();
        shared.host_actions = actions;
        drop(shared);
        self.withdraw_personal_ask(&withdrawn);
        withdrawn
    }

    /// The host actions offered now, in the host's order.
    pub fn host_actions(&self) -> Vec<HostAction> {
        self.shared.borrow().host_actions.clone()
    }

    /// Whether the host offers the action `id` (see [`Self::set_host_actions`]).
    pub fn offers_host_action(&self, id: &str) -> bool {
        self.shared
            .borrow()
            .host_actions
            .iter()
            .any(|action| action.id == id)
    }

    /// Choose the host action `id` for the person, as their pick in search or a banner does: the
    /// host carries it out on its next pass ([`Self::take_host_choices`]). An app asks for one
    /// through the shell ([`tilo_action_services::session_actions::SHOW_CONNECT`]).
    pub fn choose_host_action(&self, id: &str) {
        self.shared.borrow_mut().host_choices.push(id.to_owned());
        self.wake();
    }

    /// The host actions the person chose since the last call, by id.
    pub fn take_host_choices(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.borrow_mut().host_choices)
    }

    /// Open search, or close it when it is open. The host is asked for the pass that opens it.
    pub fn toggle_search(&self) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Toggle);
        self.wake();
    }

    /// Close search, wherever it stands; nothing to close is nothing to do. The host is asked for
    /// the pass that closes it. A host that sees a press the shell's own hit test never reaches (a page's chrome
    /// beside the canvas) dismisses the search holding the keyboard this way, as a press beside
    /// the card does on the shell itself, however often it asks.
    pub fn close_search(&self) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Close);
        self.wake();
    }

    /// Open search at `scope` , with `query` typed. The host is asked for the pass that opens it.
    pub fn open_search(&self, scope: Scope, query: Option<String>) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Open { scope, query });
        self.wake();
    }

    /// Whether the person asked for search and it is not up yet: its request waits for the next
    /// update pass. What they type meanwhile is the search's ([`Self::type_ahead`]).
    fn search_opening(&self) -> bool {
        let shared = self.shared.borrow();
        !shared.search_open
            && matches!(
                shared.search_request,
                Some(SearchRequest::Open { .. } | SearchRequest::Toggle)
            )
    }

    /// Take `event` for the search the person asked for while it is not up yet
    /// ([`Self::search_opening`]): what they type goes into its field once it opens, Backspace takes
    /// back what they typed, Enter chooses the top row of what that finds, and Escape leaves the
    /// search before it opened. Every key and input method's text is the search's meanwhile, so
    /// none reaches the window that had the keyboard before they asked; a chord's control
    /// characters are not text. Whether the search took it.
    pub fn type_ahead(&self, event: &WindowEvent) -> bool {
        if !self.search_opening() {
            return false;
        }
        let text = match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                if key.state != ElementState::Pressed {
                    return true;
                }
                match &key.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        let mut shared = self.shared.borrow_mut();
                        shared.search_request = None;
                        shared.typed_ahead = TypedAhead::default();
                        shared.search.borrow_mut().close();
                        return true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.shared.borrow_mut().typed_ahead.enter = true;
                        self.wake();
                        return true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.shared.borrow_mut().typed_ahead.text.pop();
                        return true;
                    }
                    _ => key.text.as_deref(),
                }
            }
            WindowEvent::Ime(ui::winit::event::Ime::Commit(text)) => Some(text.as_str()),
            WindowEvent::Ime(_) => None,
            _ => return false,
        };
        if let Some(text) = text.filter(|text| !text.chars().any(char::is_control)) {
            self.shared.borrow_mut().typed_ahead.text.push_str(text);
        }
        true
    }

    /// The row the host carried out did not happen, for `why` : search opens again where it stood,
    /// with the person's query, saying why on that row rather than closing over a failure. False
    /// when the row is not the one last carried out (search has moved on since).
    pub fn answer_search(&self, target: &Target, why: String) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some((scope, query, chosen)) = shared.search_choice_context.take() else {
            return false;
        };
        if &chosen != target {
            return false;
        }
        shared.search_refusal = Some((chosen, why));
        shared.search_request = Some(SearchRequest::Open {
            scope,
            query: Some(query),
        });
        drop(shared);
        self.wake();
        true
    }

    /// Search `tab` 's app, the way an app in a process of its own asked
    /// (`ui::floating_surface::FloatingSurfaceClientMessage::OpenAppSearch`): inside a session
    /// there is one search, the shell's, and the app opens none of its own. A tab that shows no app
    /// searches nothing.
    pub fn search_app(&self, tab: TabId, request: ui::widget::search_request::SearchRequest) {
        let mut shared = self.shared.borrow_mut();
        let Some(app) = shared
            .tree
            .tab(tab)
            .and_then(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile))
            .map(String::from)
        else {
            return;
        };
        shared.search_request = Some(SearchRequest::Open {
            scope: Scope::InApp(AppScope {
                app: Some(app),
                part: request.part.clone(),
            }),
            query: request.query,
        });
    }

    /// Whether the search palette is open, as of the last update pass.
    pub fn search_open(&self) -> bool {
        self.shared.borrow().search_open
    }

    /// Who holds the keyboard: `"system-search"` while the search palette is up (it takes the
    /// keyboard, the pointer and Escape), else the application of the active workspace's focused
    /// window. `None` while no window does — an empty shell, or one whose windows are all gone.
    ///
    /// A host that stands outside the shell's own hit test (a page's chrome over the canvas) reads
    /// this to know whose input a press elsewhere belongs to.
    pub fn input_owner(&self) -> Option<String> {
        let shared = self.shared.borrow();
        if shared.search_open {
            return Some("system-search".to_owned());
        }
        let route = crate::desktop::navigation::active_route(&shared.tree)?;
        let window = crate::desktop::navigation::locate(&shared.tree, route)?;
        let tab = window.tabs.iter().find(|tab| tab.id == route.tab)?;
        tilo_desktop_model::apps::canonical_app_id(&tab.tile).map(String::from)
    }

    /// The palette the shell searches with, shared with the host that draws it over the windows: the
    /// shell fills it with what its windows hold and carries out what is chosen there, while that
    /// host draws it, routes the person's input to it and closes it. [`Self::note_search_activity`]
    /// tells the shell what that host changed.
    pub fn search_palette(
        &self,
    ) -> std::rc::Rc<std::cell::RefCell<tilo_desktop_search::DesktopSearch>> {
        std::rc::Rc::clone(&self.shared.borrow().search)
    }

    /// Where the palette stands when it was opened for one window: that window under its bar, over
    /// the rest of it, as the shell last laid it out; nothing over everything.
    pub fn search_area(&self) -> Option<Rect> {
        self.shared.borrow().search_area
    }

    /// Changes to what the palette shows or asks of the shell, counted: a host
    /// that changed the palette without asking the shell to (typing in it,
    /// opening it) bumps it, and the shell takes that up on its next pass.
    pub fn note_search_activity(&self) {
        let mut shared = self.shared.borrow_mut();
        shared.search_revision = shared.search_revision.wrapping_add(1);
    }

    /// The [`Self::note_search_activity`] count as of the last update pass.
    pub fn search_revision(&self) -> u64 {
        self.shared.borrow().search_revision
    }

    /// Whether the shell itself takes the keys the person types: its notification centre is open,
    /// or a field in a window's bar (a tab being named), and what they type goes into it rather
    /// than to the window behind. Its search does not: the palette stands over the windows, drawn
    /// by the host above them, which takes the keys while it is up.
    pub fn wants_keyboard(&self) -> bool {
        self.bar_takes_typing()
            || self
                .shared
                .borrow()
                .presentation
                .notifications
                .as_ref()
                .is_some_and(|notifications| notifications.center_open)
    }

    /// Search from now on also over what `host` adds, and hand it what is chosen that it carries
    /// out ([`SearchHost::performs`]). A search open now opens again over what it adds.
    pub fn set_search_host(&self, host: std::rc::Rc<dyn SearchHost>) {
        let mut shared = self.shared.borrow_mut();
        shared.search_host = Some(host);
        if shared.search_request.is_none() {
            shared.search_request = Some(SearchRequest::Again);
        }
        drop(shared);
        self.wake();
    }

    /// The rows chosen in search since the last call that the host carries out, in order, each
    /// with the compositor's serial of the person's press on the system's own surfaces that chose
    /// it, when one did.
    pub fn take_search_choices(&self) -> Vec<(Target, Option<u32>)> {
        std::mem::take(&mut self.shared.borrow_mut().search_choices)
    }

    /// The open search's query and the rows it shows, as of the last update pass; nothing while
    /// search is closed.
    pub fn search_rows(&self) -> (String, Vec<SearchRow>) {
        let shared = self.shared.borrow();
        (
            shared.shown_search.query.clone(),
            shared.shown_search.rows.clone(),
        )
    }

    /// What the open search reaches (`workspace`, `everywhere`, one app's own
    /// search, the apps a group shows): a window's search and the shell's are
    /// different searches, and a page or a test reading what the shell
    /// published can tell them apart. `None` while search is closed.
    pub fn search_scope(&self) -> Option<String> {
        let shared = self.shared.borrow();
        (!shared.shown_search.scope.is_empty()).then(|| shared.shown_search.scope.clone())
    }

    /// Whether the open search is still filling in, as of the last update pass: a source has still
    /// to answer for the query, or the rows shown are not cut to the card yet, so the rows will
    /// change. What the shell publishes as a search still searching.
    pub fn search_searching(&self) -> bool {
        self.shared.borrow().shown_search.searching
    }

    /// Whether the open search still expects rows to join the ones shown, as
    /// of the last update pass: a source is still out, so the shell looks
    /// again shortly and draws the row when it arrives. It is not "still
    /// searching": the rows shown are the search's answer.
    pub fn search_expecting(&self) -> bool {
        self.shared.borrow().shown_search.expecting
    }

    /// The field the person types into in the desktop itself, as of the last
    /// update pass: the open search's query field.
    ///
    /// # Errors
    /// Returns why the desktop has no field to write into: none takes typing,
    /// or the one that does takes a secret.
    pub fn typed_field(&self) -> Result<TypedField, TypedRefusal> {
        self.shared.borrow().typed_field
    }

    /// Write committed text (a dictation's) into `field` , on the next update pass, if the person
    /// is still typing into it then.
    #[must_use]
    pub fn commit_typed(&self, field: TypedField, text: &str) -> TypedInsertion {
        let mut shared = self.shared.borrow_mut();
        match shared.typed_field {
            Ok(current) if current == field => {}
            Ok(_) => return TypedInsertion::Changed,
            Err(refusal) => return TypedInsertion::Refused(refusal),
        }
        shared.typed_ask = Some(TypedAsk {
            field,
            text: text.to_owned(),
        });
        TypedInsertion::Inserted
    }

    /// Choose the open search's row `index` ([`Self::search_rows`]), with `argument` the line its
    /// action asks for and `way` the modifier it was chosen with (`here`, `beside` or `more`), on
    /// the next update pass: as the person's press on the system's own surfaces that chooses it
    /// now, when one does.
    pub fn choose_search_row(&self, index: usize, argument: Option<String>, way: Option<String>) {
        let press = tilo_actions::remote::press_points::handled();
        self.shared
            .borrow_mut()
            .search_asks
            .push((index, argument, way, press));
    }
}

/// The search chords: Super+K and Super+Space as in the native shell, and Ctrl+K where Ctrl is the
/// platform's command key.
pub fn is_search_chord(
    event: &WindowEvent,
    modifiers: ui::winit::event::Modifiers,
    platform: tilo_ui_base::platform::PlatformConventions,
) -> bool {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return false;
    };
    if key.state != ElementState::Pressed || key.repeat {
        return false;
    }
    let mods = modifiers.state();
    if mods.alt_key() || mods.shift_key() {
        return false;
    }
    let command = mods.super_key()
        || (mods.control_key() && platform.platform() != tilo_ui_base::platform::Platform::MacOs);
    match &key.logical_key {
        Key::Character(text) => command && text.eq_ignore_ascii_case("k"),
        Key::Named(NamedKey::Space) => mods.super_key() && !mods.control_key(),
        _ => false,
    }
}

/// Escape as a person presses it; a held key counts as each press it repeats,
/// as the old launcher did: the first clears, the next closes.
pub fn is_escape(event: &WindowEvent) -> bool {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return false;
    };
    key.state == ElementState::Pressed && matches!(&key.logical_key, Key::Named(NamedKey::Escape))
}
/// What a host asked of search, taken on the next update pass.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchRequest {
    /// Open search at a [`Scope`], with `query` typed.
    Open {
        scope: Scope,
        query: Option<String>,
    },
    Toggle,
    /// Close search wherever it stands: a host dismissing the one that holds the keyboard, which
    /// asks nothing of what is under it and does nothing when none is open.
    Close,
    /// Open the open search again where it stands, with the person's query: the host changed.
    Again,
}

/// What the person typed after asking for search and before it was up: the search's, as the keys
/// typed into it once it is. Its field takes the text when it opens, and an Enter among it chooses
/// the top row of what that text finds, as an Enter typed ahead of its rows does in an open search.
#[derive(Debug, Default)]
pub struct TypedAhead {
    pub text: String,
    pub enter: bool,
}

/// What a host adds to the desktop's search over everything, collected as it opens
/// ([`SearchHost::feed`]).
#[derive(Default)]
pub struct SearchHostFeed {
    /// The apps this host opens, beside those the desktop's tile source lists.
    pub applications: Vec<tilo_desktop_search::providers::AppEntry>,
    /// What providers publish through the host's action host, run by address there.
    pub published: Vec<tilo_desktop_search::providers::PublishedActions>,
    /// What fills a resource-kind parameter's field: the workspace's items of the kind, by the
    /// list action the kind names, asked through the host's action host
    /// ([`tilo_desktop_search::providers::ResourceSource`]).
    pub resource_options:
        Option<std::sync::Arc<dyn tilo_desktop_search::providers::ResourceSource>>,
    /// The host's own sources, each over what is published or a service answers.
    pub sources: Vec<tilo_desktop_search::Held>,
    /// The sources the person switched off.
    pub preferences: tilo_desktop_search::SearchPreferences,
    /// Now, in Unix seconds.
    pub now: u64,
    /// What the person chose before, as the host keeps it; `None` keeps what search learned while
    /// the desktop runs.
    pub usage: Option<tilo_desktop_search::Usage>,
}

/// The field the person types into in the desktop itself: the open search's query field, where text
/// committed from outside the keyboard (a dictation's) may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedField {
    /// The field's identity for the desktop's life.
    pub id: u64,
    /// The revision of its text and selection: any edit or caret move since moves it, so committed
    /// text can tell whether the field is still the one it was meant for.
    pub revision: u64,
    /// The field takes more than one line.
    pub multiline: bool,
}

/// Why the desktop's own field may not take committed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedRefusal {
    /// The desktop has no field the person types into.
    NoField,
    /// The field takes a secret, so words spoken elsewhere never go into it.
    Secret,
}

/// What became of committed text in the desktop's own field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedInsertion {
    /// The field took the text.
    Inserted,
    /// The field is no longer the one the text was meant for: nothing went in.
    Changed,
    /// The field may not take committed text any more.
    Refused(TypedRefusal),
}

/// Text committed into the desktop's own field, for the next update pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedAsk {
    /// The field the text was committed into, and its revision then.
    pub field: TypedField,
    pub text: String,
}

/// The identity of the search query field, for the desktop's life.
pub const QUERY_FIELD: u64 = 1;

/// The key the query field's revision is fingerprinting with: it never leaves the process, and only
/// says whether the field moved.
pub const TYPING_KEY: &[u8; 32] = b"desktop typing field revision 1!";

/// The open search's query field, as committed text may write into it. The palette draws it in the
/// desktop itself, so no text-input boundary of the windows the shell shows ever sees it.
pub fn typed_field(
    search: &tilo_desktop_search::DesktopSearch,
) -> Result<TypedField, TypedRefusal> {
    let Some(field) = search.palette().query_field(TYPING_KEY) else {
        return Err(TypedRefusal::NoField);
    };
    if field.secret {
        return Err(TypedRefusal::Secret);
    }
    let mut revision = [0u8; 8];
    revision.copy_from_slice(&field.fingerprint[..8]);
    Ok(TypedField {
        id: QUERY_FIELD,
        revision: u64::from_le_bytes(revision),
        multiline: field.multiline,
    })
}

/// A host that adds to the desktop's search and carries out what is chosen there that only it can:
/// running an action by address through its action host, starting an app, opening an address.
pub trait SearchHost {
    /// What to search beyond the desktop's own, in the workspace
    /// `workspace`, narrowed to it unless `scope` reaches every workspace
    /// ([`Scope::Everywhere`]): then the rows of every workspace are offered
    /// too, each saying which workspace it is in. Called as search opens,
    /// inside the desktop's update pass: it reads what the host keeps, never
    /// the desktop's own tree. A search over the workspace, one over every
    /// workspace and a window group's new tab ask it; an app's own search
    /// does not.
    fn feed(&self, workspace: &str, scope: &Scope) -> SearchHostFeed;
    /// The apps this host opens, each an id a launch accepts, whether or not a search is over a
    /// workspace: a window group's new tab and the empty shell's launchpad list the applications
    /// alone, and the apps a person can open from there are this host's. A host whose tile source
    /// already lists them answers nothing here.
    fn applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        Vec::new()
    }
    /// Whether the host carries out `target` itself; the desktop then hands it back through
    /// [`DesktopHandle::take_search_choices`].
    fn performs(&self, target: &Target) -> bool;
    /// The person chose a row: `usage` is what search keeps of their choices now, for the host to
    /// keep.
    fn chose(&self, _usage: &tilo_desktop_search::Usage) {}
}

/// One row the open search shows, as the desktop publishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRow {
    pub title: String,
    pub subtitle: Option<String>,
    /// The group it stands in ("Apps", "Settings").
    pub kind: String,
    /// What the row says about the thing beyond its name: where it lives,
    /// when it was last edited and by whom, where the person was there, in
    /// the source's own words.
    pub facts: Vec<String>,
}

/// The open search as the shell last showed it: its query and rows, and whether it is still filling
/// in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShownSearch {
    pub query: String,
    /// What the search reaches: the workspace in front, every workspace, one
    /// app's own, or the apps a window group can show. A person's search and a
    /// window's are different searches of the same shell, so what the shell
    /// publishes says which one answered.
    pub scope: String,
    pub rows: Vec<SearchRow>,
    /// The rows shown are not the search's answer yet: they are not cut to
    /// the card, or nothing is ready to show. What the shell publishes as a
    /// search still searching — and what holds Enter from a search that has
    /// nothing to press.
    pub searching: bool,
    /// Rows are still expected to join the ones shown (a window's content
    /// index being built, an app's server still looking). What the shell
    /// looks again for, so a late row is drawn when it arrives, without
    /// calling the ready rows "still searching".
    pub expecting: bool,
}

pub const CLOSE_WINDOW: &str = "desktop.close-window";
pub const MAXIMIZE_WINDOW: &str = "desktop.maximize-window";
/// Search command ids and banner action keys of the host's own actions.
pub const HOST_PREFIX: &str = "host:";
/// The least workspace the palette keeps from the surface's sides.
pub const PALETTE_EDGE: f64 = 8.0;

/// An action only the host that embeds the desktop can perform (in a browser: keeping the person's
/// identity with a passkey). The desktop offers it in search beside its own commands and on banners
/// the host posts, and hands the id back when the person chooses it
/// ([`DesktopHandle::take_host_choices`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct HostAction {
    pub id: String,
    /// What the action is called: its name, as the menus and search list it ("Leave workspace"),
    /// never the sentence describing it, which is `subtitle` .
    pub title: String,
    /// What a list of its kind names it by, where that is not its title: the workspace an
    /// `Open …` action goes to, as the workspace switcher lists it.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// The action's own picture, by its Lucide name ([`tilo_ui_base::icons::Icon::name`]): every list of it
    /// draws the one the action declares, never a generic glyph of its kind.
    #[serde(default)]
    pub icon: Option<String>,
    /// Taking something away from the person (leaving this workspace,
    /// logging out): a menu draws it as that, and search never offers it
    /// before anything is typed.
    #[serde(default)]
    pub destructive: bool,
    /// Search offers it with nothing typed: what a person may want from the empty card, which is
    /// making something or bringing someone in, never a verb that only takes something away or
    /// closes this computer down.
    #[serde(default)]
    pub unasked: bool,
    /// What the action is about, named as the source of the notices that offer it.
    #[serde(default)]
    pub source: Option<HostActionSource>,
}

/// The part of the system a host action belongs to, in the desktop's language: the source line of a
/// notice that offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostActionSource {
    /// Who the person is: keeping, recovering or naming their identity.
    Identity,
    /// The person's other devices: pairing one.
    Devices,
    /// Another of the person's workspaces, to go to.
    Workspaces,
    /// Workspaces themselves: joining one, leaving this one, a new one.
    Workspace,
    /// The session itself, on this computer: locking the screen, logging out, restarting the
    /// computer, shutting it down.
    Session,
    /// The person's own desktop: their private one, which is no workspace of
    /// theirs and is offered apart from them.
    Desktop,
}

impl HostActionSource {
    /// The name the person sees an offer from it under ("Devices").
    #[must_use]
    pub fn label(self) -> String {
        crate::desktop::i18n::t(match self {
            Self::Identity => "host-action-source-identity",
            Self::Devices => "host-action-source-devices",
            Self::Workspaces | Self::Workspace => "host-action-source-workspace",
            Self::Desktop => "action-source-desktop",
            Self::Session => "host-action-source-session",
        })
    }
}

impl HostAction {
    /// The name the person sees this offer from.
    #[must_use]
    pub fn source_label(&self) -> String {
        self.source.map(HostActionSource::label).unwrap_or_default()
    }

    /// The banner action key this action is offered under: a banner button with this key hands `id`
    /// back when the person chooses it, so a host offering two answers to one ask (approve or
    /// decline a device) can make both its own.
    #[must_use]
    pub fn key(&self) -> String {
        host_action_key(&self.id)
    }
}

/// The banner action key host action `id` is offered under ([`HostAction::key`]).
#[must_use]
pub fn host_action_key(id: &str) -> String {
    format!("{HOST_PREFIX}{id}")
}

pub fn is_host_offer(notification: &crate::desktop::notifications::Notification) -> bool {
    notification
        .actions
        .iter()
        .any(|(key, _)| host_choice(key).is_some())
}

/// Whether `notification` offers host action `id` .
pub fn offers(notification: &crate::desktop::notifications::Notification, id: &str) -> bool {
    notification
        .actions
        .iter()
        .any(|(key, _)| host_choice(key) == Some(id))
}

/// The host action a banner action key or search command id names.
pub fn host_choice(key: &str) -> Option<&str> {
    key.strip_prefix(HOST_PREFIX)
}

/// The search chord `is_search_chord` answers to, as the keyboard spells it on `platform` : ⌘K on a
/// Mac, where Ctrl+K does not open search, and Ctrl K elsewhere.
pub fn search_chord_keys(platform: tilo_ui_base::platform::PlatformConventions) -> Vec<String> {
    let keycaps = tilo_ui_base::keycaps::active();
    if platform.platform() == tilo_ui_base::platform::Platform::MacOs {
        keycaps
            .on_family(tilo_ui_base::interaction::KeyboardFamily::Apple)
            .chord_keys(["Super"], "k")
    } else {
        keycaps.chord_keys(["Ctrl"], "k")
    }
}

/// Every open window as search lists it: called by what it shows and the app it shows it in
/// (`Groceries — Notes`), with the app for its glyph. Only windows called alike say more: where
/// each of them is, or which of them it is, counted left to right.
pub fn window_app(tree: &Layout, group: GroupId) -> Option<String> {
    let window = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|window| window.id == group)?;
    let tab = window.tabs.iter().find(|tab| tab.id == window.active_tab)?;
    match &tab.tile {
        TileContent::LocalApp { label, .. } => Some(label.clone()),
        _ => None,
    }
}

pub fn windows<S: std::hash::BuildHasher>(
    tree: &Layout,
    holders: &std::collections::HashMap<TabId, ui::widget::TabHolder, S>,
) -> Vec<WindowEntry> {
    let mut out = Vec::new();
    for workspace in &tree.workspaces {
        for (at, strip) in workspace.strips.iter().enumerate() {
            let strip_name = crate::desktop::names::strip_named(&strip.name, at);
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile);
                    // The app's own name, never the place it shows: a window on "Sheet1" is a
                    // Sheets window, and a person typing "sheets" finds it.
                    let app_title = app
                        .as_ref()
                        .map_or_else(String::new, |app| tilo_desktop_search::names::app_name(app));
                    let place = holders
                        .get(&tab.id)
                        .map(|holder| holder.name.clone())
                        .into_iter()
                        .chain([
                            crate::desktop::names::workspace(&workspace.name),
                            strip_name.clone(),
                        ])
                        .collect();
                    // A window's name by the one rule every surface names a window by: its title
                    // and the app it shows, or the app alone where the title only names the app.
                    let title = app.as_ref().map_or_else(
                        || tab.title.clone(),
                        |app| {
                            tilo_desktop_search::names::window_name_of(app, &app_title, &tab.title)
                        },
                    );
                    out.push(WindowEntry {
                        tab: tab.id,
                        title,
                        app_title,
                        app: app.map(String::from),
                        place,
                    });
                }
            }
        }
    }
    let alike = |a: &WindowEntry, b: &WindowEntry| a.title == b.title && a.place == b.place;
    let counted: Vec<(usize, usize)> = out
        .iter()
        .enumerate()
        .map(|(at, entry)| {
            // Counting itself: the first of them is the first.
            let nth = out
                .iter()
                .take(at.saturating_add(1))
                .filter(|other| alike(other, entry))
                .count();
            let all = out.iter().filter(|other| alike(other, entry)).count();
            (nth, all)
        })
        .collect();
    for (entry, (nth, all)) in out.iter_mut().zip(counted) {
        if all > 1 {
            entry.place.push(crate::desktop::i18n::t_args(
                "search-window-nth",
                &[("nth", nth.into()), ("count", all.into())],
            ));
        } else {
            // Where a window is needs saying only when another is called the same: a lone window's
            // name is enough to find it by.
            entry.place.clear();
        }
    }
    out
}

/// The workspace that holds `strip`, as search names it: where a window's search reads what its
/// app published.
pub fn workspace_holding(tree: &Layout, strip: StripId) -> Option<String> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.strips.iter().any(|held| held.id == strip))
        .map(|workspace| workspace.id().to_string())
}

/// What search offers to do, in this order: the host's actions (a workspace's own, the session's),
/// then what the focused window allows (no close for a window someone else holds, `held` ), then
/// the other workspaces. Each row says whether it may stand there with nothing typed: closing a
/// window or moving to another workspace is something a person goes looking for, and typing still
/// finds every one of them.
pub fn commands(
    tree: &Layout,
    host: &[HostAction],
    held: &dyn Fn(TabId) -> bool,
) -> Vec<CommandSpec> {
    let mut out: Vec<CommandSpec> = host
        .iter()
        .map(|action| CommandSpec {
            id: format!("{HOST_PREFIX}{}", action.id),
            title: action.title.clone(),
            subtitle: action.subtitle.clone(),
            icon: action
                .icon
                .as_deref()
                .and_then(tilo_ui_base::icons::Icon::from_name),
            unasked: action.unasked && !action.destructive,
        })
        .collect();
    if let Some(ids) = navigation::active_route(tree)
        && let Some(group) = navigation::locate(tree, ids)
    {
        let title = group
            .tabs
            .iter()
            .find(|tab| tab.id == ids.tab)
            .map(|tab| tab.title.clone());
        if !held(ids.tab) {
            out.push(CommandSpec {
                id: CLOSE_WINDOW.into(),
                title: "Close window".into(),
                subtitle: title.clone(),
                icon: Some(tilo_ui_base::icons::Icon::X),
                unasked: false,
            });
        }
        out.push(CommandSpec {
            id: MAXIMIZE_WINDOW.into(),
            title: if group.maximized {
                "Restore window size".into()
            } else {
                "Maximize window".into()
            },
            subtitle: title,
            icon: Some(if group.maximized {
                tilo_ui_base::icons::Icon::Minimize2
            } else {
                tilo_ui_base::icons::Icon::Maximize2
            }),
            unasked: false,
        });
    }
    for workspace in &tree.workspaces {
        if workspace.id() != tree.active {
            out.push(CommandSpec {
                id: format!(
                    "{}{}",
                    tilo_desktop_search::WORKSPACE_COMMAND_PREFIX,
                    workspace.id()
                ),
                title: format!(
                    "Switch to {}",
                    crate::desktop::names::workspace(&workspace.name)
                ),
                subtitle: Some("Workspace".into()),
                icon: Some(tilo_ui_base::icons::Icon::LayoutGrid),
                unasked: false,
            });
        }
    }
    out
}

#[cfg(test)]
mod host_action_tests {
    use super::*;
    use crate::desktop::state::DesktopHandle;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};

    fn desktop() -> DesktopHandle {
        let mut minter = IdMinter::new(7);
        let tab = Tab {
            id: minter.mint(),
            title: "App0".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.app0".into(),
                label: "App0".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = crate::desktop::seed_tree(&mut minter, "Office", vec![tab]);
        DesktopHandle::new(tree, 7)
    }

    /// A host action no longer offered is reported once, by its id, so whoever shows it can take it
    /// away: what stays offered is not withdrawn, and what is new is not either.
    #[test]
    fn host_actions_no_longer_offered_are_reported_for_withdrawal() {
        let handle = desktop();
        let action = |id: &str| HostAction {
            id: id.into(),
            title: id.into(),
            label: None,
            subtitle: None,
            icon: None,
            destructive: false,
            unasked: false,
            source: None,
        };
        assert!(
            handle
                .set_host_actions(vec![action("keep"), action("pair")])
                .is_empty()
        );
        assert_eq!(
            handle.set_host_actions(vec![action("pair"), action("recover")]),
            ["keep"]
        );
    }
}
