//! The desktop as a widget: the active workspace's active strip presented
//! as the toolkit's [`GroupRow`] of [`WindowGroup`]s, with the model as the
//! source of truth.
//!
//! Local changes flow both ways without diffing. Gestures inside the row
//! (tab drags, splits, closes, activation, resize, maximize, the new-tab
//! control) are harvested into the tree on the next update pass, exactly as
//! the shell's overlay host harvests them. Navigation commands are admitted
//! against the tree when dispatched, then applied to the tree and to the
//! live widgets in the same update pass. Rows are built from the tree the
//! first time their strip is shown and kept alive afterwards, so a tab's
//! application survives switching strips or workspaces.
//!
//! A tree changed elsewhere (a peer, a restored copy) replaces the model
//! through [`DesktopHandle::replace_tree`]; the next update pass rebuilds the
//! rows from it by tab id, moving each surviving tab's live content into its
//! new place and closing the applications of tabs that are gone.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_sort::CompactSort as _;

use tilo_desktop_model::{
    DesktopTree, GroupId, GroupShape, IdMinter, StripId, Tab, TabId, WindowWidth, Workspace,
    WorkspaceId,
};
use ui::automerge::{AutoCommit, ObjId};
use ui::collaboration::PresencePayload;
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceKey;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::time::{Duration, Instant};
use ui::vello::Scene;
use ui::widget::{
    GroupRow, OutputSpan, STRIP_BOTTOM_GUTTER_PX, STRIP_EDGE_GUTTER_PX, TabDescriptor,
    TaffyTreeExt as _, TileContent, TileHost, Widget, WindowGroup,
};
use ui::winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use ui::{TextRenderer, Theme};

use crate::navigation::{
    self, ActivityView, Admitted, Application, Command, DesktopPan, HistoryDirection, HistoryState,
    NotificationsState, Presentation, RouteIds, ScrollAction, ScrollState, Snapshot,
};
use crate::session::{ActivityAsk, ActivityReads, ActivityRequests};

mod arrivals;
#[cfg(test)]
mod close_motion_tests;
mod history;
mod live_shares;
mod notices;
#[cfg(test)]
mod open_motion_tests;
mod personal_ask;
mod search;
mod splice;
mod window_bars;

pub use history::{ActivityModel, ActivityPaneCode, HistorySource, OpenActivity, open_activity};
pub use notices::{AppInbox, AppNotifier, DesktopNotifications, NotificationOutcome};
pub use personal_ask::PersonalAsk;
pub use search::{HostAction, HostActionSource, is_host_offer, search_chord_keys};

/// The caret or selection a surface publishes as its presence: the
/// keyboard-focused widget's when it is one of the focused window's (or
/// application's) editors `window`, else that window's first editor caret.
/// Keyboard focus can stay behind in another window, an action can move a
/// caret without focus, and a hidden mount never holds keyboard focus.
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
    /// The content for `tab` in `workspace`, built the first time the tab's
    /// strip is shown.
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost;

    /// A launch command named `app`: the tab to open under `id`. `None`
    /// opens nothing.
    fn launch(&mut self, _workspace: &Workspace, _id: TabId, _app: &str) -> Option<Tab> {
        None
    }

    /// The applications desktop search offers to open, each an id
    /// [`Self::launch`] accepts, followed by those this desktop cannot run
    /// (`unavailable` set), which search lists with their reason.
    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        Vec::new()
    }

    /// The tab already showing what the launch `app` opens (a record
    /// launch's record, [`crate::session::record_launch`]): it comes
    /// forward in place of a second window on the same record.
    fn showing(&self, _app: &str) -> Option<TabId> {
        None
    }

    /// The record `tab`'s window shows and the kind of resource it keeps,
    /// for the windows a person may share ([`ui::window_share`]).
    fn window_record(&self, _tab: TabId) -> Option<(String, String)> {
        None
    }

    /// The workspace the windows are in, which each window's app is told:
    /// apps sharing one process have none of their own.
    fn workspace_room(&self) -> Option<String> {
        None
    }

    /// Whether this desktop draws the views windows embed of other apps'
    /// resources ([`Self::embed`]).
    fn draws_embeds(&self) -> bool {
        false
    }

    /// `view`'s own app on this desktop's copy of its record, to be drawn
    /// scaled into the window that embeds it: the app, or why it cannot
    /// show. Never creates the record.
    fn embed(&mut self, _view: &ui::window_share::ResourceView) -> Result<TileHost, String> {
        Err("This desktop can't show it here".into())
    }
}

struct Shared {
    tree: DesktopTree,
    /// Whoever holds each window someone else opened, by tab
    /// ([`DesktopHandle::set_holders`]).
    holders: HashMap<TabId, ui::widget::TabHolder>,
    /// Its person opens windows and tabs here ([`DesktopHandle::set_opens`]):
    /// the windows offer a new tab.
    opens: bool,
    /// Bumped on every tree change, so a host republishes navigation only
    /// when something moved.
    revision: u64,
    queue: Vec<Admitted>,
    /// What the Activity app was last asked to show (a view, and whose
    /// doings when one person's), until the activity model is up to offer
    /// it.
    activity_focus: Option<(ActivityView, Option<String>)>,
    minter: IdMinter,
    /// Logical width the strip lays out against.
    viewport_width: f64,
    /// The tree was replaced from outside; rows must be reconciled.
    replaced: bool,
    /// The content bounds of each window's active tab in the shown strip,
    /// as last laid out.
    shown_tabs: Vec<(TabId, Rect)>,
    /// The shown strip, the area it is drawn in and how far it is
    /// panned, as last laid out.
    shown_strip: Option<(StripId, Rect, f64)>,
    /// Where the shown strip's controls are for control hints, as last
    /// drawn.
    hint_places: ui::widget::HintPlaces,
    /// Carets and selections of the focused window's editors, as last laid
    /// out ([`ui::presence::capability::subtree_presence`]).
    focused_presence: Vec<(PresenceKey, PresencePayload)>,
    /// Tabs whose content the host asked to build again.
    rebuild: Vec<TabId>,
    search_request: Option<search::SearchRequest>,
    search_open: bool,
    /// What the host offers to do for the person, and what they chose.
    host_actions: Vec<search::HostAction>,
    host_choices: Vec<String>,
    /// The ask about the person themselves the bar shows from their avatar.
    personal_ask: Option<personal_ask::PersonalAsk>,
    personal_ask_revision: u64,
    /// Windows someone else opened lately, for the edge hint.
    arrivals: Vec<arrivals::Arrival>,
    /// Pan, scroll, history and activity state, as last presented.
    presentation: Presentation,
    /// Bumped whenever `presentation` changes. Unlike `revision` it never
    /// asks a host to save or share the tree.
    presented: u64,
    /// Where the host confirms in passing what left nothing to see.
    acknowledger: ui::widget::acknowledgement::Acknowledger,
}

impl Shared {
    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn present(&mut self, presentation: Presentation) {
        if self.presentation != presentation {
            self.presentation = presentation;
            self.presented = self.presented.wrapping_add(1);
        }
    }
}

/// The host's side of a mounted [`DesktopSurface`]: read the tree, publish
/// navigation, queue commands. Cheap to clone; single-threaded.
#[derive(Clone)]
pub struct DesktopHandle {
    shared: Rc<RefCell<Shared>>,
}

impl DesktopHandle {
    /// Confirm in passing an action that left nothing to see ("Link
    /// copied"): a short-lived pill, said politely to screen readers,
    /// never a notification. The host asks for a redraw.
    pub fn acknowledge(&self, text: impl Into<String>) {
        self.shared.borrow().acknowledger.say(text);
    }

    pub fn revision(&self) -> u64 {
        self.shared.borrow().revision
    }

    /// Changes whenever the navigation snapshot may: the tree or what the
    /// surface presents beside it (pan, scroll, history, activity).
    pub fn navigation_revision(&self) -> u64 {
        let shared = self.shared.borrow();
        shared.revision.wrapping_add(shared.presented)
    }

    pub fn read<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R {
        read(&self.shared.borrow().tree)
    }

    /// Where each window of the shown strip draws its active tab, in the
    /// surface's coordinates, as of the last layout. Hidden tabs and other
    /// strips are absent.
    pub fn shown_tabs(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.clone()
    }

    /// The shown strip, the area it is drawn in (surface coordinates) and
    /// how far its content is panned past that area's left edge, as of the
    /// last layout: a point `x` over it is `x - area.x0 + pan` along the
    /// strip, however wide the surface or wherever it is panned to.
    pub fn shown_strip(&self) -> Option<(StripId, Rect, f64)> {
        self.shared.borrow().shown_strip
    }

    /// Where the shown strip's window bars and edges are for control hints,
    /// in the surface's coordinates, as last drawn.
    pub fn hint_places(&self) -> ui::widget::HintPlaces {
        self.shared.borrow().hint_places.clone()
    }

    /// The caret or selection this desktop publishes as its presence, given
    /// the keyboard-focused widget's (`keyboard`): see
    /// [`published_selection`], with the focused window's editors as of the
    /// last layout.
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
    pub fn dispatch(&self, command: &Command, generation: &str) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        let capabilities = shared.presentation.capabilities();
        let admitted = navigation::admit_with(command, generation, &shared.tree, &capabilities)?;
        let presentation = &shared.presentation;
        match (&admitted, command) {
            (Admitted::Pan(strip, _), _)
                if presentation
                    .pan
                    .as_ref()
                    .is_none_or(|pan| pan.strip != strip.0.to_string()) =>
            {
                return Err("Desktop pan unavailable".into());
            }
            (Admitted::Scroll(..), Command::Scroll { region, .. })
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
            // A window someone else holds is theirs (or a moderator's) to
            // close: refused here, said at once, and nothing moves.
            (Admitted::Close(ids), _) if shared.holders.contains_key(&ids.tab) => {
                let refusal = holder_refusal(&shared.holders, ids.tab);
                shared.acknowledger.say(refusal.clone());
                return Err(refusal);
            }
            _ => {}
        }
        shared.queue.push(admitted);
        Ok(())
    }

    /// The logical width of the surface, which window widths are fractions
    /// of. Hosts call this on every resize.
    pub fn set_viewport_width(&self, width: f64) {
        self.shared.borrow_mut().viewport_width = width.max(1.0);
    }

    /// On a desktop several people share: who holds each window this
    /// desktop's person may not close or move, by tab. Those windows offer
    /// no close, resist a drag and name their holder on hover; every other
    /// window behaves as it always does. Applied to the windows on screen
    /// in the next update pass, and to every window built after. Whether
    /// that changed anything.
    /// Whether this desktop's person opens windows and tabs here: someone
    /// the room lets only look (a spectator) is offered no new tab. Whether
    /// that changed anything.
    pub fn set_opens(&self, opens: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let changed = shared.opens != opens;
        shared.opens = opens;
        changed
    }

    pub fn set_holders(&self, holders: HashMap<TabId, ui::widget::TabHolder>) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.holders == holders {
            return false;
        }
        shared.holders = holders;
        true
    }

    /// Replace the tree with one changed elsewhere. The next update pass
    /// reconciles the live rows with it: tabs still present keep their
    /// running content, new tabs get content from the host's
    /// [`TileSource`], and removed tabs' applications close. Queued commands
    /// stay queued and are re-checked against the new tree when applied.
    pub fn replace_tree(&self, tree: DesktopTree) {
        let mut shared = self.shared.borrow_mut();
        observe_ids(&mut shared.minter, &tree);
        shared.tree = tree;
        shared.replaced = true;
        shared.changed();
    }

    /// The room snapshot, excluding process-local stream proxies and
    /// device presentation. Durable profile saves still use the full tree.
    pub fn shared_tree(&self) -> DesktopTree {
        self.read(|tree| crate::shared_tree::projection(tree.clone()))
    }

    /// Apply room topology without replacing this device's stream bindings
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

    /// Ask the [`TileSource`] for `tab`'s content again on a coming update
    /// pass, as when the tab was first shown: for a host whose first answer
    /// was a notice (its record had not arrived). The host schedules a frame.
    pub fn rebuild(&self, tab: TabId) {
        let mut shared = self.shared.borrow_mut();
        if !shared.rebuild.contains(&tab) {
            shared.rebuild.push(tab);
        }
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
    /// room's policy ([`tilo_desktop_model::commands::may_command`]) first.
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
            .find(|workspace| workspace.id == active)
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

    /// Focus the window showing `tab`, on the next update pass. False when
    /// no window shows it.
    pub fn focus_tab(&self, tab: TabId) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = route_of(&shared.tree, tab) else {
            return false;
        };
        shared.queue.push(Admitted::Select(ids));
        true
    }

    /// Focus a window of application `app_id` in the active workspace (the
    /// focused one when it is that application's), on the next update pass.
    /// False when the workspace has none.
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
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
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

/// Answer what the windows asked of the activity with a host that keeps no
/// history for the workspace: each write that it isn't kept, each read
/// that it is still to come, as a native session reads a workspace it is
/// not showing.
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
                    workspace: workspace.id,
                    strip: strip.id,
                    group: group.id,
                    tab,
                })
            })
        })
    })
}

/// A widget-side follow-up to a command already applied to the tree.
enum RowOp {
    Show(StripId, GroupId, TabId),
    Width(StripId, GroupId, WindowWidth),
    ToggleMaximized(StripId, GroupId),
    /// A launched window, already in the tree at `index` of `strip`.
    Open {
        strip: StripId,
        group: GroupId,
        index: usize,
        tab: Tab,
    },
    Close {
        strip: StripId,
        group: GroupId,
        tab: TabId,
        group_removed: bool,
    },
    Pan(StripId, f64),
    Scroll(StripId, GroupId, ScrollAction),
    History(StripId, GroupId, TabId, HistoryDirection),
}

pub struct DesktopSurface {
    shared: Rc<RefCell<Shared>>,
    tiles: Box<dyn TileSource>,
    rows: HashMap<StripId, GroupRow>,
    /// The slots the apps put in their windows' bars ([`window_bars`]).
    window_slots: ui::window_slots::WindowSlots,
    shown: Option<StripId>,
    rect: Rect,
    structure_dirty: bool,
    close_reports: Vec<ui::application::ComponentCloseReport>,
    /// A launched window to pan into view once it has been laid out.
    reveal: Option<(StripId, GroupId)>,
    /// Tabs of a shown strip whose content is not built yet, in build
    /// order. Only the focused window is built before the strip's first
    /// frame; the rest follow within a per-frame budget.
    unbuilt: Vec<(StripId, TabId)>,
    /// The sideways swipe in progress, if any.
    swipe: Option<Swipe>,
    /// The window a pan ended on. Focus goes to it where the strip is,
    /// without panning it into view: the strip stays where the person left
    /// it, however little a lone wheel notch moved it.
    settle: Option<GroupId>,
    search: tilo_desktop_search::DesktopSearch,
    /// The window a group's new-tab control opened search for: the
    /// application chosen there opens as its tab, as in the native shell.
    new_tab_into: Option<(StripId, GroupId)>,
    /// The window search was opened over (its in-app search): an action
    /// chosen there runs inside that window's app.
    search_window: Option<(StripId, GroupId)>,
    /// For search over everything, the window publishing each resource it
    /// lists, by resource id: an app's action chosen there runs inside the
    /// app's window, which comes forward.
    search_owners: HashMap<String, (StripId, GroupId)>,
    /// An action chosen over a window that hosts no app to run it, left for
    /// the host's own door, which says why it did not run.
    unrouted_control: Option<ui::tilo_actions::ControlInvocation>,
    /// The pointer in logical coordinates, while search is open.
    search_pointer: Point,
    platform: ui::platform::PlatformConventions,
    /// The host's retained workspace history, when it keeps one.
    history: Option<Box<dyn HistorySource>>,
    /// What the windows ask of the workspace's activity, when the host
    /// serves it to them.
    activity_requests: Option<ActivityRequests>,
    /// The activity model, while a window follows it or asks of it.
    activity: Option<Box<dyn history::Served>>,
    /// The model was asked for while the host still loads its code: frames
    /// keep coming, each asking again, until it can open.
    activity_loading: bool,
    /// The banners and centre, when the host installed notifications.
    notices: Option<notices::NotificationLayer>,
    /// What an action that leaves nothing to see confirms in passing
    /// ("Link copied"), over everything else.
    acknowledgement: ui::widget::acknowledgement::AcknowledgementLayer,
    /// "Kim opened Notes": windows others opened out of sight.
    arrival_hints: arrivals::ArrivalHints,
    /// Where the host collects what controls emit, once it has bound this
    /// surface. A tab built after that (a launch, a record that arrived, a
    /// rebuilt row) is bound to it as it is built, or its registered
    /// buttons would emit into a queue nobody drains.
    controls: Option<ui::ControlDomain>,
    /// The tab the keyboard was last aimed at: the focused window's shown
    /// tab, as of the last presentation.
    keyboard_tab: Option<TabId>,
    /// A tab brought forward whose app the host has not activated yet
    /// ([`Widget::take_app_activation`]): it waits while the tab's app is
    /// still loading, so a window opened before its app arrived still
    /// takes the keyboard once it does.
    activation: Option<(StripId, TabId)>,
    /// The window a press landed in. When the press ends the strip pans
    /// it wholly into view: never during the press, which would move what
    /// was pressed from under the pointer.
    reveal_on_release: Option<(StripId, GroupId)>,
    /// The windows shared live, and the views windows embed of them.
    live_shares: live_shares::LiveShares,
}

/// What one app may ask of its host in one frame; more waits for the next.
const MAX_HOST_INTENTS: usize = 8;

/// Time the update pass may spend building deferred tabs in one frame. At
/// least one tab is built per frame, however long it takes.
const BUILD_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// A pause in wheel input this long ends a swipe. A trackpad keeps
/// reporting while the fingers move and through the momentum after they
/// lift, and a spun mouse wheel's notches come 0 to 50 ms apart, all well
/// inside this; the pauses between spins are mostly longer.
const SWIPE_GAP: Duration = Duration::from_millis(200);
/// How soon search looks again for an app's answer that is still coming.
const SEARCH_ANSWER_POLL: Duration = Duration::from_millis(50);

/// Logical pixels a line of sideways wheel travels: the 40 px to the line a
/// trackpad's pixels are weighed against when telling sideways from
/// vertical, so both kinds of input move the strip alike.
const WHEEL_LINE_PX: f64 = 40.0;

/// Sideways travel that steps focus one window on a strip that cannot pan
/// that way. About the width of a window: a Normal one is two thirds of a
/// 1280 px strip, so a spin steps about as many windows on a strip that fits
/// as it pans past on one that overflows. A notch of the native wheel (a
/// line) is a sixteenth of it, a spin of about twenty notches a window or
/// two.
const STEP_TRAVEL_PX: f64 = 640.0;

/// One horizontally dominant wheel gesture, from its first event until
/// wheel input pauses for [`SWIPE_GAP`]: a trackpad swipe and its momentum,
/// or a spin of a mouse's sideways wheel.
#[derive(Clone, Copy)]
struct Swipe {
    /// The strip took the gesture, so the rest of it is the strip's, ends
    /// included; else it belongs to the content under the pointer.
    taken: bool,
    /// Travel since the last step, toward the next one, while the strip
    /// cannot pan the way the gesture goes.
    travel: f64,
    /// The gesture panned the strip, so its end focuses the window it was
    /// panned to.
    panned: bool,
    /// Where the gesture has asked the strip to be since it took hold of
    /// it ([`GroupRow::hold_strip_pull`]), past either end included, so
    /// there it pulls against the rubber band and its end lets the strip
    /// settle.
    pan: Option<f64>,
    last: Instant,
}

/// A wheel event's sideways travel in logical pixels when horizontal motion
/// dominates, positive forward (toward later windows, like Super+Right).
/// Winit's deltas are negative for a wheel turned (or fingers scrolling)
/// rightward, which scrolls content toward its later columns and so moves
/// forward; it reports pixels physical. The shell's overlay host classifies
/// wheel input the same way.
fn sideways(delta: MouseScrollDelta, scale_factor: f64) -> Option<f64> {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            (f64::from(x) * WHEEL_LINE_PX, f64::from(y) * WHEEL_LINE_PX)
        }
        MouseScrollDelta::PixelDelta(p) => {
            let scale = scale_factor.max(f64::EPSILON);
            (p.x / scale, p.y / scale)
        }
    };
    (dx.abs() > dy.abs()).then_some(-dx)
}

/// A wheel event's vertical travel in logical pixels, positive for a wheel
/// turned (or fingers scrolling) downward, which winit reports negative.
fn vertical(delta: MouseScrollDelta, scale_factor: f64) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * WHEEL_LINE_PX,
        MouseScrollDelta::PixelDelta(p) => -p.y / scale_factor.max(f64::EPSILON),
    }
}

/// The window a pan left the person looking at: the focused one while it is
/// still wholly in view, else the one showing the most of itself, the one
/// nearer the middle of the view between two showing as much.
fn panned_to(row: &GroupRow, focused: Option<GroupId>) -> Option<GroupId> {
    let (left, right) = row.visible_canvas_span();
    let middle = f64::midpoint(left, right);
    let shown = |x0: f64, x1: f64| (x1.min(right) - x0.max(left)).max(0.0);
    let spans = row.group_canvas_spans();
    if spans
        .iter()
        .any(|&(id, x0, x1)| Some(id) == focused && shown(x0, x1) >= x1 - x0 - 0.5)
    {
        return focused;
    }
    spans
        .iter()
        .filter(|&&(_, x0, x1)| shown(x0, x1) > 0.0)
        .min_by(|&&(_, a0, a1), &&(_, b0, b1)| {
            let (a, b) = (shown(a0, a1), shown(b0, b1));
            if (a - b).abs() > 0.5 {
                b.total_cmp(&a)
            } else {
                let off = |x0: f64, x1: f64| (f64::midpoint(x0, x1) - middle).abs();
                off(a0, a1).total_cmp(&off(b0, b1))
            }
        })
        .map(|&(id, _, _)| id)
}

impl DesktopSurface {
    /// Present `tree`. `salt` is this device's peer salt for new ids.
    pub fn new(tree: DesktopTree, salt: u16, tiles: Box<dyn TileSource>) -> (Self, DesktopHandle) {
        let mut minter = IdMinter::new(salt);
        observe_ids(&mut minter, &tree);
        let shared = Rc::new(RefCell::new(Shared {
            tree,
            holders: HashMap::new(),
            opens: true,
            revision: 1,
            queue: Vec::new(),
            activity_focus: None,
            minter,
            viewport_width: ui::widget::DEFAULT_STRIP_USABLE_PX,
            replaced: false,
            shown_tabs: Vec::new(),
            shown_strip: None,
            hint_places: ui::widget::HintPlaces::default(),
            focused_presence: Vec::new(),
            rebuild: Vec::new(),
            search_request: None,
            search_open: false,
            host_actions: Vec::new(),
            host_choices: Vec::new(),
            personal_ask: None,
            personal_ask_revision: 0,
            arrivals: Vec::new(),
            presentation: Presentation::default(),
            presented: 0,
            acknowledger: ui::widget::acknowledgement::Acknowledger::default(),
        }));
        let acknowledger = shared.borrow().acknowledger.clone();
        let mut surface = Self {
            shared: Rc::clone(&shared),
            tiles,
            rows: HashMap::new(),
            window_slots: ui::window_slots::WindowSlots::default(),
            shown: None,
            rect: Rect::ZERO,
            structure_dirty: true,
            close_reports: Vec::new(),
            reveal: None,
            unbuilt: Vec::new(),
            swipe: None,
            settle: None,
            search: tilo_desktop_search::DesktopSearch::new(),
            new_tab_into: None,
            search_window: None,
            search_owners: HashMap::new(),
            unrouted_control: None,
            search_pointer: Point::ZERO,
            platform: ui::platform::PlatformConventions::default(),
            history: None,
            activity_requests: None,
            activity: None,
            activity_loading: false,
            notices: None,
            acknowledgement: ui::widget::acknowledgement::AcknowledgementLayer::new(acknowledger),
            arrival_hints: arrivals::ArrivalHints::default(),
            controls: None,
            keyboard_tab: None,
            activation: None,
            reveal_on_release: None,
            live_shares: live_shares::LiveShares::default(),
        };
        surface.show_active_strip();
        (surface, DesktopHandle { shared })
    }

    /// Give every live window the holders the host named
    /// ([`DesktopHandle::set_holders`]). Whether that changed what one
    /// offers.
    fn apply_holders(&mut self) -> bool {
        let shared = self.shared.borrow();
        let mut changed = false;
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
                changed |= group.set_new_tab(shared.opens);
                for tab in tabs {
                    changed |= group.set_tab_holder(tab, shared.holders.get(&tab).cloned());
                }
            }
        }
        changed
    }

    /// Read workspace history from `source`: the Activity app then shows
    /// it, once the host serves the windows what they ask of it
    /// ([`Self::serve_activity`]).
    pub fn set_history_source(&mut self, source: Box<dyn HistorySource>) {
        self.history = Some(source);
        let mut shared = self.shared.borrow_mut();
        let mut next = shared.presentation.clone();
        next.retained_history = true;
        shared.present(next);
    }

    /// Answer what the windows ask of the workspace's activity through
    /// `requests` (the desktop's end of its session) from the history
    /// source.
    pub fn serve_activity(&mut self, requests: ActivityRequests) {
        self.activity_requests = Some(requests);
    }

    /// Show `notifications`' banners over the strip, and its centre when a
    /// `notification-center` command opens it.
    pub fn set_notifications(&mut self, notifications: DesktopNotifications) {
        self.notices = Some(notices::NotificationLayer::new(notifications));
        self.structure_dirty = true;
        self.refresh_notifications();
    }

    /// Show applications' notifications from `inbox` (the other half of
    /// the [`AppNotifier`] the host's services hand them), attributed to the
    /// window that posted each. Choosing one focuses that application's
    /// window and hands the application its activation.
    pub fn set_app_notifications(&mut self, inbox: AppInbox) {
        if let Some(layer) = self.notices.as_mut() {
            layer.inbox = Some(inbox);
        }
    }

    /// Open or close the notification centre, as the host's own control
    /// does.
    pub fn set_notification_center(&mut self, open: bool) {
        if let Some(layer) = self.notices.as_mut() {
            layer.set_center_open(open);
            self.refresh_notifications();
        }
    }

    /// Retire banners that ran out, open what the centre asked to, and
    /// record what the notifications show.
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
                        Some((app_id.clone(), label.clone(), Some(workspace.id.raw())))
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
            self.shared.borrow_mut().queue.push(Admitted::Launch(app));
        }
        let mut shared = self.shared.borrow_mut();
        let mut next = shared.presentation.clone();
        next.notifications = Some(state);
        shared.present(next);
    }

    /// Serve the windows what they asked of the workspace's activity: the
    /// activity model is open while one follows it or asks of it, for the
    /// active workspace, and each read it follows is read again whenever
    /// the model moved.
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

    /// Build (once) and show the active workspace's active strip.
    fn show_active_strip(&mut self) {
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
            let (row, deferred) = build_row_lazily(
                self.tiles.as_mut(),
                self.controls.as_ref(),
                workspace,
                strip,
                focused,
            );
            // The focused window, built first, is also the one in view.
            if self.reveal.is_none() {
                self.reveal = focused.map(|group| (strip_id, group));
            }
            self.unbuilt
                .extend(deferred.into_iter().map(|tab| (strip_id, tab)));
            self.rows.insert(strip_id, row);
        }
        if self.shown != strip_id {
            self.shown = strip_id;
            self.structure_dirty = true;
        }
    }

    /// Build deferred tabs of the shown strip until the frame's budget is
    /// spent. Tabs of strips not shown wait until theirs is.
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
            // A window that held an app's place (its code or record still
            // coming) hands its arrival, and its place in the layout, to
            // what is built there now: the window says how its layout
            // changes, so the rest of the desktop's is kept.
            host.take_place_of(rebuilt);
            if ui::time::now().duration_since(started) >= BUILD_BUDGET {
                break;
            }
        }
        waiting.extend(queue);
        drop(shared);
        self.unbuilt = waiting;
    }

    /// Bring the rows to a replaced tree, keeping live content by tab id.
    /// A strip already shown keeps its row and changes in place
    /// ([`splice::splice_row`]): its pan stays where the person left it, and
    /// windows removed, added or moved elsewhere slide as they do when the
    /// person closes, opens or moves one here. Rebuilding the row instead
    /// threw its pan away, so each peer's edit, or the room's echo of a
    /// close, flew the strip home and cut every window to its new place.
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
        // Tabs leaving their window, or the desktop, give up their content
        // first, so a window they move to can take it over.
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
                // Strips with surviving content are built now so that
                // content is not dropped; the rest are built when first
                // shown.
                let survives = strip
                    .window_groups
                    .iter()
                    .flat_map(|group| &group.tabs)
                    .any(|tab| live.contains_key(&tab.id));
                if survives {
                    let row = build_row(
                        self.tiles.as_mut(),
                        self.controls.as_ref(),
                        workspace,
                        strip,
                        &mut live,
                    );
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
        // A tab still waiting for its content waits where it stayed; one
        // that moved was built with its new window.
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

    /// A press anywhere in a window, its body included and not only its tab
    /// bar, makes that window the active one. It stays where it is while
    /// pressed, since panning would move it from under the pointer; the
    /// press's release brings it wholly into view.
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
            // Only one strip is on screen; there is nowhere else to drop.
            row.restore_cross_strip_detach(payload);
        }
        if let Some((group, index)) = row.take_pending_group_reorder() {
            changed |= shared
                .tree
                .reorder_group_within_strip(strip_id, group, index)
                .is_some();
        }
        let mut new_tab = None;
        let mut search_in = None;
        for group in &mut row.groups {
            if group.take_pending_search_open().is_some() {
                search_in = Some((group.id, ui::search::SearchRequest::this_app()));
            }
            // An app asks its host for what only the host can do. Here that
            // is search: the app's own search button opens the window's
            // search, as the window bar's does. What else an app may ask
            // (moving a device's display, a host shell, opening a local
            // document) a browser page cannot do, so it is let go.
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
                    // Restored to its width, the window comes back into
                    // view where the strip stood before it filled the
                    // display, which may leave its controls off screen.
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
        // The new-tab control asks which application to open there: search
        // over the applications, aimed at that window.
        if let Some(group) = new_tab {
            shared.search_request = Some(search::SearchRequest::NewTab(strip_id, group));
        }
        // A window's search control searches what that window shows, as
        // the native shell's does.
        if let Some((group, query)) = search_in {
            shared.search_request = Some(search::SearchRequest::Window(strip_id, group, query));
        }
        if changed {
            shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
            shared.changed();
        }
    }

    /// Apply queued navigation to the tree, then to the live rows.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass: every command against the tree, then its widget follow-ups in order"
    )]
    fn apply_commands(&mut self, state: &AutoCommit, path: &ObjId) {
        let queue = std::mem::take(&mut self.shared.borrow_mut().queue);
        if queue.is_empty() {
            return;
        }
        let mut ops = Vec::new();
        {
            let mut shared = self.shared.borrow_mut();
            for command in queue {
                let command = match command {
                    Admitted::Open(app) => navigation::resolve_open(&shared.tree, &app),
                    // A record already open in a window comes forward.
                    Admitted::Launch(app) => match self
                        .tiles
                        .showing(&app)
                        .and_then(|tab| route_of(&shared.tree, tab))
                    {
                        Some(ids) => Admitted::Select(ids),
                        None => Admitted::Launch(app),
                    },
                    // The workspace's history is the Activity app's to show:
                    // it opens, or comes forward, and is offered the view.
                    Admitted::Activity(_, view, author) => {
                        shared.activity_focus = Some((view, author));
                        navigation::resolve_open(&shared.tree, ACTIVITY_APP)
                    }
                    other => other,
                };
                // Admission ran at dispatch; an earlier command in this batch
                // may since have removed the target.
                let live =
                    |tree: &DesktopTree, ids: RouteIds| navigation::locate(tree, ids).is_some();
                match command {
                    Admitted::Select(ids) if live(&shared.tree, ids) => {
                        let tree = &mut shared.tree;
                        tree.active = ids.workspace;
                        tree.active_strip.insert(ids.workspace, ids.strip);
                        tree.set_active_group(ids.strip, ids.group);
                        if let Some(group) = find_group(tree, ids.strip, ids.group) {
                            group.activate_tab(ids.tab);
                        }
                        ops.push(RowOp::Show(ids.strip, ids.group, ids.tab));
                    }
                    Admitted::Width(ids, permille) if live(&shared.tree, ids) => {
                        if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group) {
                            group.width = WindowWidth::Fraction(permille);
                            ops.push(RowOp::Width(ids.strip, ids.group, group.width));
                        }
                    }
                    Admitted::Maximize(ids, maximized) if live(&shared.tree, ids) => {
                        if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group)
                            && group.maximized != maximized
                        {
                            group.maximized = maximized;
                            ops.push(RowOp::ToggleMaximized(ids.strip, ids.group));
                        }
                    }
                    Admitted::Close(ids) if live(&shared.tree, ids) => {
                        if let Some(removed) = shared.tree.close_tab(ids.tab) {
                            ops.push(RowOp::Close {
                                strip: removed.strip,
                                group: removed.group,
                                tab: ids.tab,
                                group_removed: removed.group_removed,
                            });
                        }
                    }
                    Admitted::Strip(workspace, strip) => {
                        shared.tree.active = workspace;
                        shared.tree.active_strip.insert(workspace, strip);
                    }
                    Admitted::Workspace(workspace) => shared.tree.active = workspace,
                    Admitted::Launch(app) => match launch(self.tiles.as_mut(), &mut shared, &app) {
                        Some(op) => ops.push(op),
                        None => continue,
                    },
                    // Opening search changes no model state.
                    Admitted::Search(query) => {
                        shared.search_request = Some(search::SearchRequest::Open(query));
                        continue;
                    }
                    // Presentation only: the tree does not change.
                    Admitted::Pan(strip, offset) => {
                        ops.push(RowOp::Pan(strip, offset));
                        continue;
                    }
                    Admitted::Scroll(ids, action) if live(&shared.tree, ids) => {
                        ops.push(RowOp::Scroll(ids.strip, ids.group, action));
                        continue;
                    }
                    Admitted::History(ids, direction) if live(&shared.tree, ids) => {
                        ops.push(RowOp::History(ids.strip, ids.group, ids.tab, direction));
                        continue;
                    }
                    // The Activity app's windows close as any window does.
                    Admitted::CloseActivity(_) => {
                        while let Some(ids) = navigation::window_of_app(&shared.tree, ACTIVITY_APP)
                        {
                            let Some(removed) = shared.tree.close_tab(ids.tab) else {
                                break;
                            };
                            ops.push(RowOp::Close {
                                strip: removed.strip,
                                group: removed.group,
                                tab: ids.tab,
                                group_removed: removed.group_removed,
                            });
                        }
                    }
                    Admitted::NotificationCenter(open) => {
                        if let Some(layer) = self.notices.as_mut() {
                            layer.set_center_open(open);
                        }
                        continue;
                    }
                    _ => continue,
                }
                shared.changed();
            }
        }
        self.show_active_strip();
        for op in ops {
            match op {
                RowOp::Show(strip, group, tab) => {
                    if let Some(row) = self.rows.get_mut(&strip) {
                        row.set_group_active_tab(group, tab);
                        if self.settle.take() != Some(group) {
                            row.scroll_group_into_view(group);
                        }
                    }
                }
                RowOp::Width(strip, group, width) => {
                    if let Some(group) = self.group_widget(strip, group) {
                        group.set_width(width);
                    }
                }
                RowOp::Open {
                    strip,
                    group,
                    index,
                    tab,
                } => self.open_group(strip, group, index, &tab),
                RowOp::ToggleMaximized(strip, group_id) => {
                    let restored = self.group_widget(strip, group_id).is_some_and(|group| {
                        group.toggle_maximized();
                        // The tree already holds the new state.
                        let _ = group.take_pending_maximize_change();
                        !group.maximized
                    });
                    if restored {
                        // Restored, the window comes back into view.
                        self.reveal = Some((strip, group_id));
                    }
                }
                RowOp::Close {
                    strip,
                    group,
                    tab,
                    group_removed,
                } => {
                    if let Some(group) = self.group_widget(strip, group) {
                        if group_removed {
                            group.start_closing();
                        } else {
                            group.request_close_tab(tab);
                        }
                    }
                }
                RowOp::Pan(strip, offset) => {
                    if self.shown == Some(strip)
                        && let Some(row) = self.rows.get_mut(&strip)
                        && pan_row(row, offset)
                    {
                        self.structure_dirty = true;
                    }
                }
                RowOp::Scroll(strip, group, action) => {
                    if let Some(group) = self.group_widget(strip, group) {
                        let _ = group.apply_primary_vertical_scroll(action.into());
                    }
                }
                RowOp::History(strip, group, tab, direction) => {
                    if let Some(host) = self
                        .group_widget(strip, group)
                        .and_then(|group| group.host_for_tab_mut(tab))
                        && host.on_nav_gesture(direction, state, path)
                    {
                        self.structure_dirty = true;
                    }
                }
            }
        }
    }

    /// Record what the shown strip and focused window present beside the
    /// tree, for the navigation snapshot.
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

    /// Where window `group`'s new-tab control is drawn, while its strip is
    /// shown, so a test presses it as a person does.
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

    /// Present a launched window. A row first built after the launch
    /// already holds it, with its content, from the tree.
    fn open_group(&mut self, strip: StripId, group: GroupId, index: usize, tab: &Tab) {
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
            // Placed whole on screen beside the window it opened next to,
            // sharing the display with it if need be, as on the native
            // desktop: revealing it alone pans the window in use half off
            // screen, taking its name with it.
            row.share_with_neighbour(group);
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

    /// Accent the active group of the shown strip and lay out against the
    /// host's viewport.
    fn present(&mut self) {
        let (route, width) = {
            let shared = self.shared.borrow();
            (
                navigation::active_route(&shared.tree),
                shared.viewport_width,
            )
        };
        let active = route.map(|ids| ids.group);
        // Whatever brought a window forward (a click, a launch, a switch, a
        // closed neighbour), its app takes the keyboard.
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
                // Windows size against the width between the strip's two
                // gutters, so one as wide as the display still shows its
                // border at both ends.
                row.set_output_spans(&[OutputSpan {
                    x: 0.0,
                    width,
                    usable: (width - 2.0 * STRIP_EDGE_GUTTER_PX).max(1.0),
                }]);
            }
        }
    }

    /// Whether the strip takes this wheel event. A sideways gesture, a
    /// trackpad's swipe or a mouse wheel's spin alike, moves the strip by its
    /// travel: it pans the strip under the pointer as one direct pan, 1:1
    /// within the strip and against the rubber band past either end, and
    /// when the gesture ends the strip settles and focus goes to the window
    /// panned to ([`Self::end_idle_swipe`]). A strip whose windows all fit
    /// cannot pan, so there the travel steps focus a window per
    /// [`STEP_TRAVEL_PX`], the first event of a gesture stepping at once so
    /// a single notch is felt.
    ///
    /// A gesture arrives as many events and all of them follow the decision
    /// its first one made: the strip takes it when it can pan or there is a
    /// window that way, and then keeps the rest of it at the strip's ends,
    /// pulling the rubber band, so the app under the pointer does not scroll
    /// sideways at the end of a flick; else the content under the pointer
    /// has it first ([`Self::pull_unhandled_swipe`]).
    fn route_swipe(&mut self, delta: MouseScrollDelta, scale_factor: f64, now: Instant) -> bool {
        self.end_idle_swipe(now);
        let Some(travel) = self.strip_travel(delta, scale_factor) else {
            return false;
        };
        let forward = if travel > 0.0 { 1 } else { -1 };
        let (pans, fits) = self.shown_row_ref().map_or((false, true), |row| {
            (
                can_pan(row, travel),
                !can_pan(row, 1.0) && !can_pan(row, -1.0),
            )
        });
        let mut swipe = self.swipe.unwrap_or_else(|| {
            let taken = pans || self.neighbour(forward).is_some();
            Swipe {
                taken,
                // On a strip that fits, the first event steps at once.
                travel: if taken {
                    f64::from(forward) * STEP_TRAVEL_PX - travel
                } else {
                    0.0
                },
                panned: false,
                pan: None,
                last: now,
            }
        });
        swipe.last = now;
        if swipe.taken {
            let stepping = fits && swipe.pan.is_none() && self.neighbour(forward).is_some();
            if stepping {
                // A reversal starts its travel afresh.
                if swipe.travel * travel < 0.0 {
                    swipe.travel = 0.0;
                }
                swipe.travel += travel;
                loop {
                    if swipe.travel.abs() < STEP_TRAVEL_PX {
                        break;
                    }
                    let step = if swipe.travel > 0.0 { 1 } else { -1 };
                    if !self.step_focus(step) {
                        swipe.travel = 0.0;
                        break;
                    }
                    swipe.travel -= f64::from(step) * STEP_TRAVEL_PX;
                }
            } else {
                self.pull_strip(&mut swipe, travel, now);
            }
        }
        self.swipe = Some(swipe);
        swipe.taken
    }

    /// Move the shown strip by `travel` as part of `swipe`'s direct pan,
    /// beginning it on the gesture's first pull. A strip showing a
    /// maximized window stays at home.
    fn pull_strip(&mut self, swipe: &mut Swipe, travel: f64, now: Instant) {
        let Some(row) = self.shown_row() else {
            return;
        };
        if row.groups.iter().any(|g| g.maximized) {
            return;
        }
        let requested = swipe.pan.unwrap_or_else(|| row.scroll_x()) + travel;
        swipe.pan = Some(requested);
        swipe.panned |= row.hold_strip_pull(requested, now);
    }

    /// A sideways gesture neither the strip took nor the content under the
    /// pointer scrolled by, such as one over the only window on a strip:
    /// the strip takes it from here and pulls against its rubber band, as
    /// it does past either end of a strip that pans.
    fn pull_unhandled_swipe(&mut self, delta: MouseScrollDelta, scale_factor: f64, now: Instant) {
        let Some(travel) = sideways(delta, scale_factor) else {
            return;
        };
        let Some(mut swipe) = self.swipe.filter(|swipe| !swipe.taken) else {
            return;
        };
        swipe.taken = true;
        swipe.last = now;
        self.pull_strip(&mut swipe, travel, now);
        self.swipe = Some(swipe);
    }

    /// The strip travel of a wheel event: its sideways travel, or over
    /// empty canvas, where nothing scrolls, its vertical travel too, so a
    /// plain mouse wheel moves the strip there (down toward later windows).
    fn strip_travel(&self, delta: MouseScrollDelta, scale_factor: f64) -> Option<f64> {
        sideways(delta, scale_factor).or_else(|| {
            self.shown_row_ref()
                .is_some_and(GroupRow::pointer_over_background)
                .then(|| vertical(delta, scale_factor))
                .filter(|travel| *travel != 0.0)
        })
    }

    /// Whether [`Self::route_swipe`] would take `delta` at `now` only to pan
    /// the shown strip, answered without routing it. A pan moves where the windows
    /// are and nothing else. Stepping focus on a strip that fits, a swipe at
    /// the strip's end, and the end of an earlier pan, which moves focus to
    /// the window panned to, all do more.
    fn pans_strip(&self, delta: MouseScrollDelta, now: Instant) -> bool {
        // Only the direction matters here, which no scale factor changes.
        let Some(travel) = self.strip_travel(delta, 1.0) else {
            return false;
        };
        let pans = self.shown_row_ref().is_some_and(|row| can_pan(row, travel));
        match self.swipe {
            Some(swipe) if now.saturating_duration_since(swipe.last) < SWIPE_GAP => {
                swipe.taken && pans
            }
            Some(swipe) if swipe.panned => false,
            _ => pans,
        }
    }

    /// End the swipe once wheel input has paused for [`SWIPE_GAP`]. When it
    /// panned the strip, focus goes to the window the person panned to, so
    /// the keyboard and Super+Left/Right carry on from what they look at.
    /// The strip stays put: framing that window would undo a short pan,
    /// such as a single notch's.
    fn end_idle_swipe(&mut self, now: Instant) {
        let Some(swipe) = self.swipe else {
            return;
        };
        if now.saturating_duration_since(swipe.last) < SWIPE_GAP {
            return;
        }
        self.swipe = None;
        if swipe.pan.is_some()
            && let Some(row) = self.shown_row()
        {
            // The platform already coasted the gesture (a trackpad's
            // momentum, a spun wheel's notches), so the strip is thrown
            // nowhere: a pull past an end springs back, and a strip left
            // within its range stays there through later geometry changes.
            if !row.cancel_strip_pan(now) {
                row.hold_strip_pan();
            }
        }
        if !swipe.panned {
            return;
        }
        let mut shared = self.shared.borrow_mut();
        let Some(current) = navigation::active_route(&shared.tree) else {
            return;
        };
        let Some((group, tab)) = self
            .shown
            .zip(self.shown_row_ref())
            .and_then(|(strip, row)| {
                if strip != current.strip {
                    return None;
                }
                let group = panned_to(row, Some(current.group))?;
                let tab = row.groups.iter().find(|g| g.id == group)?.active_tab_id();
                Some((group, tab))
            })
        else {
            return;
        };
        if group == current.group {
            return;
        }
        shared.queue.push(Admitted::Select(RouteIds {
            group,
            tab,
            ..current
        }));
        self.settle = Some(group);
    }

    /// Focus the window `steps` along the shown strip, walking every tab of
    /// every group in order and stopping at either end, as Super+Left/Right
    /// does in the shell. False when there is nowhere to go.
    fn step_focus(&self, steps: i32) -> bool {
        let Some(target) = self.neighbour(steps) else {
            return false;
        };
        self.shared
            .borrow_mut()
            .queue
            .push(Admitted::Select(target));
        true
    }

    /// The window `steps` along the shown strip from the focused one (or the
    /// one a queued select is about to focus), as
    /// [`Self::step_focus`] walks it.
    fn neighbour(&self, steps: i32) -> Option<RouteIds> {
        let shared = self.shared.borrow();
        // A step queued since the last update has moved focus already.
        let current = shared
            .queue
            .iter()
            .rev()
            .find_map(|admitted| match admitted {
                Admitted::Select(route) => Some(*route),
                _ => None,
            })
            .or_else(|| navigation::active_route(&shared.tree))?;
        let strip = shared
            .tree
            .workspaces
            .iter()
            .find(|w| w.id == current.workspace)
            .and_then(|w| w.strips.iter().find(|s| s.id == current.strip))?;
        let windows: Vec<(GroupId, TabId)> = strip
            .window_groups
            .iter()
            .flat_map(|g| g.tabs.iter().map(move |t| (g.id, t.id)))
            .collect();
        let at = windows
            .iter()
            .position(|&(g, t)| g == current.group && t == current.tab)?;
        let &(group, tab) = at
            .checked_add_signed(isize::try_from(steps).unwrap_or(0))
            .and_then(|index| windows.get(index))?;
        Some(RouteIds {
            group,
            tab,
            ..current
        })
    }
}

/// Put a window for `app` after the focused one of the shown strip.
fn launch(tiles: &mut dyn TileSource, shared: &mut Shared, app: &str) -> Option<RowOp> {
    let (tab_id, group_id) = (shared.minter.mint(), shared.minter.mint());
    let workspace = shared.tree.active_workspace()?;
    let strip = navigation::active_strip(&shared.tree, workspace)?;
    let tab = tiles.launch(workspace, tab_id, app)?;
    let after = shared.tree.active_group_for(strip);
    let model = shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|w| w.strips.iter_mut())
        .find(|s| s.id == strip)?;
    let index = after
        .and_then(|id| model.window_groups.iter().position(|g| g.id == id))
        .map_or(model.window_groups.len(), |position| {
            position.saturating_add(1)
        });
    model.window_groups.insert(
        index,
        tilo_desktop_model::WindowGroup {
            id: group_id,
            tabs: vec![tab.clone()],
            active_tab: tab.id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    );
    shared.tree.set_active_group(strip, group_id);
    shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
    Some(RowOp::Open {
        strip,
        group: group_id,
        index,
        tab,
    })
}

impl DesktopSurface {
    /// Open `app` as a new tab of window `group` in strip `strip`, focused.
    /// Returns whether it opened.
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

/// Pan `row` to `offset` the way the shell's direct strip pan does, within
/// the legal range, and keep it there when the strip's geometry changes. A
/// strip showing a maximized window stays at home.
fn pan_row(row: &mut GroupRow, offset: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let target = offset.clamp(0.0, row.pan_max_scroll_x());
    let now = ui::time::now();
    row.begin_strip_pan();
    let moved = row.update_strip_pan(target - row.scroll_x(), now);
    row.end_strip_pan(now);
    if moved {
        row.hold_strip_pan();
    }
    moved
}

/// Whether `row` can pan `travel` from where it rests.
fn can_pan(row: &GroupRow, travel: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let offset = row.scroll_x();
    if travel > 0.0 {
        offset < row.pan_max_scroll_x() - 0.5
    } else {
        travel < 0.0 && offset > 0.5
    }
}

fn observe_ids(minter: &mut IdMinter, tree: &DesktopTree) {
    for workspace in &tree.workspaces {
        minter.observe(workspace.id.0);
        for strip in &workspace.strips {
            minter.observe(strip.id.0);
            for group in &strip.window_groups {
                minter.observe(group.id);
                for tab in &group.tabs {
                    minter.observe(tab.id);
                }
            }
        }
    }
}

/// A strip's row with only the focused window's active tab built; the
/// other tabs are placeholders, returned in the order to build them: each
/// group's active tab by distance from the focused window, then the
/// inactive tabs.
fn build_row_lazily(
    tiles: &mut dyn TileSource,
    controls: Option<&ui::ControlDomain>,
    workspace: &Workspace,
    strip: &tilo_desktop_model::Strip,
    focused: Option<GroupId>,
) -> (GroupRow, Vec<TabId>) {
    let focus = focused
        .and_then(|id| strip.window_groups.iter().position(|g| g.id == id))
        .unwrap_or(0);
    let mut deferred: Vec<(bool, usize, TabId)> = Vec::new();
    let groups = strip
        .window_groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let tabs = group
                .tabs
                .iter()
                .map(|tab| (descriptor(tab), placeholder()))
                .collect();
            let mut built = WindowGroup::new(group.id, tabs, group.active_tab, group.width)
                .with_maximized(group.maximized);
            for tab in &group.tabs {
                let active = tab.id == group.active_tab;
                if active && index == focus {
                    if let Some(host) = built.host_for_tab_mut(tab.id) {
                        *host = build_tile(tiles, controls, workspace, tab);
                    }
                } else {
                    deferred.push((!active, index.abs_diff(focus), tab.id));
                }
            }
            built
        })
        .collect();
    deferred.compact_sort_by_key(|&(inactive, distance, _)| (inactive, distance));
    let order = deferred.into_iter().map(|(_, _, tab)| tab).collect();
    (strip_row(groups), order)
}

/// A strip's row with the wallpaper gutter before its first window, as the
/// native desktop keeps it.
fn strip_row(groups: Vec<WindowGroup>) -> GroupRow {
    GroupRow::new(groups).with_leading_inset(STRIP_EDGE_GUTTER_PX)
}

fn find_tab(tree: &DesktopTree, strip: StripId, tab: TabId) -> Option<(&Workspace, &Tab)> {
    tree.workspaces.iter().find_map(|workspace| {
        let strip = workspace.strips.iter().find(|s| s.id == strip)?;
        strip
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .find(|t| t.id == tab)
            .map(|tab| (workspace, tab))
    })
}

/// A strip's row as the tree describes it. Each tab takes its content from
/// `live` when present there, else from `tiles`.
fn build_row(
    tiles: &mut dyn TileSource,
    controls: Option<&ui::ControlDomain>,
    workspace: &Workspace,
    strip: &tilo_desktop_model::Strip,
    live: &mut HashMap<TabId, TileHost>,
) -> GroupRow {
    let groups = strip
        .window_groups
        .iter()
        .map(|group| {
            let tabs = group
                .tabs
                .iter()
                .map(|tab| (descriptor(tab), placeholder()))
                .collect();
            let mut built = WindowGroup::new(group.id, tabs, group.active_tab, group.width)
                .with_maximized(group.maximized);
            for tab in &group.tabs {
                if let Some(host) = built.host_for_tab_mut(tab.id) {
                    *host = live
                        .remove(&tab.id)
                        .unwrap_or_else(|| build_tile(tiles, controls, workspace, tab));
                }
            }
            built
        })
        .collect();
    strip_row(groups)
}

/// `tab`'s content from `tiles`, its application named to a panic hook
/// while it is built and whenever it runs, and bound to the host's
/// `controls` when the host already bound the surface.
fn build_tile(
    tiles: &mut dyn TileSource,
    controls: Option<&ui::ControlDomain>,
    workspace: &Workspace,
    tab: &Tab,
) -> TileHost {
    let _running = ui::widget::panic_scope::enter(Some(tab.id));
    let mut host = tiles.tile(workspace, tab);
    host.set_panic_scope(tab.id);
    // Its app's bar is drawn here: what it declares goes on its window.
    let window = ui::hosted_window::HostedWindow::new();
    window.set_workspace(tiles.workspace_room());
    host.set_hosted_window(window);
    if let Some(domain) = controls {
        ui::widget::controls::install_control_custody(
            &mut host,
            domain,
            ui::control_custody::CustodyAttachment::Dynamic,
        );
    }
    host
}

fn descriptor(tab: &Tab) -> TabDescriptor {
    TabDescriptor {
        app_kind: Some(tile_app_kind(&tab.tile)),
        ..TabDescriptor::new(tab.id, tab.title.clone())
    }
}

/// The mark a tab for `tile` wears: its app's own symbol for a window,
/// the kind's glyph for a stand-in. Every host's title bar, switcher and
/// overview name a window's app by this one mapping.
pub fn tile_app_kind(tile: &tilo_desktop_model::TileContent) -> ui::widget::AppKind {
    use ui::widget::AppKind as Kind;
    match tile {
        tilo_desktop_model::TileContent::AppPlaceholder { kind, .. }
        | tilo_desktop_model::TileContent::Stub { kind, .. } => match kind {
            tilo_desktop_model::AppKind::Terminal => Kind::Terminal,
            tilo_desktop_model::AppKind::FileExplorer => Kind::Files,
            tilo_desktop_model::AppKind::Browser => Kind::Browser,
            tilo_desktop_model::AppKind::Other(id)
                if ["Notes", "Word", "Editor"]
                    .iter()
                    .any(|known| id.eq_ignore_ascii_case(known))
                    || ["so.tilo.word-processor", "so.tilo.notes"]
                        .iter()
                        .any(|known| id == known) =>
            {
                Kind::Editor
            }
            _ => Kind::Settings,
        },
        // A window wears its own app's glyph, never another app's.
        tilo_desktop_model::TileContent::Foreign { app_id, .. }
        | tilo_desktop_model::TileContent::LocalApp { app_id, .. }
        | tilo_desktop_model::TileContent::PeerWindow { app_id, .. }
        | tilo_desktop_model::TileContent::HostedWindow { app_id, .. } => {
            Kind::App(ui::icons::app_glyph(app_id, None))
        }
    }
}

fn placeholder() -> TileContent {
    TileContent::Notice {
        title: String::new(),
        detail: String::new(),
    }
}

fn find_group(
    tree: &mut DesktopTree,
    strip: StripId,
    group: GroupId,
) -> Option<&mut tilo_desktop_model::WindowGroup> {
    tree.workspaces
        .iter_mut()
        .flat_map(|w| w.strips.iter_mut())
        .find(|s| s.id == strip)?
        .window_groups
        .iter_mut()
        .find(|g| g.id == group)
}

impl Widget for DesktopSurface {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
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
        let before = self.shared.borrow().revision;
        let search_was_open = self.search.is_open();
        self.end_idle_swipe(ui::time::now());
        self.reconcile();
        self.harvest();
        {
            let (doc, root) = ui::doc::document_ref(state, path);
            self.apply_commands(doc, root);
            // Search opens over the tree as these commands left it, and what
            // it chose applies in this same pass.
            self.step_search(doc);
            self.apply_commands(doc, root);
        }
        self.show_active_strip();
        self.build_deferred();
        self.present();
        let held = self.apply_holders();
        let changed = self.shared.borrow().revision != before
            || search_was_open != self.search.is_open()
            || held;
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
        changed
            || row_changed
            || bars_changed
            || share_asked
            || embeds_changed
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
        let row: Option<NodeId> = self.shown_row().map(|row| {
            let node = row.build_layout(taffy, theme, node_map);
            // Tiled windows keep wallpaper under them, so their bottom
            // border and corners show; a maximized one takes the band.
            let bottom = if row.groups.iter().any(|group| group.maximized) {
                0.0
            } else {
                STRIP_BOTTOM_GUTTER_PX
            };
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.margin.bottom =
                ui::taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(bottom));
            let _ = taffy.set_style(node, style);
            node
        });
        let search = self.search_layout(taffy, theme, node_map);
        let notices = self
            .notices
            .as_mut()
            .map(|layer| layer.build_layout(taffy, theme, node_map));
        let arrivals = self.arrival_hints.build_layout(taffy, theme, node_map);
        let acknowledgement = self.acknowledgement.build_layout(taffy, theme, node_map);
        // The row, when shown, is always the first child, then search and
        // the notifications, each when present, and the arrival hints and
        // the acknowledgement last.
        let children: Vec<NodeId> = row
            .into_iter()
            .chain(search)
            .chain(notices)
            .chain([arrivals, acknowledgement])
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
        let has_row = self.shown_row().is_some();
        let child = if has_row {
            nodes.first().copied()
        } else {
            None
        };
        let mut next = usize::from(has_row);
        if self.search.is_open() {
            if let Some(&palette) = nodes.get(next) {
                self.search
                    .palette_mut()
                    .apply_layout(taffy, palette, inner);
            }
            next = next.saturating_add(1);
        }
        if let Some(layer) = self.notices.as_mut()
            && let Some(&notices) = nodes.get(next)
        {
            layer.apply_layout(taffy, notices, inner);
        }
        if let [.., arrivals, acknowledgement] = nodes[..] {
            self.arrival_hints.apply_layout(taffy, arrivals, inner);
            self.acknowledgement
                .apply_layout(taffy, acknowledgement, inner);
        }
        let reveal = self.reveal.take();
        let shown = self.shown;
        let mut pending = None;
        if let (Some(row), Some(child)) = (self.shown_row(), child) {
            row.apply_layout(taffy, child, inner);
            // The launch frame itself shows the new window: pan, then place
            // the groups again at the new offset.
            if let Some((strip, group)) = reveal
                && shown == Some(strip)
            {
                if !row.group_is_laid_out(group) {
                    pending = reveal;
                } else if row.scroll_group_into_view(group) {
                    row.apply_layout(taffy, child, inner);
                }
            }
        }
        self.reveal = pending;
        let shown_tabs: Vec<(TabId, Rect)> = self
            .shown_row()
            .map(|row| {
                row.groups
                    .iter()
                    .filter_map(|group| {
                        let tab = group.active_tab_id();
                        group.content_rect_for_tab(tab).map(|rect| (tab, rect))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let shown_strip = self
            .shown
            .zip(self.shown_row_ref())
            .map(|(strip, row)| (strip, row.viewport(), row.drawn_scroll_x()));
        // Banners start where the windows' content does, below their bars:
        // pinned under the top bar they covered the windows' own controls.
        let content_top = shown_tabs
            .iter()
            .map(|(_, rect)| rect.y0)
            .fold(0.0, f64::max);
        if let Some(layer) = self.notices.as_mut() {
            layer.set_clear_top(content_top);
        }
        self.place_arrival_hints(content_top);
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
        let hint_places = self.shown_row().map(|row| {
            ui::widget::draw_child(row, scene, transform, text, theme, presence);
            row.hint_places()
        });
        self.shared.borrow_mut().hint_places = hint_places.unwrap_or_default();
        self.draw_embeds(scene, transform, text, theme, presence);
        self.draw_overlays(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(layer) = self.notices.as_mut()
            && layer.route(event, ctx.scale_factor)
        {
            *ctx.handled = true;
            return;
        }
        if self.search_event(event, ctx) {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.route_swipe(*delta, ctx.scale_factor, ui::time::now())
        {
            *ctx.handled = true;
            return;
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if let [.., arrivals, _] = children[..] {
            let path = ctx.path.clone();
            self.arrival_hints
                .handle_event(event, &mut ctx.reborrow(&path, arrivals));
            if *ctx.handled {
                self.reveal_chosen_arrival();
                return;
            }
        }
        let child = children.first().copied();
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.focus_clicked_group();
            self.reveal_on_release = self.shown.and_then(|strip| {
                let group = self.rows.get(&strip)?.group_under_cursor()?;
                Some((strip, group))
            });
        }
        if let (Some(row), Some(child)) = (self.shown_row(), child) {
            let path = ctx.path.clone();
            row.handle_event(event, &mut ctx.reborrow(&path, child));
        }
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
            // A window clicked while partly off screen comes into view, as
            // focusing it from the keyboard brings it.
            self.reveal = Some((strip, group));
            self.structure_dirty = true;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && !*ctx.handled
        {
            self.pull_unhandled_swipe(*delta, ctx.scale_factor, ui::time::now());
            *ctx.handled = self.swipe.is_some_and(|swipe| swipe.taken);
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
            && !self.search.is_open()
            && self.pans_strip(*delta, ui::time::now())
    }

    /// The app of the window brought forward, once its tab is built as an
    /// app: the host then gives it keyboard focus.
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
        let search = self
            .search
            .is_open()
            .then(|| self.search.palette() as &dyn Widget);
        let notices = self.notices.as_ref().map(|l| l as &dyn Widget);
        self.shown
            .and_then(|strip| self.rows.get(&strip))
            .map(|row| row as &dyn Widget)
            .into_iter()
            .chain(search)
            .chain(notices)
            .chain([
                &self.arrival_hints as &dyn Widget,
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
        // Retain original visible overlay callback reach after the retained rows.
        if self.search.is_open() {
            request.visit(self.search.palette_mut())?;
        }
        if let Some(notices) = &mut self.notices {
            request.visit(notices)?;
        }
        Ok(())
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let open = self.search.is_open();
        let row = self
            .shown
            .and_then(|strip| self.rows.get_mut(&strip))
            .map(|row| row as &mut (dyn Widget + '_));
        let search = open.then(|| self.search.palette_mut() as &mut (dyn Widget + '_));
        let notices = self.notices.as_mut().map(|l| l as &mut (dyn Widget + '_));
        let arrivals = &mut self.arrival_hints as &mut (dyn Widget + '_);
        let acknowledgement = &mut self.acknowledgement as &mut (dyn Widget + '_);
        row.into_iter()
            .chain(search)
            .chain(notices)
            .chain([arrivals, acknowledgement])
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
            || self.embeds_animate()
            || self.unbuilt.iter().any(|&(strip, _)| Some(strip) == shown)
            || self
                .children()
                .into_iter()
                .any(Widget::needs_animation_frame)
    }

    /// A pan's end, when the strip settles and focus goes to the window
    /// panned to.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let ends = self
            .swipe
            .filter(|swipe| swipe.panned || swipe.pan.is_some())
            .and_then(|swipe| swipe.last.checked_add(SWIPE_GAP));
        // An app's answer to the search query is still coming: look again
        // shortly, as its rows show once it is in.
        let answer = self
            .search
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

    fn has_pending_visual_work(&self) -> bool {
        let shown = self.shown;
        self.unbuilt.iter().any(|&(strip, _)| Some(strip) == shown)
            || self
                .children()
                .into_iter()
                .any(Widget::has_pending_visual_work)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }

    /// A change the shown strip's windows carry out in place stays in
    /// place; the surface's own changes rebuild.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        let own = ui::widget::StructureChange::of(std::mem::take(&mut self.structure_dirty));
        let row = self.shown_row().map_or(
            ui::widget::StructureChange::None,
            Widget::take_structure_change,
        );
        own.join(row)
    }
}

impl ui::component_lifecycle::ComponentLifecycle for DesktopSurface {
    /// Every tab's application, shown or not, closes with the surface.
    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        self.each_host(|host| ui::component_lifecycle::close_components(host))
    }

    fn take_component_close_reports(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports =
            self.each_host(|host| ui::component_lifecycle::take_component_close_reports(host));
        // Windows a row removed itself: pruned after their close, or
        // dropped by a tree change.
        for row in self.rows.values_mut() {
            reports.extend(ui::component_lifecycle::take_component_close_reports(row));
        }
        reports
    }
}

impl ui::widget::controls::ActionControls for DesktopSurface {
    /// Bind every tab, shown or not, and keep `domain` for the tabs built
    /// later; then the overlays the surface currently shows.
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        self.controls = Some(domain.clone());
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
                for tab in tabs {
                    if let Some(host) = group.host_for_tab_mut(tab) {
                        ui::widget::controls::install_control_custody(host, domain, attachment);
                    }
                }
            }
        }
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.unrouted_control.is_some()
            || self
                .children()
                .into_iter()
                .any(ui::widget::controls::has_pending_control_invocation)
    }

    /// An action row the palette queued outlives the palette closing on it.
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.unrouted_control
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(self.search.palette_mut()))
            .or_else(|| ui::widget::controls::take_control_invocation(self.shown_row()?))
    }
}

impl DesktopSurface {
    /// The search palette, then the banners and centre over everything.
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
        self.draw_search(scene, transform, text, theme, presence);
        if let Some(layer) = self.notices.as_mut() {
            layer.draw(scene, transform, text, theme, presence);
        }
        self.acknowledgement
            .draw(scene, transform, text, theme, presence);
    }

    /// Traversal reaches only active tabs; lifecycle must reach them all.
    fn each_host(
        &mut self,
        mut visit: impl FnMut(&mut TileHost) -> Vec<ui::application::ComponentCloseReport>,
    ) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.close_reports);
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
                for tab in tabs {
                    if let Some(host) = group.host_for_tab_mut(tab) {
                        reports.extend(visit(host));
                    }
                }
            }
        }
        reports
    }
}

/// The in-memory desktop a host starts from when it has no stored one: one
/// workspace, one strip, and a window group per tab in `tabs`.
pub fn seed_tree(minter: &mut IdMinter, workspace_name: &str, tabs: Vec<Tab>) -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(minter.mint()), workspace_name);
    let mut strip = tilo_desktop_model::Strip::new(StripId(minter.mint()), "Main");
    strip.window_groups = tabs
        .into_iter()
        .map(|tab| tilo_desktop_model::WindowGroup {
            id: minter.mint(),
            active_tab: tab.id,
            tabs: vec![tab],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    let first_group = strip.window_groups.first().map(|g| g.id);
    let strip_id = strip.id;
    workspace.strips = vec![strip];
    let mut tree = DesktopTree {
        active: workspace.id,
        ..DesktopTree::default()
    };
    tree.active_strip.insert(workspace.id, strip_id);
    if let Some(group) = first_group {
        tree.set_active_group(strip_id, group);
    }
    tree.workspaces = vec![workspace];
    tree
}

#[cfg(test)]
mod activity_tests;
#[cfg(test)]
mod tests;
