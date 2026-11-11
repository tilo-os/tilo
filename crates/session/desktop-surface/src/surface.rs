//! The desktop as a widget: the active workspace's active strip presented as the toolkit's
//! [`GroupRow`] of [`WindowGroup`]s, with the model as the source of truth.
//! Local changes flow both ways without diffing. Gestures inside the row (tab drags, splits,
//! closes, activation, resize, maximize, the new-tab control) are harvested into the tree on the
//! next update pass, exactly as the shell's overlay host harvests them. Navigation commands are
//! admitted against the tree when dispatched, then applied to the tree and to the live widgets in
//! the same update pass. Rows are built from the tree the first time their strip is shown and kept
//! alive afterwards, so a tab's application survives switching strips or workspaces.
//! A tree changed elsewhere (a peer, a restored copy) replaces the model through
//! [`DesktopHandle::replace_tree`]; the next update pass rebuilds the rows from it by tab id,
//! moving each surviving tab's live content into its new place and closing the applications of tabs
//! that are gone.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use shown::ShownTabs;

use tilo_desktop_model::{
    CanvasPlace, DesktopTree, GroupId, GroupShape, IdMinter, StripId, Tab, TabId, TileContent,
    WindowWidth, Workspace, WorkspaceSlot,
};
use ui::automerge::ObjId;
use ui::collaboration::PresencePayload;
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceKey;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::time::{Duration, Instant};
use ui::vello::Scene;
use ui::widget::{
    FLOAT_CARD_ROOM, FLOAT_SIZE, GroupRow, OutputSpan, STRIP_EDGE_GUTTER_PX, TaffyTreeExt as _,
    TileHost, Widget, WindowGroup,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::{TextRenderer, Theme};

use crate::navigation::{
    self, ActivityView, Admitted, Application, ChromeEffect, DesktopPan, HistoryDirection,
    HistoryState, NotificationsState, Presentation, RouteIds, ScrollState, Snapshot, Verb,
};
use crate::session::{ActivityAsk, ActivityReads, ActivityRequests};

use paging::Sideways;
use strip::{build_row, build_row_lazily, strip_style};

mod arrivals;
mod artwork;
#[cfg(test)]
mod close_button_tests;
#[cfg(test)]
mod close_motion_tests;
mod commands;
mod displays;
mod floating;
mod floats;
mod history;
mod hung;
mod launch;
mod lookup;
use lookup::{find_group, find_tab, observe_ids};
mod live_shares;
mod liveness;
mod notices;
#[cfg(test)]
mod open_motion_tests;
mod paging;
#[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
pub mod peer_view;
mod personal_ask;
mod search;
mod seed;
mod shared;
mod shown;
pub mod slide;
#[cfg(test)]
mod slide_tests;
mod splice;
mod strip;
mod tab_beside;
#[cfg(test)]
mod tab_beside_tests;
mod tile_content;
mod window_bars;
pub mod window_menu;
mod window_move;

pub use history::{ActivityModel, ActivityPaneCode, HistorySource, OpenActivity, open_activity};
pub use liveness::LivenessChoice;
pub use notices::{AppInbox, AppNotifier, DesktopNotifications, NotificationOutcome};
pub use personal_ask::PersonalAsk;
pub use search::{
    HostAction, HostActionSource, PALETTE_EDGE, SearchHost, SearchHostFeed, SearchRow, TypedField,
    TypedInsertion, TypedRefusal, host_action_key, is_escape, is_host_offer, is_search_chord,
    search_chord_keys,
};
pub use seed::{seed_tree, seed_tree_in};
pub use tile_content::tile_app_kind;
use tile_content::{build_tile, descriptor, placeholder};
pub use window_menu::Row as WindowMenuRow;

/// The caret or selection a surface publishes as its presence: the keyboard-focused widget's when
/// it is one of the focused window's (or application's) editors `window` , else that window's first
/// editor caret. Keyboard focus can stay behind in another window, an action can move a caret
/// without focus, and a hidden mount never holds keyboard focus.
pub fn published_selection(
    keyboard: Option<(PresenceKey, PresencePayload)>,
    window: &[(PresenceKey, PresencePayload)],
) -> Option<(PresenceKey, PresencePayload)> {
    match keyboard {
        Some(focused) if window.is_empty() || window.iter().any(|(key, _)| *key == focused.0) => {
            Some(focused)
        }
        _ => window.first().cloned(),
    }
}

/// The app that shows a workspace's history.
const ACTIVITY_APP: &str = tilo_actions::remote::workspace_activity::APP_ID;

/// Where a host's tab contents come from.
pub trait TileSource {
    /// The content for `tab` in `workspace` , built the first time the tab's strip is shown.
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost;

    /// A launch command named `app` : the tab to open under `id` . `None` opens nothing.
    fn launch(&mut self, _workspace: &Workspace, _id: TabId, _app: &str) -> Option<Tab> {
        None
    }

    /// The person reopened `tab` where they closed it, and its window is gone: the tab holds the
    /// app that opens it again, and waits for that app's window to take it. What the source needs
    /// to ask for the app.
    fn reopened(&mut self, _workspace: &Workspace, _tab: &Tab) {}

    /// The applications desktop search offers to open, each an id [`Self::launch`] accepts,
    /// followed by those this desktop cannot run (`unavailable` set), which search lists with their
    /// reason.
    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        Vec::new()
    }

    /// The tab already showing what the launch `app` opens (a record launch's record,
    /// [`tilo_desktop_search::record_launch`]): it comes forward in place of a second window on the same
    /// record.
    fn showing(&self, _app: &str) -> Option<TabId> {
        None
    }

    /// The record `tab`'s window shows and the kind of resource it keeps,
    /// for the windows a person may share ([`ui::window_share`]).
    fn window_record(&self, _tab: TabId) -> Option<(String, String)> {
        None
    }

    /// The workspace the windows are in, which each window's app is told: apps sharing one process
    /// have none of their own.
    fn workspace_id(&self) -> Option<String> {
        None
    }

    /// The person carried `tab` to the workspace at `to` , and a window of an app this device
    /// started fills it. Such a tab does not cross by itself: the app runs in the workspace that
    /// started it, with that workspace's grants and records, so the session starts it again where
    /// the person put it. Recording the move is what hands it over
    /// ([`tilo_desktop_policy::DesktopPolicy::take_carried`]).
    /// Whether the session took it: false keeps the move the desk's own, and nothing else about it
    /// changes.
    fn carry(&self, _tab: TabId, _to: WorkspaceSlot) -> bool {
        false
    }

    /// Whether this desktop draws the views windows embed of other apps' resources
    /// ([`Self::embed`]).
    fn draws_embeds(&self) -> bool {
        false
    }

    /// `view` 's own app on this desktop's copy of its record, to be drawn scaled into the window
    /// that embeds it: the app, or why it cannot show. Never creates the record.
    fn embed(&mut self, _view: &ui::window_share::ResourceView) -> Result<TileHost, String> {
        Err("This desktop can't show it here".into())
    }
}

struct Shared {
    tree: DesktopTree,
    /// Whoever holds each window someone else opened, by tab ([`DesktopHandle::set_holders`]).
    holders: HashMap<TabId, ui::widget::TabHolder>,
    /// The agent run that opened each window an agent opened, by tab
    /// ([`DesktopHandle::set_opened_by`]).
    opened_by: std::collections::BTreeMap<TabId, tilo_desktop_model::ownership::OpenedBy>,
    /// The windows the sharing app lists for its picker, as last published
    /// ([`DesktopSurface::offer_window_shares`]): a window's menu offers the share row exactly for
    /// these.
    window_offers: Vec<ui::window_share::OpenWindow>,
    /// The windows waiting for the person ([`DesktopHandle::ask_attention`]),
    /// until each has their focus.
    asking: std::collections::BTreeSet<TabId>,
    /// The bars the person asked a menu of (a secondary press), and where they pressed, waiting for
    /// the host.
    menus: Vec<(TabId, Point)>,
    /// Its person opens windows and tabs here ([`DesktopHandle::set_opens`]): the windows offer a
    /// new tab.
    opens: bool,
    /// Bumped on every tree change, so a host republishes navigation only when something moved.
    revision: u64,
    queue: Vec<Admitted>,
    /// What the Activity app was last asked to show (a view, and whose doings when one person's),
    /// until the activity model is up to offer it.
    activity_focus: Option<(ActivityView, crate::activity::ActivityDeepLink)>,
    minter: IdMinter,
    /// Logical width the strip lays out against.
    viewport_width: f64,
    /// How much of the desk's top edge the desktop bar overlays now
    /// ([`DesktopHandle::set_top_overlay_inset`]): every window moves down by it, whole, so its own
    /// bar stands below the bar shown over it rather than under it.
    top_overlay: f64,
    /// The tree was replaced from outside; rows must be reconciled.
    replaced: bool,
    /// Where each shown window stands and the bounds of the tab it shows, as last laid out.
    shown_tabs: ShownTabs,
    /// Each window floating over the desk, and what it is drawn whole over the windows under it —
    /// its bar, its body and the shadow that lifts it — as last drawn.
    float_covers: Vec<(TabId, Rect)>,
    /// The shown strip, the area it is drawn in and how far it is panned, as last laid out.
    shown_strip: Option<(StripId, Rect, f64)>,
    /// Where the shown strip's controls are for control hints, as last drawn.
    hint_places: ui::widget::HintPlaces,
    /// Carets and selections of the focused window's editors, as last laid out
    /// ([`ui::presence::capability::subtree_presence`]).
    focused_presence: Vec<(PresenceKey, PresencePayload)>,
    /// Tabs whose content the host asked to build again.
    rebuild: Vec<TabId>,
    search_request: Option<search::SearchRequest>,
    search_open: bool,
    /// The search palette itself, shared with the host that draws it over the windows
    /// (`DesktopHandle::search_palette`): the desk feeds it what its windows hold and carries out
    /// what is chosen there, while the host above the windows draws it and routes the person's
    /// input to it.
    search: Rc<RefCell<tilo_desktop_search::LazyDesktopSearch>>,
    /// Bumped whenever what the palette shows or what opening it asks of the desk changes, so the
    /// desk takes up what the host above it did and the host above it draws what the desk
    /// published.
    search_revision: u64,
    /// Where search stands when it was opened for one window: under that window's bar, over the
    /// rest of it, as the desk last laid it out.
    search_area: Option<Rect>,
    /// What the host offers to do for the person, and what they chose.
    host_actions: Vec<search::HostAction>,
    host_choices: Vec<String>,
    /// What the host adds to search and carries out of it ([`DesktopHandle::set_search_host`]).
    search_host: Option<std::rc::Rc<dyn search::SearchHost>>,
    /// Rows chosen in search that the host carries out, in order.
    search_choices: Vec<tilo_desktop_search::Target>,
    /// The search the host last carried a row out of: where it stood and what was typed, so a row
    /// it could not carry out is asked again with the answer on that row.
    search_choice_context: Option<(ui::search::Scope, String, tilo_desktop_search::Target)>,
    /// The reason a row the host carried out did not happen, for the search opened again over it to
    /// say on that row.
    search_refusal: Option<(tilo_desktop_search::Target, String)>,
    /// The rows a caller of the published search chose, with the argument each one's action asks
    /// for, taken on the next update pass.
    search_asks: Vec<(usize, Option<String>)>,
    /// The open search, as last shown: its query and rows, and whether it is still filling in.
    shown_search: search::ShownSearch,
    /// The field the person types into in the desktop itself, as last
    /// shown, and the text committed into it from outside the keyboard,
    /// taken on the next update pass.
    typed_field: Result<search::TypedField, search::TypedRefusal>,
    typed_ask: Option<search::TypedAsk>,
    /// The ask about the person themselves the bar shows from their avatar.
    personal_ask: Option<personal_ask::PersonalAsk>,
    personal_ask_revision: u64,
    /// Windows someone else opened lately, for the edge hint.
    arrivals: Vec<arrivals::Arrival>,
    /// Pan, scroll, history and activity state, as last presented.
    presentation: Presentation,
    /// Bumped whenever `presentation` changes. Unlike `revision` it never asks a host to save or
    /// share the tree.
    presented: u64,
    /// The artwork each window brought of its own, by the compositor's id for it
    /// ([`DesktopHandle::set_window_artwork`]).
    artwork: std::collections::BTreeMap<u64, ui::icons::AppImage>,
    /// The apps whose windows keep what they show to themselves
    /// ([`DesktopHandle::set_withheld_apps`]).
    withheld: std::collections::BTreeSet<String>,
    /// The windows whose app stopped answering ([`DesktopHandle::set_unresponsive_windows`]), by
    /// the compositor's id for them.
    unresponsive: std::collections::BTreeSet<u64>,
    /// Bumped whenever `unresponsive` changes, so a reading made from it is published again.
    unresponsive_revision: u64,
    /// What the person chose on a window that stopped answering, for the host to carry out
    /// ([`DesktopHandle::take_liveness_choices`]).
    liveness: Vec<(TabId, LivenessChoice)>,
    /// Where the host confirms in passing what left nothing to see.
    acknowledger: ui::widget::acknowledgement::Acknowledger,
    /// The windows other devices share with this one, each decoded into its tile as it arrives.
    #[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
    peers: Rc<RefCell<peer_view::PeerSurfaces>>,
    /// What the session's apps asked of their windows' bars since the last pass, and what the
    /// person did there, for the apps' connections ([`DesktopHandle::declare_bar`] and its
    /// siblings).
    bars: window_bars::Bars,
    /// Sideways wheel travel the apps of these windows left to the desk,
    /// by tab, taken on the next pass ([`DesktopHandle::sideways_wheel`]).
    sideways: Vec<(TabId, f64)>,
    /// What wakes the host holding this desk for a change queued on its handle
    /// ([`DesktopHandle::set_wake`]).
    wake: crate::wake::Waker,
}

/// The host's side of a mounted [`DesktopSurface`]: read the tree, publish navigation, queue
/// commands. Cheap to clone; single-threaded.
#[derive(Clone)]
pub struct DesktopHandle {
    shared: Rc<RefCell<Shared>>,
}

impl DesktopHandle {
    /// Confirm in passing an action that left nothing to see ("Link
    /// copied"): a short-lived pill, said politely to screen readers,
    /// never a notification. The host asks for a redraw.
    pub fn acknowledge(&self, text: impl Into<String>) {
        let shared = self.shared.borrow_mut();
        shared.acknowledger.say(text);
        shared.wake.wake();
    }

    pub fn revision(&self) -> u64 {
        self.shared.borrow().revision
    }

    /// Changes whenever the navigation snapshot may: the tree or what the surface presents beside
    /// it (pan, scroll, history, activity).
    pub fn navigation_revision(&self) -> u64 {
        let shared = self.shared.borrow();
        shared.revision.wrapping_add(shared.presented)
    }

    pub fn read<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R {
        read(&self.shared.borrow().tree)
    }

    /// The windows other devices host that this desk shows, with the device hosting each: the
    /// windows whose page a host asks the plane for.
    #[must_use]
    pub fn hosted_windows(&self) -> Vec<(TabId, String)> {
        self.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .filter_map(|tab| match &tab.tile {
                    tilo_desktop_model::TileContent::HostedWindow { host, .. } => {
                        Some((tab.id, host.clone()))
                    }
                    _ => None,
                })
                .collect()
        })
    }

    /// Have `wake` run whenever what this handle commands or changes is waiting for the desk's next
    /// pass: the host holding the desk asks for that frame, however little of the desk shows.
    pub fn set_wake(&self, wake: Rc<dyn Fn()>) {
        self.shared.borrow_mut().wake.set(wake);
    }

    /// The apps whose windows keep what they show to themselves: a password manager's, an app that
    /// asked for the same while it shows a secret. The host names them — it is the one that knows
    /// what a window may not be shown to — and from then on nothing makes a picture of their
    /// windows; a window is named by the app it belongs to.
    pub fn set_withheld_apps(&self, apps: Vec<String>) {
        self.shared.borrow_mut().withheld = apps.into_iter().collect();
    }

    /// Whether windows of `app_id` keep what they show to themselves, as the host last named them.
    #[must_use]
    pub fn withholds(&self, app_id: &str) -> bool {
        self.shared.borrow().withheld.contains(app_id)
    }

    /// The shown strip, the area it is drawn in (surface coordinates) and how far its content is
    /// panned past that area's left edge, as of the last layout: a point `x` over it is
    /// `x - area.x0 + pan` along the strip, however wide the surface or wherever it is panned to.
    pub fn shown_strip(&self) -> Option<(StripId, Rect, f64)> {
        self.shared.borrow().shown_strip
    }

    /// Where the shown strip's window bars and edges are for control hints,
    /// in the surface's coordinates, as last drawn.
    pub fn hint_places(&self) -> ui::widget::HintPlaces {
        self.shared.borrow().hint_places.clone()
    }

    /// The caret or selection this desktop publishes as its presence, given the keyboard-focused
    /// widget's (`keyboard`): see [`published_selection`], with the focused window's editors as of
    /// the last layout.
    pub fn published_selection(
        &self,
        keyboard: Option<(PresenceKey, PresencePayload)>,
    ) -> Option<(PresenceKey, PresencePayload)> {
        published_selection(keyboard, &self.shared.borrow().focused_presence)
    }

    /// What the surface presents beside the tree (pan, history, activity,
    /// notifications), as last presented.
    pub fn presentation(&self) -> Presentation {
        self.shared.borrow().presentation.clone()
    }

    pub fn snapshot(&self, generation: &str, applications: Vec<Application>) -> Snapshot {
        let shared = self.shared.borrow();
        navigation::snapshot_with(&shared.tree, generation, applications, &shared.presentation)
    }

    /// Admit `command` against the current tree and queue it for the next
    /// update pass. The host schedules a frame after an accepted command.
    ///
    /// # Errors
    /// As [`navigation::admit`], and refuses a pan, scroll or history move
    /// the presented desktop cannot make.
    pub fn dispatch(&self, verb: impl Into<Verb>) -> Result<(), String> {
        let verb = verb.into();
        let mut shared = self.shared.borrow_mut();
        let capabilities = shared.presentation.capabilities();
        let admitted = navigation::admit_with(&verb, &shared.tree, &capabilities)?;
        let presentation = &shared.presentation;
        match (&admitted, &verb) {
            (Admitted::Pan(strip, _), _)
                if presentation
                    .pan
                    .as_ref()
                    .is_none_or(|pan| pan.strip != strip.0.to_string()) =>
            {
                return Err("Desktop pan unavailable".into());
            }
            (Admitted::Scroll(..), Verb::Chrome(ChromeEffect::Scroll { region, .. }))
                if presentation
                    .scroll
                    .as_ref()
                    .is_none_or(|scroll| scroll.region != *region) =>
            {
                return Err("Scroll target is no longer active".into());
            }
            (Admitted::History(ids, direction), _)
                if navigation::active_route(&shared.tree) == Some(*ids)
                    && presentation.history.is_some_and(|history| match direction {
                        HistoryDirection::Back => !history.can_back,
                        HistoryDirection::Forward => !history.can_forward,
                    }) =>
            {
                return Err(match direction {
                    HistoryDirection::Back => "Nothing to go back to in this window",
                    HistoryDirection::Forward => "Nothing to go forward to in this window",
                }
                .into());
            }
            // A window someone else holds is theirs (or a moderator's) to close: refused here, said
            // at once, and nothing moves.
            (Admitted::Close(ids), _) if shared.holders.contains_key(&ids.tab) => {
                let refusal = holder_refusal(&shared.holders, ids.tab);
                shared.acknowledger.say(refusal.clone());
                return Err(refusal);
            }
            _ => {}
        }
        shared.queue.push(admitted);
        shared.wake.wake();
        Ok(())
    }

    /// The logical width of the surface, which window widths are fractions of. Hosts call this on
    /// every resize.
    pub fn set_viewport_width(&self, width: f64) {
        self.shared.borrow_mut().viewport_width = width.max(1.0);
    }

    /// How much of the desk's top edge something overlays now: the desktop bar's height while it
    /// shows over the windows under an auto-hiding bar, else zero. Every window moves down by it,
    /// whole, so the controls on its own bar stand below the bar instead of under it — where the
    /// bar's clock would read across a window's title on a phone.
    pub fn set_top_overlay_inset(&self, inset: f64) -> bool {
        let inset = inset.max(0.0);
        let mut shared = self.shared.borrow_mut();
        if (shared.top_overlay - inset).abs() <= f64::EPSILON {
            return false;
        }
        shared.top_overlay = inset;
        shared.changed();
        true
    }

    /// On a desktop several people share: who holds each window this desktop's person may not close
    /// or move, by tab. Those windows offer no close, resist a drag and name their holder on hover;
    /// every other window behaves as it always does. Applied to the windows on screen in the next
    /// update pass, and to every window built after. Whether that changed anything. Whether this
    /// desktop's person opens windows and tabs here: someone the workspace lets only look (a
    /// viewer) is offered no new tab. Whether that changed anything.
    pub fn set_opens(&self, opens: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let changed = shared.opens != opens;
        shared.opens = opens;
        changed
    }

    /// Tab `tab`'s window is waiting for the person (refused the keyboard,
    /// a dialog where they are not looking): its tab wears a dot until it
    /// has their focus. False when the desktop holds no such tab or it is
    /// the one they are looking at.
    pub fn ask_attention(&self, tab: TabId) -> bool {
        let mut shared = self.shared.borrow_mut();
        let looking = navigation::active_route(&shared.tree).map(|route| route.tab);
        if looking == Some(tab) || route_of(&shared.tree, tab).is_none() {
            return false;
        }
        shared.asking.insert(tab)
    }

    /// Whether tab `tab` 's window is waiting for the person.
    #[must_use]
    pub fn asks_attention(&self, tab: TabId) -> bool {
        self.shared.borrow().asking.contains(&tab)
    }

    pub fn set_holders(&self, holders: HashMap<TabId, ui::widget::TabHolder>) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.holders == holders {
            return false;
        }
        shared.holders = holders;
        true
    }

    /// Replace the tree with one changed elsewhere. The next update pass reconciles the live rows
    /// with it: tabs still present keep their running content, new tabs get content from the host's
    /// [`TileSource`], and removed tabs' applications close. Queued commands stay queued and are
    /// re-checked against the new tree when applied.
    pub fn replace_tree(&self, mut tree: DesktopTree) {
        let mut shared = self.shared.borrow_mut();
        observe_ids(&mut shared.minter, &tree);
        // A tree kept beside the state file carries no range of its own: it writes where this
        // desk's ids were minted.
        tree.salt = shared.minter.salt();
        shared.tree = tree;
        shared.replaced = true;
        shared.changed();
    }

    /// The workspace snapshot, excluding process-local stream proxies and device presentation.
    /// Durable profile saves still use the full tree.
    pub fn shared_tree(&self) -> DesktopTree {
        self.read(|tree| crate::shared_tree::projection(tree.clone()))
    }

    /// Apply workspace topology without replacing this device's stream bindings
    /// or focus. Only `local_tabs` survive omission from the shared snapshot;
    /// removed workspaces and strips still retire their contents.
    pub fn replace_shared_tree(&self, tree: DesktopTree, local_tabs: &[TabId]) {
        let (tree, opened) = self.read(|local| {
            let tree = crate::shared_tree::receive(local, tree, local_tabs);
            let opened = arrivals::opened_elsewhere(local, &tree, local_tabs);
            (tree, opened)
        });
        self.replace_tree(tree);
        self.note_arrivals(opened, ui::time::now());
    }

    /// Ask the [`TileSource`] for `tab` 's content again on a coming update pass, as when the tab
    /// was first shown: for a host whose first answer was a notice (its record had not arrived).
    /// The host schedules a frame.
    pub fn rebuild(&self, tab: TabId) {
        let mut shared = self.shared.borrow_mut();
        if !shared.rebuild.contains(&tab) {
            shared.rebuild.push(tab);
        }
    }

    /// `tab` stands in for the app whose window did not come, saying why: what the person sees
    /// where the window would be, and closes when they are done with it. False when the tab is
    /// gone, or holds a live window rather than an app this desk opened.
    pub fn note_in_tab(&self, tab: TabId, note: &str) -> bool {
        let mut tree = self.read(Clone::clone);
        let Some(found) = tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
            .find(|found| found.id == tab)
        else {
            return false;
        };
        let TileContent::LocalApp { note: said, .. } = &mut found.tile else {
            return false;
        };
        if said == note {
            return false;
        }
        note.clone_into(said);
        self.replace_tree(tree);
        self.rebuild(tab);
        true
    }

    /// A fresh id from the tree's id space.
    pub fn mint_id(&self) -> u64 {
        self.shared.borrow_mut().minter.mint()
    }

    /// Apply a workspace or strip command to the active workspace through
    /// the model's own reduction
    /// ([`tilo_desktop_model::commands::apply_command`]), with ids from the
    /// tree's id space. A strip it creates is shown; when the strip shown is
    /// removed, the workspace's first one is. The caller has asked the
    /// workspace's policy ([`tilo_desktop_model::commands::may_command`]) first.
    /// Returns the strip a `CreateStrip` made.
    ///
    /// # Errors
    ///
    /// The model's refusal (no active workspace, an invalid name, a strip
    /// that is gone or not empty, the last strip), or a command that is not
    /// about the workspace or its strips: windows change through
    /// navigation commands and gestures.
    pub fn apply_command(
        &self,
        command: &tilo_desktop_model::commands::DesktopCommand,
    ) -> Result<Option<StripId>, String> {
        use tilo_desktop_model::commands::{DesktopCommand, apply_command};
        if !matches!(
            command,
            DesktopCommand::RenameWorkspace { .. }
                | DesktopCommand::CreateStrip { .. }
                | DesktopCommand::RenameStrip { .. }
                | DesktopCommand::RemoveStrip { .. }
                | DesktopCommand::MoveStrip { .. }
        ) {
            return Err("only workspace and strip commands apply here".into());
        }
        let mut shared = self.shared.borrow_mut();
        let Shared { tree, minter, .. } = &mut *shared;
        let active = tree.active;
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.slot == active)
            .ok_or("there is no active workspace")?;
        let before: Vec<StripId> = workspace.strips.iter().map(|strip| strip.id).collect();
        apply_command(
            workspace,
            command,
            &mut || Ok(minter.mint()),
            &mut |_, _| Err("a workspace command opens no window".into()),
        )?;
        let after: Vec<StripId> = workspace.strips.iter().map(|strip| strip.id).collect();
        let made = after.iter().copied().find(|id| !before.contains(id));
        let first = after.first().copied();
        if let Some(made) = made {
            tree.active_strip.insert(active, made);
        } else if let Some(first) = first
            && tree
                .active_strip
                .get(&active)
                .is_none_or(|shown| !after.contains(shown))
        {
            tree.active_strip.insert(active, first);
        }
        shared.replaced = true;
        shared.changed();
        Ok(made)
    }

    /// Focus the window showing `tab` , on the next update pass. False when no window shows it.
    pub fn focus_tab(&self, tab: TabId) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = route_of(&shared.tree, tab) else {
            return false;
        };
        shared.queue.push(Admitted::Select(ids));
        shared.wake.wake();
        true
    }

    /// Focus a window of application `app_id` in the active workspace (the focused one when it is
    /// that application's), on the next update pass. False when the workspace has none.
    pub fn focus_app(&self, app_id: &str) -> bool {
        let tab = self.read(|tree| {
            let shows = |tab: &Tab| {
                matches!(&tab.tile, tilo_desktop_model::TileContent::LocalApp { app_id: id, .. } if id == app_id)
            };
            if let Some(active) = navigation::active_route(tree)
                && find_tab(tree, active.strip, active.tab).is_some_and(|(_, tab)| shows(tab))
            {
                return Some(active.tab);
            }
            tree.active_workspace()?
                .tabs()
                .find(|tab| shows(tab))
                .map(|tab| tab.id)
        });
        tab.is_some_and(|tab| self.focus_tab(tab))
    }
}

/// Why the window `tab` someone else holds cannot be closed here.
fn holder_refusal(holders: &HashMap<TabId, ui::widget::TabHolder>, tab: TabId) -> String {
    holders.get(&tab).map_or_else(
        || "Only whoever opened this window, or a moderator, can close it".to_owned(),
        |holder| {
            format!(
                "Only {}, who opened this window, or a moderator can close it",
                holder.name
            )
        },
    )
}

/// Answer what the windows asked of the activity with a host that keeps no history for the
/// workspace: each write that it isn't kept, each read that it is still to come, as a native
/// session reads a workspace it is not showing.
fn unavailable(requests: &ActivityRequests) {
    struct Unkept;
    impl ActivityReads for Unkept {
        fn revision(&self) -> u64 {
            0
        }
        fn read(
            &mut self,
            _action: &str,
            _arguments: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({ "pending": true }))
        }
    }
    for ask in requests.take() {
        if let ActivityAsk::Act { answer, .. } = ask {
            answer.complete(Err(crate::i18n::t("activity-served-unavailable")));
        }
    }
    requests.serve(&mut Unkept);
}

fn route_of(tree: &DesktopTree, tab: TabId) -> Option<RouteIds> {
    tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group.tabs.iter().any(|t| t.id == tab).then_some(RouteIds {
                    workspace: workspace.slot,
                    strip: strip.id,
                    group: group.id,
                    tab,
                })
            })
        })
    })
}

mod row_op;

/// What one app may ask of its host in one frame; more waits for the next.
const MAX_HOST_INTENTS: usize = 8;

/// Time the update pass may spend building deferred tabs in one frame. At least one tab is built
/// per frame, however long it takes.
const BUILD_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// A pause in wheel input this long ends a swipe. A trackpad keeps reporting while the fingers move
/// and through the momentum after they lift, and a spun mouse wheel's notches come 0 to 50 ms
/// apart, all well inside this; the pauses between spins are mostly longer.
const SWIPE_GAP: Duration = Duration::from_millis(200);
/// How soon search looks again for an app's answer that is still coming.
const SEARCH_ANSWER_POLL: Duration = tilo_desktop_search::ANSWER_POLL;

/// Sideways travel that steps focus one window on a strip that cannot pan
/// that way. About the width of a window: a Normal one is two thirds of a
/// 1280 px strip, so a spin steps about as many windows on a strip that fits
/// as it pans past on one that overflows. A notch of the native wheel (a
/// line) is a sixteenth of it, a spin of about twenty notches a window or
/// two.
const STEP_TRAVEL_PX: f64 = 640.0;

/// What asked for the strip to bring a window into view now: the input the
/// person just made, so a key's move slides within `fast` and a click glides,
/// or nothing, which means the change came from another device or a sync and
/// slides the same way a shortcut's does.
fn reveal_source() -> ui::tokens::MotionSource {
    ui::motion::recent_input(ui::motion::frame_now()).unwrap_or(ui::tokens::MotionSource::Action)
}

pub struct DesktopSurface {
    shared: Rc<RefCell<Shared>>,
    tiles: Box<dyn TileSource>,
    rows: HashMap<StripId, GroupRow>,
    /// The slots the apps put in their windows' bars ([`window_bars`]).
    window_slots: ui::window_slots::WindowSlots,
    shown: Option<StripId>,
    /// The strips the other displays show, laid out behind the shown one: each display keeps a
    /// strip of its own.
    behind: Vec<StripId>,
    rect: Rect,
    structure_dirty: bool,
    /// The shown strip moved (a pan, a window brought into view): the next frame lays it out again
    /// in the nodes it keeps.
    relayout: bool,
    close_reports: Vec<ui::application::ComponentCloseReport>,
    /// A launched window to pan into view once it has been laid out.
    reveal: Option<(StripId, GroupId)>,
    /// Tabs of a shown strip whose content is not built yet, in build order. Only the focused
    /// window is built before the strip's first frame; the rest follow within a per-frame budget.
    unbuilt: Vec<(StripId, TabId)>,
    /// The sideways swipe in progress, if any.
    swipe: Option<Swipe>,
    /// The strip a pan ended on, whose focus is to follow it once the strip rests where the pan
    /// left it: a thrown strip coasts past the window the fingers let go of, so focus lands on what
    /// the person is left looking at.
    pan_follow: Option<StripId>,
    /// The window a pan ended on. Focus goes to it where the strip is,
    /// without panning it into view: the strip stays where the person left
    /// it, however little a lone wheel notch moved it.
    settle: Option<GroupId>,
    search: Rc<RefCell<tilo_desktop_search::LazyDesktopSearch>>,
    /// The `Shared::search_revision` this surface last took up: what the host above the windows
    /// changed since then is work for the next pass.
    seen_search: u64,
    /// The window a group's new-tab control opened search for: the application chosen there opens
    /// as its tab, as in the native shell.
    new_tab_into: Option<(StripId, GroupId)>,
    /// The window search was opened over (its in-app search): an action chosen there runs inside
    /// that window's app.
    search_window: Option<(StripId, GroupId)>,
    /// For search over everything, the window publishing each resource it lists, by resource id: an
    /// app's action chosen there runs inside the app's window, which comes forward.
    search_owners: HashMap<String, (StripId, GroupId)>,
    /// An action chosen over a window that hosts no app to run it, left for the host's own door,
    /// which says why it did not run.
    unrouted_control: Option<ui::tilo_actions::ControlInvocation>,
    /// The pointer in logical coordinates, as it last moved over the surface.
    pointer: Point,
    platform: ui::platform::PlatformConventions,
    /// The host's retained workspace history, when it keeps one.
    history: Option<Box<dyn HistorySource>>,
    /// What the windows ask of the workspace's activity, when the host serves it to them.
    activity_requests: Option<ActivityRequests>,
    /// The activity model, while a window follows it or asks of it.
    activity: Option<Box<dyn history::Served>>,
    /// The model was asked for while the host still loads its code: frames keep coming, each asking
    /// again, until it can open.
    activity_loading: bool,
    /// The banners and centre, when the host installed notifications.
    notices: Option<notices::NotificationLayer>,
    /// What an action that leaves nothing to see confirms in passing ("Link copied"), over
    /// everything else.
    acknowledgement: ui::widget::acknowledgement::AcknowledgementLayer,
    /// "Kim opened Notes": windows others opened out of sight.
    arrival_hints: arrivals::ArrivalHints,
    /// "Not responding": the windows whose app stopped answering, each with its Wait and its Force
    /// quit.
    hung_apps: hung::HungApps,
    /// Where the host collects what controls emit, once it has bound this surface. A tab built
    /// after that (a launch, a record that arrived, a rebuilt row) is bound to it as it is built,
    /// or its registered buttons would emit into a queue nobody drains.
    controls: Option<ui::ControlDomain>,
    /// The tab the keyboard was last aimed at: the focused window's shown tab, as of the last
    /// presentation.
    keyboard_tab: Option<TabId>,
    /// A tab brought forward whose app the host has not activated yet
    /// ([`Widget::take_app_activation`]): it waits while the tab's app is still loading, so a
    /// window opened before its app arrived still takes the keyboard once it does.
    activation: Option<(StripId, TabId)>,
    /// The window a press landed in. When the press ends the strip pans it wholly into view: never
    /// during the press, which would move what was pressed from under the pointer.
    reveal_on_release: Option<(StripId, GroupId)>,
    /// The windows shared live, and the views windows embed of them.
    live_shares: live_shares::LiveShares,
    /// The strip or workspace change the frame draws as a slide, and where each strip stood when it
    /// was last drawn.
    pub(crate) slides: std::cell::RefCell<slide::Slides>,
    /// The theme's motion tokens, as the last frame laid out with them.
    motion: ui::tokens::EffectiveMotion,
    /// What asked for the change the frame shows, until it is shown.
    navigation_source: Option<ui::tokens::MotionSource>,
}

/// One horizontally dominant gesture, from its first event until it ends: a trackpad swipe and its
/// momentum, a spin of a mouse's sideways wheel, or three fingers carrying the strip.
#[derive(Clone, Copy)]
struct Swipe {
    /// Who took the gesture's first sideways event: the content under the pointer, or the strip,
    /// and so the rest of it, ends included.
    owner: Sideways,
    /// Which hand moves it, and so what its release coasts with.
    hand: paging::Hand,
    /// Travel since the last step, toward the next one, while the strip cannot pan the way the
    /// gesture goes.
    travel: f64,
    /// The gesture panned the strip, so its end leaves focus to follow it once the strip rests
    /// where they let go of it.
    panned: bool,
    /// How far the gesture has carried the strip since it took hold of it,
    /// past either end included, so there it pulls against the rubber band
    /// and its end lets the strip settle.
    pan: Option<f64>,
    /// How far three fingers have carried the strip since they landed: each report is their total,
    /// and the strip takes what is new of it.
    carried: f64,
    last: Instant,
}

impl DesktopSurface {
    /// Present `tree` . `salt` is this device's peer salt for new ids.
    pub fn new(tree: DesktopTree, salt: u16, tiles: Box<dyn TileSource>) -> (Self, DesktopHandle) {
        let mut minter = IdMinter::new(salt);
        observe_ids(&mut minter, &tree);
        let search: Rc<RefCell<tilo_desktop_search::LazyDesktopSearch>> =
            Rc::new(RefCell::new(tilo_desktop_search::LazyDesktopSearch::new()));
        let shared = Rc::new(RefCell::new(Shared::new(tree, minter, &search)));
        let acknowledger = shared.borrow().acknowledger.clone();
        #[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
        let tiles = Box::new(peer_view::PeerTiles::new(
            tiles,
            Rc::clone(&shared.borrow().peers),
        )) as Box<dyn TileSource>;
        let mut surface = Self::standing(Rc::clone(&shared), tiles, &search, acknowledger);
        surface.show_active_strip();
        (surface, DesktopHandle { shared })
    }

    /// The surface's own state, standing on the `shared` desk `new` made and drawing with `tiles` ,
    /// after its search `search` .
    fn standing(
        shared: Rc<RefCell<Shared>>,
        tiles: Box<dyn TileSource>,
        search: &Rc<RefCell<tilo_desktop_search::LazyDesktopSearch>>,
        acknowledger: ui::widget::acknowledgement::Acknowledger,
    ) -> Self {
        Self {
            shared,
            tiles,
            rows: HashMap::new(),
            window_slots: ui::window_slots::WindowSlots::default(),
            shown: None,
            behind: Vec::new(),
            rect: Rect::ZERO,
            structure_dirty: true,
            relayout: false,
            close_reports: Vec::new(),
            reveal: None,
            unbuilt: Vec::new(),
            swipe: None,
            pan_follow: None,
            settle: None,
            search: Rc::clone(search),
            seen_search: 0,
            new_tab_into: None,
            search_window: None,
            search_owners: HashMap::new(),
            unrouted_control: None,
            pointer: Point::ZERO,
            platform: ui::platform::PlatformConventions::default(),
            history: None,
            activity_requests: None,
            activity: None,
            activity_loading: false,
            notices: None,
            acknowledgement: ui::widget::acknowledgement::AcknowledgementLayer::new(acknowledger),
            arrival_hints: arrivals::ArrivalHints::default(),
            hung_apps: hung::HungApps::default(),
            controls: None,
            keyboard_tab: None,
            activation: None,
            reveal_on_release: None,
            live_shares: live_shares::LiveShares::default(),
            slides: std::cell::RefCell::new(slide::Slides::default()),
            motion: ui::tokens::EffectiveMotion::default(),
            navigation_source: None,
        }
    }

    /// Give every live window the holders and the agent runs the host named
    /// ([`DesktopHandle::set_holders`], [`DesktopHandle::set_opened_by`]), and open in History the
    /// run whose mark the person pressed. Whether that changed what one offers.
    fn apply_holders(&mut self) -> bool {
        {
            // The person is looking at the focused window, however they got there; a closed window
            // waits for no one.
            let mut shared = self.shared.borrow_mut();
            let looking = navigation::active_route(&shared.tree).map(|route| route.tab);
            let Shared { asking, tree, .. } = &mut *shared;
            asking.retain(|tab| Some(*tab) != looking && route_of(tree, *tab).is_some());
        }
        let shared = self.shared.borrow();
        let mut changed = false;
        let mut runs = Vec::new();
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
                changed |= group.set_new_tab(shared.opens);
                for tab in tabs {
                    changed |= group.set_tab_holder(tab, shared.holders.get(&tab).cloned());
                    changed |= group.set_tab_opened_by(tab, shared.opened_by.get(&tab).cloned());
                    changed |= group.set_tab_attention(tab, shared.asking.contains(&tab));
                }
                runs.extend(group.take_opened_by_press().map(|(_, run)| run));
            }
        }
        drop(shared);
        if let Some(run) = runs.pop() {
            self.open_run(run);
            changed = true;
        }
        changed
    }

    /// Read workspace history from `source` : the Activity app then shows it, once the host serves
    /// the windows what they ask of it ([`Self::serve_activity`]).
    pub fn set_history_source(&mut self, source: Box<dyn HistorySource>) {
        self.history = Some(source);
        let mut shared = self.shared.borrow_mut();
        let mut next = shared.presentation.clone();
        next.retained_history = true;
        shared.present(next);
    }

    /// Answer what the windows ask of the workspace's activity through `requests` (the desktop's
    /// end of its session) from the history source.
    pub fn serve_activity(&mut self, requests: ActivityRequests) {
        self.activity_requests = Some(requests);
    }

    /// Show `notifications` ' banners over the strip, and its centre when a `notification-center`
    /// command opens it.
    pub fn set_notifications(&mut self, notifications: DesktopNotifications) {
        self.notices = Some(notices::NotificationLayer::new(notifications));
        self.structure_dirty = true;
        self.refresh_notifications();
    }

    /// Show applications' notifications from `inbox` (the other half of the [`AppNotifier`] the
    /// host's services hand them), attributed to the window that posted each. Choosing one focuses
    /// that application's window and hands the application its activation.
    pub fn set_app_notifications(&mut self, inbox: AppInbox) {
        if let Some(layer) = self.notices.as_mut() {
            layer.inbox = Some(inbox);
        }
    }

    /// Open or close the notification centre, as the host's own control does.
    pub fn set_notification_center(&mut self, open: bool) {
        if let Some(layer) = self.notices.as_mut() {
            layer.set_center_open(open);
            self.refresh_notifications();
        }
    }

    /// Retire banners that ran out, open what the centre asked to, and record what the
    /// notifications show.
    fn refresh_notifications(&mut self) {
        let Some(layer) = self.notices.as_mut() else {
            return;
        };
        let notifications = layer.notifications().clone();
        if let Some(inbox) = layer.inbox.as_mut() {
            let shared = self.shared.borrow();
            let tree = &shared.tree;
            inbox.drain(&notifications, &|tab| {
                let ids = route_of(tree, tab)?;
                let (workspace, model) = find_tab(tree, ids.strip, tab)?;
                match &model.tile {
                    tilo_desktop_model::TileContent::LocalApp { app_id, label, .. } => {
                        Some((app_id.clone(), label.clone(), Some(workspace.slot.raw())))
                    }
                    _ => None,
                }
            });
        }
        layer.tick();
        let mut open_app = layer.open_app.take();
        for outcome in layer.notifications().take_outcomes() {
            let NotificationOutcome::Activated {
                notification,
                action,
            } = outcome
            else {
                continue;
            };
            if let Some(choice) = action.as_deref().and_then(search::host_choice) {
                self.shared
                    .borrow_mut()
                    .host_choices
                    .push(choice.to_owned());
                continue;
            }
            if let Some(uri) = layer
                .inbox
                .as_mut()
                .and_then(|inbox| inbox.take_activation(notification.id))
            {
                ui::notify::report_activation(&uri);
            }
            if let Some(app) = notification.app_id {
                open_app = Some(app);
            }
        }
        let (active, retained) = layer.notifications().counts();
        let state = NotificationsState {
            active,
            retained,
            center_open: layer.center_open(),
        };
        let handle = DesktopHandle {
            shared: Rc::clone(&self.shared),
        };
        if let Some(app) = open_app
            && !handle.focus_app(&app)
        {
            self.shared
                .borrow_mut()
                .queue
                .push(Admitted::Launch(app, false));
        }
        let mut shared = self.shared.borrow_mut();
        let mut next = shared.presentation.clone();
        next.notifications = Some(state);
        shared.present(next);
    }

    /// Serve the windows what they asked of the workspace's activity: the activity model is open
    /// while one follows it or asks of it, for the active workspace, and each read it follows is
    /// read again whenever the model moved.
    fn sync_activity(&mut self) {
        let Some(requests) = self.activity_requests.clone() else {
            return;
        };
        // A view offered to the app stays offered until it showed it.
        let wanted = requests.wanted()
            || self.shared.borrow().activity_focus.is_some()
            || self
                .activity
                .as_ref()
                .is_some_and(|model| model.offers_focus());
        let workspace = self.shared.borrow().tree.active;
        if let Some(mut old) = self
            .activity
            .take_if(|model| !wanted || model.workspace() != workspace)
        {
            old.finish();
        }
        self.activity_loading = false;
        if !wanted {
            return;
        }
        let Some(source) = self.history.as_mut() else {
            unavailable(&requests);
            return;
        };
        if self.activity.is_none() {
            match source.activity_pane() {
                ActivityPaneCode::Ready(open) => {
                    let shared = self.shared.borrow();
                    self.activity = open(source.as_mut(), &shared.tree, shared.revision, workspace)
                        .map(|model| model.0);
                }
                ActivityPaneCode::Loading => {
                    self.activity_loading = true;
                    return;
                }
                ActivityPaneCode::Unavailable => {}
            }
        }
        let Some(model) = self.activity.as_mut() else {
            self.shared.borrow_mut().activity_focus = None;
            unavailable(&requests);
            return;
        };
        let mut opened = None;
        {
            let mut shared = self.shared.borrow_mut();
            model.follow(source.as_mut(), &shared.tree, shared.revision);
            if let Some((view, author)) = shared.activity_focus.take() {
                model.focus(view, author);
            }
        }
        for ask in requests.take() {
            match ask {
                ActivityAsk::Act {
                    action,
                    arguments,
                    answer,
                } => answer.complete(model.act(&action, &arguments).map(|open| {
                    if open.is_some() {
                        opened = open;
                    }
                })),
                ActivityAsk::BeginSummary => model.begin_summary(),
                ActivityAsk::EndSummary => model.end_summary(),
            }
        }
        let reads: &mut dyn ActivityReads = model.as_mut();
        requests.serve(reads);
        requests.wake_waiting();
        // A record opened from the history: its window comes forward.
        if let Some(target) = opened {
            let mut shared = self.shared.borrow_mut();
            if let Some(ids) = source
                .resource_tab(&shared.tree, &target)
                .and_then(|tab| route_of(&shared.tree, tab))
            {
                shared.queue.push(Admitted::Select(ids));
            }
        }
    }

    fn shown_row(&mut self) -> Option<&mut GroupRow> {
        let strip = self.shown?;
        self.rows.get_mut(&strip)
    }

    /// Hand each row the windows the workspace holds floating above its strip, of the strip the
    /// person works in: the others show their windows where they are.
    fn sync_floats(&mut self) {
        let shown = self.shown;
        let mut returning = None;
        for (strip, row) in &mut self.rows {
            let floats = if Some(*strip) == shown {
                floats::floats_of(
                    &self.shared.borrow().tree,
                    *strip,
                    floats::area_of(row.get_rect()),
                )
                .into_iter()
                .map(|(group, at)| (group, row.keep_float_inside(at)))
                .collect()
            } else {
                HashMap::new()
            };
            // A window the record put back in its strip after floating is the one the person just
            // snapped back: its strip brings it into view as it does a window that opens or takes
            // the focus, or the window lands where the pan left the strip rather than where it
            // stood when it was lifted.
            let back = row
                .floats()
                .keys()
                .find(|group| !floats.contains_key(group))
                .copied();
            row.set_floats(floats);
            if Some(*strip) == shown
                && let Some(group) = back
                && !row.is_floating(group)
            {
                returning = Some((*strip, group));
            }
        }
        if self.reveal.is_none() {
            self.reveal = returning;
        }
    }

    /// Build (once) and show the active workspace's active strip.
    fn show_active_strip(&mut self) {
        let shown_before = self.shown;
        let shared = self.shared.borrow();
        let tree = &shared.tree;
        let Some(workspace) = tree.active_workspace() else {
            if self.shown.take().is_some() {
                self.structure_dirty = true;
            }
            return;
        };
        let strip_id = navigation::active_strip(tree, workspace);
        if let Some(strip_id) = strip_id
            && !self.rows.contains_key(&strip_id)
            && let Some(strip) = workspace.strips.iter().find(|s| s.id == strip_id)
        {
            let focused = tree.active_group_for(strip_id);
            let (mut row, deferred) = build_row_lazily(
                self.tiles.as_mut(),
                self.controls.as_ref(),
                workspace,
                strip,
                focused,
            );
            // A row comes back with the window covering this display, so a rebuild while it covers
            // does not take it back.
            row.set_fullscreen(tree.fullscreen_group(), false);
            // The focused window, built first, is also the one in view.
            if self.reveal.is_none() {
                self.reveal = focused.map(|group| (strip_id, group));
            }
            self.unbuilt
                .extend(deferred.into_iter().map(|tab| (strip_id, tab)));
            self.rows.insert(strip_id, row);
        }
        let changed = self.shown != strip_id;
        let tree = if changed {
            Some(shared.tree.clone())
        } else {
            None
        };
        drop(shared);
        if let Some(tree) = tree {
            self.show_strip_changed(shown_before, strip_id, &tree);
        }
        self.show_display_strips();
    }

    /// Build deferred tabs of the shown strip until the frame's budget is spent. Tabs of strips not
    /// shown wait until theirs is.
    fn build_deferred(&mut self) {
        for tab in std::mem::take(&mut self.shared.borrow_mut().rebuild) {
            let strip = self.rows.iter_mut().find_map(|(strip, row)| {
                row.groups
                    .iter_mut()
                    .any(|group| group.host_for_tab_mut(tab).is_some())
                    .then_some(*strip)
            });
            if let Some(strip) = strip
                && !self.unbuilt.contains(&(strip, tab))
            {
                self.unbuilt.push((strip, tab));
            }
        }
        let Some(shown) = self.shown else {
            return;
        };
        let started = ui::time::now();
        let shared = self.shared.borrow();
        let mut waiting = Vec::new();
        let mut queue = std::mem::take(&mut self.unbuilt).into_iter();
        for (strip, tab) in queue.by_ref() {
            if strip != shown {
                waiting.push((strip, tab));
                continue;
            }
            let Some((workspace, model)) = find_tab(&shared.tree, strip, tab) else {
                continue;
            };
            let Some(host) = self.rows.get_mut(&strip).and_then(|row| {
                row.groups
                    .iter_mut()
                    .find_map(|group| group.host_for_tab_mut(tab))
            }) else {
                continue;
            };
            let rebuilt = build_tile(
                self.tiles.as_mut(),
                self.controls.as_ref(),
                workspace,
                model,
            );
            // A window that held an app's place (its code or record still coming) hands its
            // arrival, and its place in the layout, to what is built there now: the window says how
            // its layout changes, so the rest of the desktop's is kept.
            host.take_place_of(rebuilt);
            if ui::time::now().duration_since(started) >= BUILD_BUDGET {
                break;
            }
        }
        waiting.extend(queue);
        drop(shared);
        self.unbuilt = waiting;
    }

    /// Bring the rows to a replaced tree, keeping live content by tab id. A strip already shown
    /// keeps its row and changes in place ([`splice::splice_row`]): its pan stays where the person
    /// left it, and windows removed, added or moved elsewhere slide as they do when the person
    /// closes, opens or moves one here. Rebuilding the row instead threw its pan away, so each
    /// peer's edit, or the workspace's echo of a close, flew the strip home and cut every window to
    /// its new place.
    fn reconcile(&mut self) {
        if !std::mem::take(&mut self.shared.borrow_mut().replaced) {
            return;
        }
        let shared = Rc::clone(&self.shared);
        let shared = shared.borrow();
        let tree = &shared.tree;
        let homes: HashMap<TabId, (StripId, GroupId)> = tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| {
                s.window_groups
                    .iter()
                    .flat_map(move |g| g.tabs.iter().map(move |t| (t.id, (s.id, g.id))))
            })
            .collect();
        let unbuilt: std::collections::HashSet<TabId> =
            self.unbuilt.iter().map(|&(_, tab)| tab).collect();
        // Tabs leaving their window, or the desktop, give up their content first, so a window they
        // move to can take it over.
        let mut live: HashMap<TabId, TileHost> = HashMap::new();
        let mut rows = std::mem::take(&mut self.rows);
        for (&strip, row) in &mut rows {
            for group in row.groups.iter_mut().filter(|g| !g.is_closing()) {
                let leaving: Vec<TabId> = group
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .filter(|tab| homes.get(tab) != Some(&(strip, group.id)))
                    .collect();
                for tab in leaving {
                    if let Some((_, mut host)) = group.detach_tab(tab) {
                        if unbuilt.contains(&tab) {
                            self.close_reports
                                .extend(ui::component_lifecycle::close_components(&mut host));
                        } else {
                            live.insert(tab, host);
                        }
                    }
                }
            }
        }
        let mut built: std::collections::HashSet<TabId> = std::collections::HashSet::new();
        for workspace in &tree.workspaces {
            for strip in &workspace.strips {
                if let Some(mut row) = rows.remove(&strip.id) {
                    splice::splice_row(&mut row, strip, &mut |tab| {
                        live.remove(&tab.id).unwrap_or_else(|| {
                            built.insert(tab.id);
                            build_tile(self.tiles.as_mut(), self.controls.as_ref(), workspace, tab)
                        })
                    });
                    self.rows.insert(strip.id, row);
                    continue;
                }
                // Strips with surviving content are built now so that content is not dropped; the
                // rest are built when first shown.
                let survives = strip
                    .window_groups
                    .iter()
                    .flat_map(|group| &group.tabs)
                    .any(|tab| live.contains_key(&tab.id));
                if survives {
                    let mut row = build_row(
                        self.tiles.as_mut(),
                        self.controls.as_ref(),
                        workspace,
                        strip,
                        &mut live,
                    );
                    if tree.active_strip_id() == Some(strip.id) {
                        row.set_fullscreen(tree.fullscreen_group(), false);
                    }
                    self.rows.insert(strip.id, row);
                }
            }
        }
        // Strips that went away close what they showed.
        for mut row in rows.into_values() {
            for group in &mut row.groups {
                self.close_reports
                    .extend(ui::component_lifecycle::close_components(group));
            }
        }
        for mut host in live.into_values() {
            self.close_reports
                .extend(ui::component_lifecycle::close_components(&mut host));
        }
        // A tab still waiting for its content waits where it stayed; one that moved was built with
        // its new window.
        let rows = &self.rows;
        self.unbuilt.retain(|&(strip, tab)| {
            !built.contains(&tab)
                && homes.get(&tab).is_some_and(|&(home, _)| home == strip)
                && rows.get(&strip).is_some_and(|row| {
                    row.groups
                        .iter()
                        .any(|g| g.tabs().iter().any(|t| t.id == tab))
                })
        });
        self.shown = None;
        self.structure_dirty = true;
    }

    /// A press anywhere in a window, its body included and not only its tab bar, makes that window
    /// the active one. It stays where it is while pressed, since panning would move it from under
    /// the pointer; the press's release brings it wholly into view.
    fn focus_clicked_group(&self) {
        let Some(strip_id) = self.shown else {
            return;
        };
        let Some(group) = self
            .rows
            .get(&strip_id)
            .and_then(GroupRow::group_under_cursor)
        else {
            return;
        };
        let mut shared = self.shared.borrow_mut();
        if shared.tree.active_group_for(strip_id) == Some(group)
            || find_group(&mut shared.tree, strip_id, group).is_none()
        {
            return;
        }
        shared.tree.set_active_group(strip_id, group);
        shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
        shared.changed();
    }

    /// Mirror gestures the shown row performed into the tree.
    fn harvest(&mut self) {
        let Some(strip_id) = self.shown else {
            return;
        };
        let Some(row) = self.rows.get_mut(&strip_id) else {
            return;
        };
        let mut shared = self.shared.borrow_mut();
        let mut changed = false;
        if let Some(payload) = row.take_pending_cross_strip_detach() {
            let down = payload.release_point.y > row.get_rect().y1;
            changed |= window_move::carry_out(&mut shared, strip_id, row, payload, down);
        }
        if let Some((group, index)) = row.take_pending_group_reorder() {
            changed |= shared
                .tree
                .reorder_group_within_strip(strip_id, group, index)
                .is_some();
        }
        // A window lifted over the desk, or let go back into its strip.
        if let Some(ask) = row.take_pending_float() {
            let area = floats::area_of(row.get_rect());
            changed |= floats::carry_out(&mut shared, strip_id, ask, area);
        }
        let mut new_tab = None;
        let mut search_in = None;
        for group in &mut row.groups {
            if group.take_pending_search_open().is_some() {
                search_in = Some((group.id, ui::search::SearchRequest::this_app()));
            }
            // An app asks its host for what only the host can do. Here that is search: the app's
            // own search button opens the window's search, as the window bar's does. What else an
            // app may ask (moving a device's display, a host shell, opening a local document) a
            // browser page cannot do, so it is let go.
            let group_id = group.id;
            if let Some(host) = group.active_host_mut() {
                for _ in 0..MAX_HOST_INTENTS {
                    let Some(intent) = host.take_host_intent() else {
                        break;
                    };
                    if let ui::widget::AppHostIntent::OpenSearch(request) = intent {
                        search_in = Some((group_id, request));
                    }
                }
            }
            for tab in group.take_pending_close_commits() {
                changed |= shared.tree.close_tab(tab).is_some();
            }
            if let Some((from, to)) = group.take_pending_tab_reorder() {
                changed |= shared
                    .tree
                    .reorder_tab_within_group(group.id, from, to)
                    .is_some();
            }
            if let Some(tab) = group.take_tab_activation() {
                if let Some(model) = find_group(&mut shared.tree, strip_id, group.id)
                    && model.tabs.iter().any(|t| t.id == tab)
                {
                    model.activate_tab(tab);
                }
                shared.tree.set_active_group(strip_id, group.id);
                changed = true;
            }
            if let Some(width) = group.take_pending_width_change()
                && let Some(model) = find_group(&mut shared.tree, strip_id, group.id)
            {
                model.width = width;
                changed = true;
            }
            if let Some(maximized) = group.take_pending_maximize_change()
                && let Some(model) = find_group(&mut shared.tree, strip_id, group.id)
            {
                model.maximized = maximized;
                changed = true;
                if !maximized {
                    // Restored to its width, the window comes back into view where the strip stood
                    // before it filled the display, which may leave its controls off screen.
                    self.reveal = Some((strip_id, group.id));
                }
            }
            if group.take_pending_new_tab().is_some() {
                new_tab = Some(group.id);
            }
        }
        if let Some(topology) = row.take_pending_topology_change() {
            let shape: Vec<GroupShape> = topology
                .into_iter()
                .map(|group| GroupShape {
                    id: group.id,
                    tab_ids: group.tab_ids,
                    active_tab: group.active_tab,
                    width: group.width,
                })
                .collect();
            changed |= shared
                .tree
                .reconcile_strip_topology(strip_id, &shape)
                .is_some();
        }
        if let Some(request) = search::search_asked(&mut shared.tree, strip_id, new_tab, search_in)
        {
            shared.search_request = Some(request);
        }
        shared.menus.extend(window_menu::take_asked_menus(row));
        if changed {
            shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
            shared.changed();
        }
    }

    /// The work area the shown strip stands on, as of this surface's last
    /// layout: where a window opened over the desk floats.
    fn shown_area(&self, shared: &Shared) -> Option<CanvasPlace> {
        let workspace = shared.tree.active_workspace()?;
        let strip = navigation::active_strip(&shared.tree, workspace)?;
        Some(floats::area_of(self.rows.get(&strip)?.get_rect()))
    }

    /// Record what the shown strip and focused window present beside the tree, for the navigation
    /// snapshot.
    fn refresh_presentation(&self) {
        let (active, tree_active) = {
            let shared = self.shared.borrow();
            (navigation::active_route(&shared.tree), shared.tree.active)
        };
        let mut next = self.shared.borrow().presentation.clone();
        next.pan = self.shown.zip(self.shown_row_ref()).map(|(strip, row)| {
            let viewport = row.get_rect().width();
            let maximized = row.groups.iter().any(|g| g.maximized);
            let max = if maximized {
                0.0
            } else {
                row.pan_max_scroll_x()
            };
            DesktopPan {
                workspace: tree_active.0.to_string(),
                strip: strip.0.to_string(),
                offset: row.scroll_x().clamp(0.0, max),
                viewport,
                content: if maximized {
                    viewport
                } else {
                    row.content_width().max(viewport)
                },
                max,
            }
        });
        let host = active.and_then(|ids| {
            let row = self.rows.get(&ids.strip)?;
            let group = row.groups.iter().find(|g| g.id == ids.group)?;
            (group.active_tab_id() == ids.tab)
                .then(|| group.active_host())
                .flatten()
                .map(|host| (ids, host))
        });
        next.scroll = host.and_then(|(ids, host)| {
            let region = host.primary_vertical_scroll()?;
            let m = region.metrics;
            Some(ScrollState {
                region: navigation::scroll_region(ids, region.key),
                offset: m.offset.clamp(0.0, m.max_scroll()),
                viewport: m.viewport,
                content: m.content,
                max: m.max_scroll(),
            })
        });
        next.history = host.map(|(_, host)| {
            let available = host.nav_availability();
            HistoryState {
                can_back: available.back,
                can_forward: available.forward,
            }
        });
        self.shared.borrow_mut().present(next);
    }

    fn shown_row_ref(&self) -> Option<&GroupRow> {
        self.rows.get(&self.shown?)
    }

    /// Where window `group` 's new-tab control is drawn, while its strip is shown, so a test
    /// presses it as a person does.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn new_tab_rect(&self, group: GroupId) -> Option<Rect> {
        self.shown_row_ref()?
            .groups
            .iter()
            .find(|g| g.id == group)
            .map(WindowGroup::new_tab_rect)
            .filter(|rect| rect.area() > 0.0)
    }

    /// Present a launched window. A row first built after the launch already holds it, with its
    /// content, from the tree.
    fn open_group(&mut self, strip: StripId, group: GroupId, index: usize, tab: &Tab, float: bool) {
        let Some(row) = self.rows.get_mut(&strip) else {
            return;
        };
        if !row.groups.iter().any(|g| g.id == group) {
            let shared = self.shared.borrow();
            let Some(workspace) = shared.tree.active_workspace() else {
                return;
            };
            let mut built = WindowGroup::new(
                group,
                vec![(descriptor(tab), placeholder())],
                tab.id,
                WindowWidth::Normal,
            );
            if let Some(host) = built.host_for_tab_mut(tab.id) {
                *host = build_tile(self.tiles.as_mut(), self.controls.as_ref(), workspace, tab);
            }
            row.insert_group(built, index);
            // The window opens on the display the person works on, so it
            // owns that display's lane before anything is shared: it is
            // placed whole on screen beside the window it opened next to,
            // sharing the display with it if need be, as on the native
            // desktop — revealing it alone pans the window in use half off
            // screen, taking its name with it.
            if let Some(display) = shared.tree.outputs.focused_display() {
                row.set_group_display(group, display);
            }
            // The strip lands where the window opens, in the pass that opens it: its slots take the
            // widths they are going to and its windows the places, so the frame that maps the
            // window shows it in place — the place the compositor is given is one on screen. Eased,
            // the window would be laid out past a neighbour still on its way to its width, often
            // off the display, and no frame would show it until it arrived there.
            row.set_motion_source(ui::tokens::MotionSource::Action);
            // A window the workspace holds floating takes no run of the strip, so
            // it shares nothing with what stands there — unless it opens over the
            // desk beside it, where the strip gives up the width its card draws at,
            // band and ring included, so the card stands beside the window it
            // opened next to, clear of it, not over it.
            if float {
                row.share_with_floating_neighbour(group, FLOAT_SIZE.0 + 2.0 * FLOAT_CARD_ROOM);
            } else if !floats::holds(&shared.tree, strip, group) {
                row.share_with_neighbour(group);
            }
        }
        if float
            && let Some(area) = {
                let shared = self.shared.borrow();
                self.shown_area(&shared)
            }
            && let Some(row) = self.rows.get(&strip)
        {
            let mut shared = self.shared.borrow_mut();
            floats::stand_beside(&mut shared, strip, row, group, area);
        }
        self.reveal = Some((strip, group));
        self.structure_dirty = true;
    }

    fn group_widget(&mut self, strip: StripId, group: GroupId) -> Option<&mut WindowGroup> {
        self.rows
            .get_mut(&strip)?
            .groups
            .iter_mut()
            .find(|g| g.id == group)
    }

    /// Accent the active group of the shown strip and lay out against the host's viewport.
    fn present(&mut self) {
        self.lay_out_displays();
        // The window the person took across the display takes the whole strip with it: its row
        // drops the gutters and the other windows step aside, wherever the person last panned them.
        // A row is built with this too; here it follows the window menu or the chord between
        // rebuilds.
        let (fullscreen, top_overlay) = {
            let shared = self.shared.borrow();
            (shared.tree.fullscreen_group(), shared.top_overlay)
        };
        if let Some(row) = self.shown_row() {
            row.set_fullscreen(fullscreen, false);
        }
        // The desktop bar over the desk (an auto-hiding one, while it shows): every row's windows
        // move down by it, so nothing of theirs stands under its clock.
        for row in self.rows.values_mut() {
            row.set_top_overlay_inset(top_overlay);
        }
        // A window the workspace holds floating stands over the desk, at the place its record keeps
        // for this device's display; every other strip shows its windows in their strip.
        self.sync_floats();
        let (route, width) = {
            let shared = self.shared.borrow();
            (
                navigation::active_route(&shared.tree),
                shared.viewport_width,
            )
        };
        let active = route.map(|ids| ids.group);
        // Whatever brought a window forward (a click, a launch, a switch, a closed neighbour), its
        // app takes the keyboard.
        let tab = route.map(|ids| ids.tab);
        if tab != self.keyboard_tab {
            self.keyboard_tab = tab;
            self.activation = route.map(|ids| (ids.strip, ids.tab));
        }
        if let Some(row) = self.shown_row() {
            for group in &mut row.groups {
                group.set_focused(Some(group.id) == active);
            }
            if row
                .output_spans()
                .first()
                .is_none_or(|span| span.width.to_bits() != width.to_bits())
            {
                // Windows size against the width between the strip's two gutters, so one as wide as
                // the display still shows its border at both ends.
                row.set_output_spans(&[OutputSpan {
                    x: 0.0,
                    width,
                    usable: (width - 2.0 * STRIP_EDGE_GUTTER_PX).max(1.0),
                }]);
            }
        }
    }
}

impl DesktopSurface {
    /// Open `app` as a tab of window `group` in strip `strip` , focused. Returns whether it opened.
    fn open_tab_in_group(&mut self, strip: StripId, group: GroupId, app: &str) -> bool {
        let mut shared = self.shared.borrow_mut();
        let id = shared.minter.mint();
        let Some(workspace) = shared.tree.active_workspace() else {
            return false;
        };
        let Some(tab) = self.tiles.launch(workspace, id, app) else {
            return false;
        };
        let host = build_tile(self.tiles.as_mut(), self.controls.as_ref(), workspace, &tab);
        let Some(model) = find_group(&mut shared.tree, strip, group) else {
            return false;
        };
        model.tabs.push(tab.clone());
        model.active_tab = tab.id;
        let index = model.tabs.len().saturating_sub(1);
        shared.tree.set_active_group(strip, group);
        if let Some(widget) = self
            .rows
            .get_mut(&strip)
            .and_then(|row| row.groups.iter_mut().find(|g| g.id == group))
        {
            widget.insert_detached_tab(index, descriptor(&tab), host);
            widget.set_active_tab(tab.id);
        }
        shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
        shared.changed();
        true
    }
}

impl Widget for DesktopSurface {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::component_lifecycle::ComponentLifecycle
    );

    fn name(&self) -> &'static str {
        "DesktopSurface"
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if let Some(row) = self.shown_row() {
            row.sync(state, path);
        }
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        // Which apps stopped answering is the host's to report; a new report is a layout of its
        // own, so their cards are placed this frame.
        let unresponsive = self.shared.borrow().unresponsive_revision;
        self.hung_apps.follow(unresponsive);
        let before = self.shared.borrow().revision;
        let search_was_open = self.search.borrow().is_open();
        self.end_idle_swipe(ui::time::now());
        // What the apps of its windows left of their sideways wheels moves the strip before this
        // pass draws it.
        let panned = self.take_app_sideways(ui::time::now());
        self.reconcile();
        self.harvest();
        // A window the pass just moved or renamed is a tree the rows follow now: what it was
        // running goes to the window it moved to in this pass, rather than being built there again.
        self.reconcile();
        {
            let (doc, root) = ui::doc::document_ref(state, path);
            self.apply_commands(doc, root);
            // Search opens over the tree as these commands left it, and what it chose applies in
            // this same pass.
            self.step_search(doc);
            self.apply_commands(doc, root);
        }
        self.show_active_strip();
        #[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
        let peers_changed = self.follow_peer_streams();
        self.build_deferred();
        self.present();
        let held = self.apply_holders();
        let changed = self.shared.borrow().revision != before
            || search_was_open != self.search.borrow().is_open()
            || held;
        #[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
        let changed = changed || peers_changed;
        let row_changed = self.shown_row().is_some_and(|row| row.update(state, path));
        self.offer_window_shares();
        let bars_changed = self.sync_window_bars();
        let share_asked = self.take_window_share_asks();
        let embeds_changed = self.follow_embeds(state, path);
        self.refresh_presentation();
        self.sync_activity();
        self.refresh_notifications();
        if !self.shared.borrow().queue.is_empty() {
            // The history, the centre or a window's share opened a window.
            let (doc, root) = ui::doc::document_ref(state, path);
            self.apply_commands(doc, root);
            self.show_active_strip();
            self.present();
            self.refresh_presentation();
        }
        // Last, so a row this pass rebuilt shows its windows' own artwork in the same frame it
        // appeared in.
        let artwork = self.sync_window_artwork();
        self.seen_search = self.shared.borrow().search_revision;
        changed
            || row_changed
            || bars_changed
            || share_asked
            || embeds_changed
            || artwork
            || panned
            || self.structure_dirty
    }

    /// The windows' edits, then the views they embed.
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        self.settle_embeds(edits, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.present();
        self.motion = ui::tokens::EffectiveMotion::new(theme.tokens.motion, theme.reduce_motion);
        let rows: Vec<NodeId> = self
            .drawn_strips()
            .into_iter()
            .filter_map(|strip| {
                let row = self.rows.get_mut(&strip)?;
                let node = row.build_layout(taffy, theme, node_map);
                let style = strip_style(row, taffy.style(node).cloned().unwrap_or_default());
                let _ = taffy.set_style(node, style);
                Some(node)
            })
            .collect();
        let notices = self
            .notices
            .as_mut()
            .map(|layer| layer.build_layout(taffy, theme, node_map));
        let arrivals = self.arrival_hints.build_layout(taffy, theme, node_map);
        let hung = self.hung_apps.build_layout(taffy, theme, node_map);
        let acknowledgement = self.acknowledgement.build_layout(taffy, theme, node_map);
        // The rows, each over the whole surface and drawing only in its
        // displays' lanes, come first, then the notifications, when present,
        // the arrival hints and the cards of apps that stopped answering,
        // and the acknowledgement last. Search is drawn over the windows, by
        // the host above them, not in this tree.
        let children: Vec<NodeId> = rows
            .into_iter()
            .chain(notices)
            .chain([arrivals, hung, acknowledgement])
            .collect();
        let style = Style {
            display: ui::taffy::Display::Flex,
            flex_direction: ui::taffy::FlexDirection::Column,
            size: ui::taffy::Size {
                width: ui::taffy::Dimension::percent(1.0),
                height: ui::taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let nodes = taffy.children(node).unwrap_or_default();
        let strips = self.drawn_strips();
        let next = strips.len();
        if let Some(layer) = self.notices.as_mut()
            && let Some(&notices) = nodes.get(next)
        {
            layer.apply_layout(taffy, notices, inner);
        }
        if let [.., arrivals, hung, acknowledgement] = nodes[..] {
            self.arrival_hints.apply_layout(taffy, arrivals, inner);
            self.hung_apps.apply_layout(taffy, hung, inner);
            self.acknowledgement
                .apply_layout(taffy, acknowledgement, inner);
        }
        let reveal = self.reveal.take();
        let shown = self.shown;
        // A strip that is not drawn yet keeps its reveal: the window it opens is framed when the
        // strip showing it is drawn, and until then the strip has not landed its widths.
        let mut pending = reveal;
        for (i, strip) in strips.iter().enumerate() {
            let Some(&child) = nodes.get(i) else {
                continue;
            };
            let Some(row) = self.rows.get_mut(strip) else {
                continue;
            };
            row.apply_layout(taffy, child, inner);
            let Some((_, group)) = reveal.filter(|(reveal_strip, _)| reveal_strip == strip) else {
                continue;
            };
            if !row.group_is_laid_out(group) {
                continue;
            }
            // The strip has laid its windows out where the window opens, in this pass: from here on
            // they follow the hand again.
            row.set_motion_source(ui::tokens::MotionSource::Pointer);
            pending = None;
            // The launch frame itself shows the new window: move the strip to it, then place the
            // groups again where the move has them.
            if Some(*strip) == shown && row.scroll_group_into_view(group) {
                // The window is on screen in the frame that opens it,
                // not sliding in after it: the frame the person sees
                // their new window must show it.
                row.apply_layout(taffy, child, inner);
            }
        }
        self.reveal = pending;
        self.note_laid_strip();
        self.settle_pan_focus();
        if self.slides.borrow().is_moving(ui::motion::frame_now()) {
            // A slide moves drawings, not places: the next frame lays out again to draw what it
            // left behind at its pose.
            ui::widget::note_layout_moved();
        }
        let search_area = self
            .search
            .borrow()
            .is_open()
            .then(|| self.searched_window_rect())
            .flatten();
        {
            let mut shared = self.shared.borrow_mut();
            if shared.search_area != search_area {
                shared.search_area = search_area;
                shared.search_revision = shared.search_revision.wrapping_add(1);
            }
        }
        let shown_tabs = ShownTabs::of_drawn(self);
        let shown_strip = self
            .shown
            .zip(self.shown_row_ref())
            .map(|(strip, row)| (strip, row.viewport(), row.drawn_scroll_x()));
        // Banners start where the windows' content does, below their bars.
        let content_top = shown_tabs.content_top();
        if let Some(layer) = self.notices.as_mut() {
            layer.set_clear_top(content_top);
        }
        self.place_arrival_hints(content_top);
        self.place_hung_apps();
        self.drain_liveness_choices();
        let focused_presence = {
            let shared = self.shared.borrow();
            navigation::active_route(&shared.tree)
                .and_then(|ids| {
                    let row = self.rows.get(&ids.strip)?;
                    let group = row.groups.iter().find(|g| g.id == ids.group)?;
                    group.active_host()
                })
                .map(|host| ui::presence::capability::subtree_presence(host))
                .unwrap_or_default()
        };
        {
            let mut shared = self.shared.borrow_mut();
            shared.shown_tabs = shown_tabs;
            shared.shown_strip = shown_strip;
            shared.focused_presence = focused_presence;
        }
        self.refresh_presentation();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.draw_leaving_strip(scene, transform, text, theme, presence);
        self.draw_display_strips(scene, transform, text, theme, presence);
        // What the strips just drew of their floated windows whole over the windows under them:
        // the compositor draws those around it rather than under it, so nothing reads through its
        // bar or its body and its lift shows against them.
        let float_covers: Vec<(TabId, Rect)> = self
            .drawn_strips()
            .into_iter()
            .filter_map(|strip| self.rows.get(&strip))
            .flat_map(|row| row.drawn_covers().iter().copied())
            .collect();
        {
            let mut shared = self.shared.borrow_mut();
            if shared.float_covers != float_covers {
                shared.float_covers = float_covers;
            }
        }
        let hint_places = self.shown_row().map(|row| {
            ui::widget::draw_child(row, scene, transform, text, theme, presence);
            row.hint_places()
        });
        self.shared.borrow_mut().hint_places = hint_places.unwrap_or_default();
        self.draw_embeds(scene, transform, text, theme, presence);
        self.draw_overlays(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Where the pointer is, read before anything answers the event: the strip asks it which
        // window a press landed in.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let Some(layer) = self.notices.as_mut()
            && layer.route(event, ctx.scale_factor)
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let now = ui::time::now();
            self.end_idle_swipe(now);
            // A wheel held with the zoom modifier zooms the window under the pointer: a browser
            // sends a trackpad's pinch as one, and a mouse wheel with the command modifier
            // natively. It never moves the strip, however much sideways motion the pinch carries.
            if !ui::content_zoom::is_zoom_scroll(event, ctx.modifiers)
                && let Some(travel) = self.strip_travel(*delta, ctx.scale_factor)
                && self.route_strip_swipe(travel, now)
            {
                *ctx.handled = true;
                return;
            }
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if let [.., _, hung, _] = children[..] {
            let path = ctx.path.clone();
            self.hung_apps
                .handle_event(event, &mut ctx.reborrow(&path, hung));
            if *ctx.handled {
                self.drain_liveness_choices();
                return;
            }
        }
        if let [.., arrivals, _, _] = children[..] {
            let path = ctx.path.clone();
            self.arrival_hints
                .handle_event(event, &mut ctx.reborrow(&path, arrivals));
            if *ctx.handled {
                self.reveal_chosen_arrival();
                return;
            }
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.focus_pressed_display();
            self.focus_clicked_group();
            self.reveal_on_release = self.shown.and_then(|strip| {
                let group = self.rows.get(&strip)?.group_under_cursor()?;
                Some((strip, group))
            });
        }
        self.route_pointer(event, ctx, &children);
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
            && let Some((strip, group)) = self.reveal_on_release.take()
            && self.shown == Some(strip)
            && self
                .shown_row_ref()
                .is_some_and(|row| !row.groups.iter().any(|g| g.maximized))
        {
            // A window clicked while partly off screen comes into view, as focusing it from the
            // keyboard brings it.
            self.reveal = Some((strip, group));
            self.relayout = true;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && !ui::content_zoom::is_zoom_scroll(event, ctx.modifiers)
        {
            // The window under the pointer and its app had the event; a node above the strip that
            // took it is no strip pan either.
            let travel = self.strip_travel(*delta, ctx.scale_factor);
            if let Some(travel) = travel
                && self.take_sideways(travel, *ctx.handled, ui::time::now())
            {
                *ctx.handled = true;
            }
        }
    }

    /// A sideways pan of the shown strip moves windows and changes nothing
    /// else: no document, focus, selection or declaration. Every other wheel
    /// reaches a layer above the strip (a banner or the notification centre
    /// shown, search) or the window under the pointer,
    /// which answer for themselves.
    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        self.notices.as_ref().is_none_or(|layer| !layer.visible())
            && !self.search.borrow().is_open()
            && self.pans_strip(*delta, ui::time::now())
    }

    /// The app of the window brought forward, once its tab is built as an app: the host then gives
    /// it keyboard focus.
    fn take_app_activation(&mut self) -> Option<ui::application::InstanceId> {
        let (strip, tab) = self.activation?;
        if self.keyboard_tab != Some(tab) {
            self.activation = None;
            return None;
        }
        let instance = self
            .rows
            .get_mut(&strip)?
            .groups
            .iter_mut()
            .find_map(|group| group.host_for_tab_mut(tab))?
            .keyboard_input_scope()?;
        self.activation = None;
        Some(instance)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.shown
            .and_then(|strip| self.rows.get(&strip))
            .is_some_and(Widget::wants_pointer_capture)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let notices = self.notices.as_ref().map(|l| l as &dyn Widget);
        let rows: Vec<&dyn Widget> = displays::drawn_rows(&self.rows, &self.drawn_strips())
            .into_iter()
            .map(|row| row as &dyn Widget)
            .collect();
        rows.into_iter()
            .chain(notices)
            .chain([
                &self.arrival_hints as &dyn Widget,
                &self.hung_apps as &dyn Widget,
                &self.acknowledgement as &dyn Widget,
            ])
            .collect()
    }

    fn visit_retained_components(
        &mut self,
        request: &mut ui::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), ui::bound_dispatch::BoundDispatchError> {
        for row in self.rows.values_mut() {
            request.visit(row)?;
        }
        if let Some(notices) = &mut self.notices {
            request.visit(notices)?;
        }
        Ok(())
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let strips = self.drawn_strips();
        let rows: Vec<&mut (dyn Widget + '_)> = displays::drawn_rows_mut(&mut self.rows, &strips)
            .into_iter()
            .map(|row| row as &mut (dyn Widget + '_))
            .collect();
        let notices = self.notices.as_mut().map(|l| l as &mut (dyn Widget + '_));
        let arrivals = &mut self.arrival_hints as &mut (dyn Widget + '_);
        let hung = &mut self.hung_apps as &mut (dyn Widget + '_);
        let acknowledgement = &mut self.acknowledgement as &mut (dyn Widget + '_);
        rows.into_iter()
            .chain(notices)
            .chain([arrivals, hung, acknowledgement])
            .collect()
    }

    fn set_platform_conventions(&mut self, conventions: ui::platform::PlatformConventions) {
        self.platform = conventions;
        for child in self.children_mut() {
            child.set_platform_conventions(conventions);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        let shown = self.shown;
        self.activity_loading
            || self.slides.borrow().is_moving(ui::motion::frame_now())
            || self.embeds_animate()
            || self.unbuilt.iter().any(|&(strip, _)| Some(strip) == shown)
            || self
                .children()
                .into_iter()
                .any(Widget::needs_animation_frame)
    }

    /// A pan's end, when the strip settles and focus goes to the window panned to.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let ends = self
            .swipe
            .filter(|swipe| {
                swipe.hand == paging::Hand::Wheel && (swipe.panned || swipe.pan.is_some())
            })
            .and_then(|swipe| swipe.last.checked_add(SWIPE_GAP));
        // An app's answer to the search query is still coming: look again shortly, as its rows show
        // once it is in.
        let answer = self
            .search
            .borrow()
            .waiting()
            .then(|| ui::time::now().checked_add(SEARCH_ANSWER_POLL))
            .flatten();
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(ends)
            .chain(answer)
            .chain(self.embed_retry_deadline())
            .min()
    }

    /// A queued command or a request the host made is work to draw: it asks for the frame that
    /// applies it rather than waiting for an unrelated one, so a host that only draws what asks
    /// needs no separate schedule. What the host above the windows did with the search palette
    /// since the last pass is work for this one: a row it chose, a scope it widened, a query field
    /// it typed into. So is a tree replaced from outside — a workspace's desktop its members
    /// changed: the desk's rows are only brought to it by a pass, and none would come for it
    /// otherwise.
    fn needs_redraw(&self) -> bool {
        let work = {
            let shared = self.shared.borrow();
            shared.search_request.is_some()
                || !shared.queue.is_empty()
                || shared.search_revision != self.seen_search
                || shared.replaced
                // An app that arrived is built by a pass (`Self::build_deferred`), so it asks for
                // the frame that takes its component rather than waiting for an unrelated one.
                || !shared.rebuild.is_empty()
        };
        // The desk asks its search sources a frame at a time, and cuts its rows to the card once
        // the card gives its room: a search with such work left draws the frames that do it, so it
        // ends with every source's rows rather than stalling part-way.
        work || self.search.borrow().filling_in()
            || self.slides.borrow().is_moving(ui::motion::frame_now())
            || self.children().into_iter().any(Widget::needs_redraw)
    }

    fn has_pending_visual_work(&self) -> bool {
        let shown = self.shown;
        self.unbuilt.iter().any(|&(strip, _)| Some(strip) == shown)
            || self.slides.borrow().is_moving(ui::motion::frame_now())
            || !self.shared.borrow().queue.is_empty()
            || self.shared.borrow().replaced
            || !self.shared.borrow().rebuild.is_empty()
            || self
                .children()
                .into_iter()
                .any(Widget::has_pending_visual_work)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }

    /// A change the shown strip's windows carry out in place stays in place; the surface's own
    /// changes rebuild.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        use ui::widget::StructureChange;
        let own = StructureChange::of(std::mem::take(&mut self.structure_dirty));
        let relayout = if std::mem::take(&mut self.relayout) {
            StructureChange::Children
        } else {
            StructureChange::None
        };
        let row = self
            .shown_row()
            .map_or(StructureChange::None, Widget::take_structure_change);
        own.join(relayout).join(row)
    }

    /// The shown strip lays out again in the nodes it keeps, under the gutter the surface leaves
    /// below it.
    fn remount_children(&mut self, remount: &mut ui::widget::Remount<'_>) -> bool {
        if !self
            .children_mut()
            .into_iter()
            .all(|child| child.remount_children(remount))
        {
            return false;
        }
        if let Some(row) = self.shown_row_ref()
            && let Some(node) = row.layout_node()
        {
            let style = strip_style(
                row,
                remount.taffy().style(node).cloned().unwrap_or_default(),
            );
            if remount.taffy().style(node).ok() != Some(&style) {
                let _ = remount.taffy().set_style(node, style);
            }
        }
        true
    }
}

impl DesktopSurface {
    /// The arrival hints, the cards of apps that stopped answering, the banners and the centre over
    /// everything the desk draws.
    fn draw_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.arrival_hints
            .draw(scene, transform, text, theme, presence);
        self.hung_apps.draw(scene, transform, text, theme, presence);
        if let Some(layer) = self.notices.as_mut() {
            layer.draw(scene, transform, text, theme, presence);
        }
        self.acknowledgement
            .draw(scene, transform, text, theme, presence);
    }
}

mod lifecycle;

#[cfg(test)]
mod activity_tests;
#[cfg(test)]
mod tests;
