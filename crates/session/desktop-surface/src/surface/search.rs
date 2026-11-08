//! Desktop search on the surface: the shared [`DesktopSearch`] palette over what this desktop has
//! open, drawn by the chrome above the windows.
//! Search opens from the same chords as the native shell's (Super+K and Super+Space; Ctrl+K where
//! the platform's command key is Ctrl), from a `global-search` /`commands` navigation command, or
//! from [`DesktopHandle::toggle_search`]. A window's new-tab control opens it over the applications
//! only, and the one chosen opens as a tab of that window, as the native shell's does. The desk
//! keeps what search stands over and what it found; the chrome, which stands above the windows,
//! draws the card there, dims what it covers, routes the person's input to it and closes it
//! ([`DesktopHandle::search_palette`], [`DesktopHandle::search_area`]). Choosing an application
//! launches it, a window focuses it, and an application's action or content runs through the
//! palette's control invocation, which the frame host resolves against the live resource tree like
//! any bound control. Search over everything also answers what is not a name, as the native shell's
//! does: a sum or a unit conversion is copied, an address or a web search opens in a new browser
//! tab.

use std::rc::Rc;

use tilo_desktop_model::{DesktopTree, GroupId, StripId, TabId, TileContent};
use tilo_desktop_search::providers::{CommandSpec, WindowEntry};
use tilo_desktop_search::{LaunchPlace, SearchData, Target};
use ui::automerge::AutoCommit;
use ui::kurbo::Rect;
use ui::search::{AppScope, Scope};
use ui::widget::Widget;
use ui::winit::event::{ElementState, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use super::{DesktopHandle, DesktopSurface, holder_refusal, route_of};
use crate::navigation::{self, Admitted};

/// What a host asked of search, taken on the next update pass.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum SearchRequest {
    /// Open search at a [`Scope`], with `query` typed.
    Open {
        scope: Scope,
        query: Option<String>,
    },
    Toggle,
}

/// The search the windows of strip `strip` asked for: a new-tab control asks which application to
/// show there, a window's search control (or an app's own search button) asks what that window
/// shows, narrowed to the app that window has in front.
pub(super) fn search_asked(
    tree: &mut DesktopTree,
    strip: StripId,
    new_tab: Option<GroupId>,
    search_in: Option<(GroupId, ui::search::SearchRequest)>,
) -> Option<SearchRequest> {
    let mut asked = new_tab.map(|group| SearchRequest::Open {
        scope: Scope::WindowGroup(Some(group)),
        query: None,
    });
    if let Some((group, request)) = search_in {
        let part = request.part().cloned();
        let app = super::find_group(tree, strip, group)
            .and_then(|window| window.tabs.iter().find(|tab| tab.id == window.active_tab))
            .and_then(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile));
        asked = Some(SearchRequest::Open {
            scope: Scope::InApp(AppScope { app, part }),
            query: request.query,
        });
    }
    asked
}

/// What a host adds to the desktop's search over everything, collected as it opens
/// ([`SearchHost::feed`]).
#[derive(Default)]
pub struct SearchHostFeed {
    /// The apps this host opens, beside those the desktop's tile source lists.
    pub applications: Vec<tilo_desktop_search::providers::AppEntry>,
    /// What providers publish through the host's action host, run by address there.
    pub published: Vec<tilo_desktop_search::providers::PublishedActions>,
    /// The host's own sources, each over what is published or a service answers.
    pub sources: Vec<std::sync::Arc<dyn tilo_desktop_search::Source>>,
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
pub(super) struct TypedAsk {
    /// The field the text was committed into, and its revision then.
    field: TypedField,
    text: String,
}

/// The identity of the search query field, for the desktop's life.
const QUERY_FIELD: u64 = 1;

/// The key the query field's revision is fingerprinting with: it never leaves the process, and only
/// says whether the field moved.
const TYPING_KEY: &[u8; 32] = b"desktop typing field revision 1!";

/// The open search's query field, as committed text may write into it. The palette draws it in the
/// desktop itself, so no text-input boundary of the windows the desk shows ever sees it.
fn typed_field(
    search: &tilo_desktop_search::LazyDesktopSearch,
) -> Result<TypedField, TypedRefusal> {
    let Some(field) = search.query_field(TYPING_KEY) else {
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
    /// the desktop's own tree. A search over the workspace and one over
    /// every workspace ask it; an app's own search and the applications
    /// alone ([`Self::applications`]) do not.
    fn feed(&self, workspace: &str, scope: &Scope) -> SearchHostFeed;
    /// The apps this host opens, each an id a launch accepts, whether or not a search is over a
    /// workspace: a window group's new tab and the empty desk's launchpad list the applications
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

/// The open search as the desk last showed it: its query and rows, and whether it is still filling
/// in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownSearch {
    pub query: String,
    pub rows: Vec<SearchRow>,
    /// A source has still to answer for the query, or the rows are not cut to the card yet, so the
    /// rows will change.
    pub searching: bool,
}

const CLOSE_WINDOW: &str = "desktop.close-window";
const MAXIMIZE_WINDOW: &str = "desktop.maximize-window";
/// Search command ids and banner action keys of the host's own actions.
const HOST_PREFIX: &str = "host:";
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
    #[serde(default)]
    pub subtitle: Option<String>,
    /// The action's own picture, by its Lucide name ([`ui::icons::Icon::name`]): every list of it
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
}

impl HostActionSource {
    fn label(self) -> String {
        crate::i18n::t(match self {
            Self::Identity => "host-action-source-identity",
            Self::Devices => "host-action-source-devices",
            Self::Workspaces | Self::Workspace => "host-action-source-workspace",
            Self::Session => "host-action-source-session",
        })
    }
}

impl HostAction {
    /// A banner that offers this action: choosing it performs the action.
    #[must_use]
    ///
    /// An offer is an ask: it stays until the person answers it or puts it off ("Not now", which
    /// keeps it in the notification centre), so nothing is decided by waiting.
    pub fn notice(
        &self,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> crate::notifications::Notification {
        let mut notice = crate::notifications::Notification::new(title, body);
        // The action first: it is what choosing the ask itself does, from the banner or later from
        // the centre.
        notice.actions = vec![
            (self.key(), self.title.clone()),
            (
                crate::notifications::NOT_NOW_ACTION.to_owned(),
                crate::i18n::t("notification-not-now"),
            ),
        ];
        notice.app_name = self.source.map(HostActionSource::label);
        notice.dismiss_after = crate::notifications::PERSISTENT_DISMISS;
        // What a host asks is on the person's behalf: who they are and which devices are theirs.
        notice.icon_kind = crate::notifications::NotificationIconKind::Permission;
        notice
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

pub fn is_host_offer(notification: &crate::notifications::Notification) -> bool {
    notification
        .actions
        .iter()
        .any(|(key, _)| host_choice(key).is_some())
}

/// Whether `notification` offers host action `id` .
pub(crate) fn offers(notification: &crate::notifications::Notification, id: &str) -> bool {
    notification
        .actions
        .iter()
        .any(|(key, _)| host_choice(key) == Some(id))
}

/// The host action a banner action key or search command id names.
pub(super) fn host_choice(key: &str) -> Option<&str> {
    key.strip_prefix(HOST_PREFIX)
}

impl DesktopHandle {
    /// Offer these host actions from now on, in place of earlier ones. Returns the ids no longer
    /// offered: a banner still asking for one of them asks for something already done, so the host
    /// withdraws it ([`crate::DesktopNotifications::withdraw_host_action`]).
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

    /// The host actions the person chose since the last call, by id.
    pub fn take_host_choices(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.borrow_mut().host_choices)
    }

    /// Open search, or close it when it is open. The host schedules a frame.
    pub fn toggle_search(&self) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Toggle);
    }

    /// Open search at `scope` , with `query` typed. The host schedules a frame.
    pub fn open_search(&self, scope: Scope, query: Option<String>) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Open { scope, query });
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
        true
    }

    /// Search `tab` 's app, the way an app in a process of its own asked
    /// (`ui::floating_surface::FloatingSurfaceClientMessage::OpenAppSearch`): inside a session
    /// there is one search, the shell's, and the app opens none of its own. A tab that shows no app
    /// searches nothing.
    pub fn search_app(&self, tab: TabId, request: ui::search::SearchRequest) {
        let mut shared = self.shared.borrow_mut();
        let Some(app) = shared
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|candidate| candidate.id == tab)
            .and_then(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile))
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

    /// The palette the desk searches with, shared with the host that draws it over the windows: the
    /// desk fills it with what its windows hold and carries out what is chosen there, while that
    /// host draws it, routes the person's input to it and closes it. [`Self::note_search_activity`]
    /// tells the desk what that host changed.
    pub fn search_palette(
        &self,
    ) -> std::rc::Rc<std::cell::RefCell<tilo_desktop_search::LazyDesktopSearch>> {
        std::rc::Rc::clone(&self.shared.borrow().search)
    }

    /// Where the palette stands when it was opened for one window: that window under its bar, over
    /// the rest of it, as the desk last laid it out; nothing over everything.
    pub fn search_area(&self) -> Option<Rect> {
        self.shared.borrow().search_area
    }

    /// Changes to what the palette shows or asks of the desk, counted: a host
    /// that changed the palette without asking the desk to (typing in it,
    /// opening it) bumps it, and the desk takes that up on its next pass.
    pub fn note_search_activity(&self) {
        let mut shared = self.shared.borrow_mut();
        shared.search_revision = shared.search_revision.wrapping_add(1);
    }

    /// The [`Self::note_search_activity`] count as of the last update pass.
    pub fn search_revision(&self) -> u64 {
        self.shared.borrow().search_revision
    }

    /// Whether the desk itself takes the keys the person types: its notification centre is open,
    /// and what they type goes into it rather than to the window behind. Its search does not: the
    /// palette stands over the windows, drawn by the host above them, which takes the keys while it
    /// is up.
    pub fn wants_keyboard(&self) -> bool {
        self.shared
            .borrow()
            .presentation
            .notifications
            .as_ref()
            .is_some_and(|notifications| notifications.center_open)
    }

    /// Search from now on also over what `host` adds, and hand it what is chosen that it carries
    /// out ([`SearchHost::performs`]).
    pub fn set_search_host(&self, host: std::rc::Rc<dyn SearchHost>) {
        self.shared.borrow_mut().search_host = Some(host);
    }

    /// The rows chosen in search since the last call that the host carries out, in order.
    pub fn take_search_choices(&self) -> Vec<Target> {
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

    /// Whether the open search is still filling in, as of the last update pass: a source has still
    /// to answer for the query, or the rows shown are not cut to the card yet, so the rows will
    /// change. What the desk publishes as a search still searching.
    pub fn search_searching(&self) -> bool {
        self.shared.borrow().shown_search.searching
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
    /// action asks for, on the next update pass.
    pub fn choose_search_row(&self, index: usize, argument: Option<String>) {
        self.shared.borrow_mut().search_asks.push((index, argument));
    }
}

/// The search chord `is_search_chord` answers to, as the keyboard spells it on `platform` : ⌘K on a
/// Mac, where Ctrl+K does not open search, and Ctrl K elsewhere.
pub fn search_chord_keys(platform: ui::platform::PlatformConventions) -> Vec<String> {
    let keycaps = ui::keycaps::active();
    if platform.platform() == ui::platform::Platform::MacOs {
        keycaps
            .on_family(ui::interaction::KeyboardFamily::Apple)
            .chord_keys(["Super"], "k")
    } else {
        keycaps.chord_keys(["Ctrl"], "k")
    }
}

/// The search chords: Super+K and Super+Space as in the native shell, and Ctrl+K where Ctrl is the
/// platform's command key.
pub fn is_search_chord(
    event: &WindowEvent,
    modifiers: ui::winit::event::Modifiers,
    platform: ui::platform::PlatformConventions,
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
        || (mods.control_key() && platform.platform() != ui::platform::Platform::MacOs);
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

/// Every open window as search lists it: called by what it shows and the app it shows it in
/// (`Groceries — Notes`), with the app for its glyph. Only windows called alike say more: where
/// each of them is, or which of them it is, counted left to right.
fn window_app(tree: &DesktopTree, group: GroupId) -> Option<String> {
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

fn windows(
    tree: &DesktopTree,
    holders: &std::collections::HashMap<TabId, ui::widget::TabHolder>,
) -> Vec<WindowEntry> {
    let mut out = Vec::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile);
                    let app_title = match &tab.tile {
                        TileContent::LocalApp { label, .. } => label.clone(),
                        _ => app.as_ref().map_or_else(String::new, |app| {
                            tilo_desktop_search::names::app_name(app)
                        }),
                    };
                    let place = holders
                        .get(&tab.id)
                        .map(|holder| holder.name.clone())
                        .into_iter()
                        .chain([workspace.name.clone(), strip.name.clone()])
                        .collect();
                    // A window bar that only says what the app is called names no place: the row is
                    // the app, so the bar's words for it are dropped rather than repeated as a
                    // title.
                    let named = app.as_deref().is_some_and(|app| {
                        tilo_desktop_search::names::is_apps_own_name(app, &tab.title)
                    });
                    let title = if named {
                        app_title.clone()
                    } else {
                        ui::window_name::named(&app_title, &tab.title)
                    };
                    out.push(WindowEntry {
                        tab: tab.id,
                        title,
                        app_title,
                        app,
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
            entry.place.push(crate::i18n::t_args(
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

/// The people in the active workspace, as search finds them, and its workspace.
fn people(
    tree: &DesktopTree,
) -> (
    String,
    Vec<tilo_desktop_search::providers::people::PersonEntry>,
) {
    let Some(workspace) = tree.active_workspace() else {
        return (String::new(), Vec::new());
    };
    let people = workspace
        .collaborators
        .iter()
        .map(
            |person| tilo_desktop_search::providers::people::PersonEntry {
                id: person.id.clone(),
                name: person.name.clone(),
                activity: person.in_app.clone(),
            },
        )
        .collect();
    (workspace.id().to_string(), people)
}

/// What search offers to do, in this order: the host's actions (a workspace's own, the session's),
/// then what the focused window allows (no close for a window someone else holds, `held` ), then
/// the other workspaces. Each row says whether it may stand there with nothing typed: closing a
/// window or moving to another workspace is something a person goes looking for, and typing still
/// finds every one of them.
fn commands(
    tree: &DesktopTree,
    host: &[HostAction],
    held: &dyn Fn(TabId) -> bool,
) -> Vec<CommandSpec> {
    let mut out: Vec<CommandSpec> = host
        .iter()
        .map(|action| CommandSpec {
            id: format!("{HOST_PREFIX}{}", action.id),
            title: action.title.clone(),
            subtitle: action.subtitle.clone(),
            icon: action.icon.as_deref().and_then(ui::icons::Icon::from_name),
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
                icon: Some(ui::icons::Icon::X),
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
                ui::icons::Icon::Minimize2
            } else {
                ui::icons::Icon::Maximize2
            }),
            unasked: false,
        });
    }
    for workspace in &tree.workspaces {
        if workspace.slot != tree.active {
            out.push(CommandSpec {
                id: format!(
                    "{}{}",
                    tilo_desktop_search::WORKSPACE_COMMAND_PREFIX,
                    workspace.slot.0
                ),
                title: format!("Switch to {}", workspace.name),
                subtitle: Some("Workspace".into()),
                icon: Some(ui::icons::Icon::LayoutGrid),
                unasked: false,
            });
        }
    }
    out
}

impl DesktopSurface {
    /// Open search at `scope` , with `query` typed: over what the desktop has open in the workspace
    /// in front ([`Scope::Workspace`]), over every workspace ([`Scope::Everywhere`]), over what an
    /// app's window holds ([`Scope::InApp`]), or over the applications a window group's new tab may
    /// show ([`Scope::WindowGroup`]).
    pub(super) fn open_search(&mut self, doc: &AutoCommit, scope: Scope, query: Option<String>) {
        // An app's search runs in that app's window; a window group's applications open into it.
        // The others stand over the desk.
        let window = match &scope {
            Scope::InApp(app) => self.app_window(app),
            Scope::WindowGroup(Some(group)) => {
                self.group_strip(*group).map(|strip| (strip, *group))
            }
            Scope::WindowGroup(None) | Scope::Workspace | Scope::Everywhere => None,
        };
        let (scope, window, data, usage) = match (scope, window) {
            (Scope::InApp(app), Some((strip, group))) => {
                let scope = Scope::InApp(app);
                if let Some(data) = self.window_search_data(doc, strip, group, &scope) {
                    (scope, window, data, None)
                } else {
                    // The window's app is gone: there is nothing of its own left to search.
                    let scope = Scope::Workspace;
                    let (data, usage) = self.desk_search_data(doc, &scope);
                    (scope, None, data, usage)
                }
            }
            // An app with no window open has nothing of its own to search: the search opens over
            // the workspace instead.
            (Scope::InApp(_), None) => {
                let scope = Scope::Workspace;
                let (data, usage) = self.desk_search_data(doc, &scope);
                (scope, None, data, usage)
            }
            (scope @ Scope::WindowGroup(_), _) => {
                let data = self.applications_search_data(&scope);
                (scope, window, data, None)
            }
            (scope @ (Scope::Workspace | Scope::Everywhere), _) => {
                let (data, usage) = self.desk_search_data(doc, &scope);
                (scope, None, data, usage)
            }
        };
        // Choosing a row of an app's search runs in that app's window; the others find their window
        // by the resource the row names.
        let owners = if matches!(scope, Scope::Workspace | Scope::Everywhere) {
            self.search_owners(doc)
        } else {
            std::collections::HashMap::new()
        };
        let refusal = self.shared.borrow_mut().search_refusal.take();
        let mut search = self.search.borrow_mut();
        search.open(data);
        if let Some((target, why)) = refusal {
            search.refuse(target, why);
        }
        if let Some(usage) = usage {
            search.set_usage(usage);
        }
        search.set_platform_conventions(self.platform);
        drop(search);
        // What a chosen application opens in: a tab of the window group the new-tab control belongs
        // to, else a window of its own.
        self.new_tab_into = match &scope {
            Scope::WindowGroup(_) => window,
            _ => None,
        };
        // Where the card stands and where an action row runs.
        self.search_window = match &scope {
            Scope::InApp(_) => window,
            _ => None,
        };
        self.search_owners = owners;
        if let Some(query) = query {
            self.search.borrow_mut().set_query(&query);
        }
        self.structure_dirty = true;
        self.note_search_activity();
    }

    fn close_search(&mut self) {
        self.search.borrow_mut().close();
        self.structure_dirty = true;
        self.note_search_activity();
    }

    /// Search opened or closed from the desk: what the host above the windows draws and asks about
    /// the palette changed, as it does when the person types there
    /// ([`DesktopHandle::note_search_activity`]).
    fn note_search_activity(&self) {
        DesktopHandle {
            shared: Rc::clone(&self.shared),
        }
        .note_search_activity();
    }

    /// The window an app is open in: its window in front, or the window in front when the scope
    /// names no app ([`AppScope::front`]).
    fn app_window(&self, app: &AppScope) -> Option<(StripId, GroupId)> {
        let shared = self.shared.borrow();
        match &app.app {
            Some(app) => {
                navigation::window_of_app(&shared.tree, app).map(|ids| (ids.strip, ids.group))
            }
            None => navigation::active_route(&shared.tree).map(|ids| (ids.strip, ids.group)),
        }
    }

    /// The window group in front, wherever the person is looking: where a row chosen as "a tab
    /// here" opens.
    fn window_in_front(&self) -> Option<(StripId, GroupId)> {
        let shared = self.shared.borrow();
        navigation::active_route(&shared.tree).map(|ids| (ids.strip, ids.group))
    }

    /// The strip showing the window group `group` , wherever it is.
    fn group_strip(&self, group: GroupId) -> Option<StripId> {
        let shared = self.shared.borrow();
        shared
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|strip| strip.window_groups.iter().any(|window| window.id == group))
            .map(|strip| strip.id)
    }

    /// What window `group` shows: its shown tab's resources, what its search providers find and its
    /// actions ([`tilo_desktop_search::window::WindowSearch`]), as the in-app search its search
    /// control and the app's own search button offer, narrowed to the part of the app `scope`
    /// names. `None` when the window's app is no longer on screen.
    fn window_search_data(
        &self,
        doc: &AutoCommit,
        strip: StripId,
        group: GroupId,
        scope: &Scope,
    ) -> Option<SearchData> {
        let host = self
            .rows
            .get(&strip)?
            .groups
            .iter()
            .find(|window| window.id == group)?
            .active_host()?;
        // The field says what it searches: the part of the app asked for,
        // else the app itself.
        let part = match scope {
            Scope::InApp(app) => app.part.clone(),
            _ => None,
        };
        let searched = part
            .as_ref()
            .map(|part| part.label.clone())
            .or_else(|| window_app(&self.shared.borrow().tree, group))
            .filter(|name| !name.is_empty());
        let published = host.window_search(doc);
        Some(SearchData {
            resources: published.resources,
            providers: published.providers,
            scope: scope.clone(),
            placeholder: searched.map(|name| {
                crate::i18n::t_args("search-window-placeholder", &[("searched", name.into())])
            }),
            ..SearchData::default()
        })
    }

    /// The applications alone, each listed: what a window group's new tab
    /// may show, the empty desk's launchpad.
    ///
    /// Its source is the desktop's tile source and the host's own apps
    /// ([`SearchHost::applications`]): a session's tile source lists no app,
    /// because the session is what starts them, so without the host's list
    /// the applications scope would list nothing and the person could not
    /// open anything from it.
    fn applications_search_data(&self, scope: &Scope) -> SearchData {
        let mut applications = self.tiles.search_applications();
        if let Some(host) = self.shared.borrow().search_host.clone() {
            applications.extend(host.applications());
        }
        SearchData {
            applications,
            scope: scope.clone(),
            ..SearchData::default()
        }
    }

    /// What the desktop has open now and what its host adds: over the workspace in front, or over
    /// every workspace.
    fn desk_search_data(
        &self,
        doc: &AutoCommit,
        scope: &Scope,
    ) -> (SearchData, Option<tilo_desktop_search::Usage>) {
        let (windows, commands, (workspace, people), host) = {
            let shared = self.shared.borrow();
            (
                windows(&shared.tree, &shared.holders),
                commands(&shared.tree, &shared.host_actions, &|tab| {
                    shared.holders.contains_key(&tab)
                }),
                people(&shared.tree),
                shared.search_host.clone(),
            )
        };
        let mut feed = host
            .map(|host| host.feed(&workspace, scope))
            .unwrap_or_default();
        let usage = feed.usage.take();
        let mut applications = self.tiles.search_applications();
        applications.extend(feed.applications);
        let mut sources = feed.sources;
        sources.push(std::sync::Arc::new(
            tilo_desktop_search::providers::people::People::new(workspace, people),
        ));
        let mut data = SearchData {
            applications,
            windows,
            commands,
            scope: scope.clone(),
            published: feed.published,
            sources,
            preferences: feed.preferences,
            now: feed.now,
            ..SearchData::default()
        };
        for row in self.rows.values() {
            data.resources
                .extend(ui::widget::published_resources(row, doc));
            data.providers
                .extend(ui::widget::published_search_providers(row));
        }
        (data, usage)
    }

    /// The window each of the desk's resources is published in, for a row chosen in the search over
    /// everything: the app's window comes forward and runs it.
    fn search_owners(
        &self,
        doc: &AutoCommit,
    ) -> std::collections::HashMap<String, (StripId, GroupId)> {
        let mut owners = std::collections::HashMap::new();
        for (strip, row) in &self.rows {
            for window in &row.groups {
                let Some(host) = window.active_host() else {
                    continue;
                };
                let mut open = ui::widget::published_resources(host, doc);
                while let Some(resource) = open.pop() {
                    owners
                        .entry(resource.id.as_str().to_owned())
                        .or_insert((*strip, window.id));
                    open.extend(resource.children);
                }
            }
        }
        owners
    }

    /// Take a host's search request, follow the palette, and perform what was chosen. Runs every
    /// update pass, before queued commands apply.
    pub(super) fn step_search(&mut self, doc: &AutoCommit) {
        // Search's code may have arrived since the last pass (the browser
        // fetches it on first use): what asked for it meanwhile opens now,
        // rather than the person having to ask again.
        if self.search.borrow_mut().adopt() {
            self.structure_dirty = true;
        }
        let opened = self.search.borrow().is_open();
        let request = self.shared.borrow_mut().search_request.take();
        match request {
            Some(SearchRequest::Toggle) if opened => self.close_search(),
            Some(SearchRequest::Toggle) => self.open_search(doc, Scope::Workspace, None),
            Some(SearchRequest::Open { scope, query }) => self.open_search(doc, scope, query),
            None => {}
        }
        let choices = std::mem::take(&mut self.shared.borrow_mut().search_asks);
        for (index, argument) in choices {
            let mut search = self.search.borrow_mut();
            if !search.is_open() {
                break;
            }
            search.choose(index, argument);
            drop(search);
            self.structure_dirty = true;
        }
        let ask = self.shared.borrow_mut().typed_ask.take();
        if let Some(ask) = ask {
            self.commit_typed(&ask);
        }
        self.follow_search(doc);
        self.publish_search_rows();
    }

    /// Write text committed into the desktop's own field, if the person is still typing into the
    /// field it was meant for.
    fn commit_typed(&mut self, ask: &TypedAsk) {
        if !self.search.borrow().is_open() {
            return;
        }
        // The field may have taken an edit or moved its caret since the text was committed: then
        // the text is not the person's to review any more and goes nowhere, as a dictation into a
        // field that moved.
        if typed_field(&self.search.borrow()) != Ok(ask.field) {
            return;
        }
        if self.search.borrow_mut().commit_query_text(&ask.text) {
            self.structure_dirty = true;
        }
    }

    /// Keep what the open search shows where the desktop publishes it.
    fn publish_search_rows(&self) {
        let search = self.search.borrow();
        let open = search.is_open();
        let rows: Vec<SearchRow> = if open {
            search
                .results()
                .iter()
                .map(|hit| SearchRow {
                    title: hit.title.clone(),
                    // The reason the host could not carry the row out stands on it, as the palette
                    // draws it, rather than the row reading as if nothing happened.
                    subtitle: search
                        .refusal_for(&hit.target)
                        .map(str::to_owned)
                        .or_else(|| hit.subtitle.clone()),
                    kind: hit.kind.label().to_owned(),
                    facts: hit.facts.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let query = if open {
            search.query().to_owned()
        } else {
            String::new()
        };
        let typed = if open {
            typed_field(&search)
        } else {
            Err(TypedRefusal::NoField)
        };
        let shown = ShownSearch {
            query,
            rows,
            searching: open && !search.settled(),
        };
        let mut shared = self.shared.borrow_mut();
        let changed = shared.shown_search != shown || shared.typed_field != typed;
        shared.shown_search = shown;
        shared.typed_field = typed;
        if changed {
            // What the host above the windows draws of search, and what it offers assistive
            // technology, follows what the desk published.
            shared.search_revision = shared.search_revision.wrapping_add(1);
        }
    }

    /// After the palette handled input (an event, or an assistive technology action), take its
    /// outcome.
    fn follow_search(&mut self, doc: &AutoCommit) {
        // Backspace at the start of the field, or the token's own button,
        // takes the scope away: the same search, over every workspace.
        let mut search = self.search.borrow_mut();
        if search.is_open() && search.take_scope_widened() {
            let query = search.query().to_owned();
            drop(search);
            self.open_search(doc, Scope::Everywhere, Some(query));
            search = self.search.borrow_mut();
        }
        if search.is_open() {
            let closed = search.observe();
            let refreshed = search.refresh();
            self.structure_dirty |= closed || refreshed;
        }
        let choice = search.take_choice();
        let usage = if choice.is_some() {
            search.usage().cloned()
        } else {
            None
        };
        drop(search);
        if let Some((usage, host)) = usage.zip(self.shared.borrow().search_host.clone()) {
            host.chose(&usage);
        }
        if matches!(choice, Some(Target::Launch { .. })) {
            // The person opened something from search themselves: what the desktop opens on its own
            // (a workspace's home app) teaches nothing.
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedWindow,
            );
        }
        match (choice, self.new_tab_into.take()) {
            // A tab in the window the person asked for: the one the search was opened from (its
            // new-tab control), else the one in front.
            (Some(Target::Launch { app, place }), into) => {
                let group = match place {
                    LaunchPlace::TabHere => into.or_else(|| self.window_in_front()),
                    LaunchPlace::Focus | LaunchPlace::Beside => into,
                };
                match group {
                    Some((strip, group)) => {
                        self.open_tab_in_group(strip, group, &app);
                    }
                    // A window of its own opens where the session starts windows, and one that
                    // names no place opens in the window its app has.
                    None => self.perform(Target::Launch { app, place }),
                }
            }
            (Some(Target::Action(action)), _) => self.run_action(&action),
            (Some(target), _) => self.perform(target),
            (None, into) => self.new_tab_into = into,
        }
        let open = self.search.borrow().is_open();
        if open {
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedSearch,
            );
        } else {
            // The aim is for one choice; search opened otherwise opens applications as their own
            // windows again.
            self.new_tab_into = None;
            self.search_window = None;
            self.search_owners.clear();
        }
        let mut shared = self.shared.borrow_mut();
        if shared.search_open != open {
            shared.search_open = open;
            shared.search_revision = shared.search_revision.wrapping_add(1);
        }
    }

    /// Focus window `group` , as choosing it in search does.
    fn bring_forward(&self, strip: StripId, group: GroupId) {
        let tab = self
            .rows
            .get(&strip)
            .and_then(|row| row.groups.iter().find(|candidate| candidate.id == group))
            .map(ui::widget::WindowGroup::active_tab_id);
        if let Some(tab) = tab {
            self.perform(Target::Window { tab, app: None });
        }
    }

    /// Run a chosen action row: by its address, in the window the search was opened in or the
    /// window of the app publishing it, as that app's own control would; the desktop's own door
    /// runs a row of its own.
    fn run_action(&mut self, action: &tilo_desktop_search::ActionTarget) {
        let mut arguments = serde_json::Map::new();
        if let Some((param, value)) = action.preset.clone() {
            arguments.insert(param, serde_json::Value::String(value));
        }
        let invocation = ui::tilo_actions::ControlInvocation {
            address: action.address(),
            arguments,
        };
        match self.search_window {
            Some((strip, group)) => self.run_in_window(strip, group, invocation),
            None => {
                match self
                    .search_owners
                    .get(invocation.address.resource.as_str())
                    .copied()
                {
                    // Chosen in search over everything: the app's window comes forward and runs it.
                    Some((strip, group)) => {
                        self.bring_forward(strip, group);
                        self.run_in_window(strip, group, invocation);
                    }
                    // Not an app's: the desktop's own door runs it.
                    None => self.unrouted_control = Some(invocation),
                }
            }
        }
    }

    /// Run `invocation` , chosen in the search over window `group` , inside the app that window
    /// shows, as its own control would: the desktop's door never reaches into an app's actions by
    /// address.
    fn run_in_window(
        &mut self,
        strip: StripId,
        group: GroupId,
        invocation: ui::tilo_actions::ControlInvocation,
    ) {
        let host = self
            .rows
            .get_mut(&strip)
            .and_then(|row| {
                row.groups
                    .iter_mut()
                    .find(|candidate| candidate.id == group)
            })
            .and_then(|window| window.active_host_mut());
        match host {
            Some(host) => {
                if let Err(invocation) = host.queue_control(invocation) {
                    self.unrouted_control = Some(invocation);
                }
            }
            None => self.unrouted_control = Some(invocation),
        }
        self.structure_dirty = true;
    }

    /// Queue what a chosen row does, as the matching navigation command,
    /// or hand it to the host where the host carries it out.
    fn perform(&self, target: Target) {
        let host = self.shared.borrow().search_host.clone();
        if host.is_some_and(|host| host.performs(&target)) {
            // Where search stood when the row was chosen, so an answer to what the host could not
            // carry out stands on that row again.
            let (scope, query) = {
                let search = self.search.borrow();
                (search.scope(), search.query().to_owned())
            };
            let mut shared = self.shared.borrow_mut();
            shared.search_choice_context = Some((scope, query, target.clone()));
            shared.search_choices.push(target);
            return;
        }
        let mut shared = self.shared.borrow_mut();
        let admitted = match target {
            // An app already open is focused, not opened twice: a new window is asked for through a
            // window's new tab instead.
            Target::Launch { app, .. } => Some(
                navigation::window_of_app(&shared.tree, &app)
                    .map_or(Admitted::Launch(app, None), Admitted::Select),
            ),
            Target::Window { tab, .. } => route_of(&shared.tree, tab).map(Admitted::Select),
            Target::Command { id } => {
                let active = navigation::active_route(&shared.tree);
                if let Some(choice) = host_choice(&id) {
                    let choice = choice.to_owned();
                    shared.host_choices.push(choice);
                    None
                } else if id == tilo_desktop_search::ALL_APPS_COMMAND {
                    // Every app by its name: the applications' own search,
                    // as the empty desk's launchpad opens it.
                    shared.search_request = Some(SearchRequest::Open {
                        scope: Scope::WindowGroup(None),
                        query: None,
                    });
                    None
                } else if id == CLOSE_WINDOW {
                    match active {
                        Some(ids) if shared.holders.contains_key(&ids.tab) => {
                            let refusal = holder_refusal(&shared.holders, ids.tab);
                            shared.acknowledger.say(refusal);
                            None
                        }
                        active => active.map(Admitted::Close),
                    }
                } else if id == MAXIMIZE_WINDOW {
                    active.and_then(|ids| {
                        let group = navigation::locate(&shared.tree, ids)?;
                        Some(Admitted::Maximize(ids, !group.maximized))
                    })
                } else {
                    id.strip_prefix(tilo_desktop_search::WORKSPACE_COMMAND_PREFIX)
                        .and_then(|id| id.parse::<u64>().ok())
                        .map(tilo_desktop_model::WorkspaceSlot)
                        .filter(|id| shared.tree.workspaces.iter().any(|w| w.slot == *id))
                        .map(Admitted::Workspace)
                }
            }
            Target::Copy { text } => {
                ui::text_selection::clipboard_copy(&text);
                shared
                    .acknowledger
                    .say(crate::i18n::t("desktop-search-copied"));
                None
            }
            // Where the host opens addresses. In a web browser that is a new tab, which the browser
            // allows only within the choosing click or key press, so it opens now and a blocked one
            // is said; a native handler may take its time, so it opens off this thread.
            Target::Open { uri, fallback } => {
                // A browser tab takes only web addresses: a scheme an app or the person's browser
                // would take opens as its fallback.
                #[cfg(target_arch = "wasm32")]
                if let Err(error) =
                    ui::widget::try_open_external_url(fallback.as_deref().unwrap_or(&uri))
                {
                    shared.acknowledger.say(error.to_string());
                }
                #[cfg(not(target_arch = "wasm32"))]
                match fallback {
                    Some(fallback) => {
                        // The opener is this thread's host's, on that thread too.
                        let host = ui::host::current();
                        std::thread::spawn(move || {
                            ui::host::scoped(&host, || {
                                if ui::widget::try_open_external_url(&uri).is_err() {
                                    ui::widget::open_external_url(&fallback);
                                }
                            });
                        });
                    }
                    None => ui::widget::open_external_url(&uri),
                }
                None
            }
            // An app's action row is run by its own window ([`Self::run_action`]), and one
            // published through the host is handed to it ([`SearchHost::performs`]). Without a host
            // running them, a desktop lists no host-published actions or jobs.
            Target::Action(_)
            | Target::Invoke(_)
            | Target::Cancel { .. }
            | Target::Prefer { .. } => None,
        };
        if let Some(admitted) = admitted {
            shared.queue.push(admitted);
        }
    }

    /// The window a search over one window (`search_window`) stands over,
    /// under its bar, which the palette keeps clear.
    pub(super) fn searched_window_rect(&self) -> Option<Rect> {
        let (strip, group) = self.search_window?;
        self.rows
            .get(&strip)?
            .groups
            .iter()
            .find(|candidate| candidate.id == group)
            .map(|group| {
                let window = group.get_rect();
                Rect::new(
                    window.x0,
                    group.bar_rect().y1.clamp(window.y0, window.y1),
                    window.x1,
                    window.y1,
                )
            })
            .filter(|rect| rect.area() > 0.0)
    }
}

/// The ids of the commands search offers over `tree` , with the windows `held` by someone else.
#[cfg(test)]
pub(super) fn offered_commands(tree: &DesktopTree, held: &dyn Fn(TabId) -> bool) -> Vec<String> {
    commands(tree, &[], held)
        .into_iter()
        .map(|spec| spec.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::winit::event::{KeyEvent, Modifiers, RawKeyEvent};
    use ui::winit::keyboard::{KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

    fn press(key: Key) -> WindowEvent {
        let event = KeyEvent::synthetic(
            RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                state: ElementState::Pressed,
            },
            key.clone(),
            None,
            KeyLocation::Standard,
            false,
            None,
            key,
        );
        ui::synthetic_input::keyboard_input(event)
    }

    fn mods(state: ModifiersState) -> Modifiers {
        Modifiers::from(state)
    }

    #[test]
    fn a_host_actions_notice_names_what_it_is_about() {
        let pair: HostAction = serde_json::from_str(
            r#"{"id":"pair-device","title":"Pair a new device","source":"devices"}"#,
        )
        .unwrap();
        let notice = pair.notice("Pair “Phone”?", "");
        assert_eq!(
            notice.sender_name(),
            crate::i18n::t("host-action-source-devices")
        );
        let plain: HostAction = serde_json::from_str(r#"{"id":"x","title":"X"}"#).unwrap();
        assert_eq!(
            plain.notice("X", "").sender_name(),
            crate::i18n::t("notification-group-system"),
            "a notice from no named part of the desktop still says who it is from"
        );
    }

    #[test]
    fn the_chord_shown_is_the_one_that_opens_search() {
        let linux = ui::platform::PlatformConventions::new(ui::platform::Platform::Linux);
        let mac = ui::platform::PlatformConventions::new(ui::platform::Platform::MacOs);
        let k = press(Key::Character("k".into()));
        assert_eq!(search_chord_keys(mac), ["⌘", "K"]);
        assert!(is_search_chord(&k, mods(ModifiersState::SUPER), mac));
        let shown = search_chord_keys(linux);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[1], "K");
        assert_eq!(shown[0], ui::keycaps::active().chord_keys(["Ctrl"], "k")[0]);
        assert!(is_search_chord(&k, mods(ModifiersState::CONTROL), linux));
    }

    #[test]
    fn search_chords_match_the_native_shell() {
        let linux = ui::platform::PlatformConventions::new(ui::platform::Platform::Linux);
        let mac = ui::platform::PlatformConventions::new(ui::platform::Platform::MacOs);
        let k = press(Key::Character("k".into()));
        let space = press(Key::Named(NamedKey::Space));
        assert!(is_search_chord(&k, mods(ModifiersState::SUPER), linux));
        assert!(is_search_chord(&k, mods(ModifiersState::SUPER), mac));
        assert!(is_search_chord(&k, mods(ModifiersState::CONTROL), linux));
        assert!(!is_search_chord(&k, mods(ModifiersState::CONTROL), mac));
        assert!(is_search_chord(&space, mods(ModifiersState::SUPER), linux));
        assert!(!is_search_chord(
            &space,
            mods(ModifiersState::CONTROL),
            linux
        ));
        assert!(!is_search_chord(
            &k,
            mods(ModifiersState::SUPER | ModifiersState::SHIFT),
            linux
        ));
        assert!(!is_search_chord(&k, mods(ModifiersState::empty()), linux));
    }
}
