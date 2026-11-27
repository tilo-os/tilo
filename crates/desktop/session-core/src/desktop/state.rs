//! The desktop's live state: the active workspace's windows and their applications, with the
//! model as the source of truth.
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

pub use shown::ShownTabs;

use tilo_desktop_model::{
    GroupId, IdMinter, Layout, StripId, Tab, TabId, TileContent, Workspace, WorkspaceId,
};
use tilo_ui_base::collaboration::PresencePayload;
use tilo_ui_base::presence::PresenceKey;
use ui::kurbo::{Point, Rect};
use ui::time::Duration;
use ui::widget::TileHost;

use crate::desktop::navigation::{
    self, ActivityView, Admitted, Application, ChromeEffect, HistoryDirection, Presentation,
    RouteIds, Snapshot, Verb,
};
use crate::desktop::session::{ActivityAsk, ActivityReads, ActivityRequests};

pub mod apply;
pub use apply::RowOp;
pub mod arrivals;
pub mod artwork;
pub mod banners;
pub mod displays;
pub mod drawn;
pub mod floats;
pub use floats::AskedFloat;
pub mod history;
pub mod launch;
pub mod lookup;
pub use lookup::{find_group, find_tab, observe_ids};
pub mod chrome_verbs;
pub mod liveness;
pub mod notices;
pub mod pass;
pub use pass::DesktopPass;
pub mod personal_ask;
pub mod reveals;
pub mod search;
pub mod shared;
pub mod shortcuts;
pub mod shown;
pub mod tile_content;
pub mod window_bars;
pub use window_bars::BarField;
pub mod window_menu;
pub mod window_sizes;
pub mod workspace_share;

pub use history::{ActivityModel, ActivityPaneCode, HistorySource, OpenActivity, open_activity};
pub use launch::{StripPart, part_among};
pub use liveness::LivenessChoice;
pub use notices::{AppInbox, AppNotifier, DesktopNotifications, NotificationOutcome};
pub use personal_ask::PersonalAsk;
pub use search::{
    HostAction, HostActionSource, PALETTE_EDGE, SearchHost, SearchHostFeed, SearchRow, TypedField,
    TypedInsertion, TypedRefusal, host_action_key, is_host_offer, search_chord_keys,
};
pub use tile_content::{app_name_of, build_tile, descriptor, placeholder};
pub use tile_content::{tile_app_kind, window_name, window_title};
pub use window_menu::Row as WindowMenuRow;
pub use workspace_share::{WorkspaceShare, WorkspaceShares};

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
pub const ACTIVITY_APP: &str = crate::desktop::activity::vocabulary::APP_ID;
/// The action of the Undo a close offers ([`DesktopHandle::press_notice`]).
pub const REOPEN_CLOSED: &str = "shell-reopen-closed";

/// Where a host's tab contents come from.
pub trait TileSource {
    /// The content for `tab` in `workspace` , built the first time the tab's strip is shown.
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost;

    /// A launch command named `app` : the tab to open under `id` , beside the tab `beside` when
    /// it is a window of the app's own rather than the window the launch opens. `None` opens
    /// nothing.
    fn launch(
        &mut self,
        _workspace: &Workspace,
        _id: TabId,
        _app: &str,
        _beside: Option<TabId>,
    ) -> Option<Tab> {
        None
    }

    /// The person reopened `tab` where they closed it, and its window is gone: the tab holds the
    /// app that opens it again, and waits for that app's window to take it. What the source needs
    /// to ask for the app: `launch` when the close remembered what opens the window on what it
    /// showed ([`tilo_desktop_model::ClosedTab::launch`]), else the tile's app.
    fn reopened(&mut self, _workspace: &Workspace, _tab: &Tab, _launch: Option<&str>) {}

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

    /// The resource `tab`'s window shows, by its published address, and the kind of resource it
    /// keeps: what the windows a person may share are listed by ([`ui::window_share`]). What the
    /// window's bar carries is the workspace's layout's
    /// ([`DesktopHandle::set_window_resources`]), which every device of the person's reads.
    fn window_resource(&self, _tab: TabId) -> Option<(String, String)> {
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
    /// Whether the session took it: false keeps the move the shell's own, and nothing else about it
    /// changes.
    fn carry(&self, _tab: TabId, _to: WorkspaceId) -> bool {
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

/// What a window says in the place its app left: the outcome the host wrote when the app could
/// not open or stopped, with the label of the button that asks for it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndedAnswer {
    /// The sentence it leads with, in the person's words ("Notes stopped working").
    pub what: String,
    /// Why, in words they can act on; empty where what happened is all there is to say.
    pub why: String,
    /// What the app itself said of it, quoted under the sentence; empty where it said nothing
    /// that tells.
    pub said: String,
    /// The label of the button that asks for the app again, when asking can work.
    pub retry: Option<String>,
    /// The technical side, kept behind the card's Details; empty where there is none.
    pub details: String,
}

/// A row a caller of the published search chose ([`Shared::search_asks`]): its place, the
/// argument its action asks for, the modifier it was chosen with, and the press that chose it.
pub type SearchAsk = (usize, Option<String>, Option<String>, Option<u32>);

pub struct Shared {
    pub tree: Layout,
    /// The windows that opened here as cards: snapped into their strip and floated again, they go
    /// back to the card they were, not over the shell at a window's size.
    pub cards_born: std::collections::BTreeSet<GroupId>,
    /// The windows the person floated or put back that the host has not made arranging actions of
    /// yet ([`DesktopHandle::take_asked_floats`]).
    pub asked_floats: Vec<floats::AskedFloat>,
    /// Whoever holds each window someone else opened, by tab ([`DesktopHandle::set_holders`]).
    pub holders: HashMap<TabId, ui::widget::TabHolder>,
    /// The agent run that opened each window an agent opened, by tab
    /// ([`DesktopHandle::set_opened_by`]).
    pub opened_by: std::collections::BTreeMap<TabId, tilo_desktop_model::ownership::OpenedBy>,
    /// The member who opened each window of the shared desktop, by tab
    /// ([`DesktopHandle::set_authors`]): whose hands opened it, which the shell shows and no
    /// permission reads.
    pub authors: std::collections::BTreeMap<TabId, String>,
    /// Who else is at each window now, by tab: the faces (`name`, `#rrggbb` colour) of the
    /// members whose pointer stands over it ([`DesktopHandle::set_pointer_faces`]).
    pub pointer_faces: HashMap<TabId, Vec<(String, String)>>,
    /// The person each of those agents opened its window for, by tab, by the name its server knows
    /// them by ([`DesktopHandle::set_opened_for`]).
    pub opened_for: std::collections::BTreeMap<TabId, String>,
    /// The resource each window shows, by tab, as the workspace's layout keeps it
    /// ([`DesktopHandle::set_window_resources`]): what a window's bar carries when a person picks
    /// it up, on every device that reads the layout.
    pub resources: std::collections::BTreeMap<TabId, String>,
    /// The item each window was opened on, by tab, as the workspace's layout keeps it
    /// ([`DesktopHandle::set_window_items`]): the conversation a send landed in, the record a
    /// search hit opened. Not the tab's view, which is what a place shows.
    pub items: std::collections::BTreeMap<TabId, String>,
    /// The windows the sharing app lists for its picker, as last published
    /// ([`DesktopSurface::offer_window_shares`]): a window's menu offers the share row exactly for
    /// these.
    pub window_offers: Vec<ui::window_share::OpenWindow>,
    /// The host session's reading of the windows it shares with their workspaces
    /// ([`DesktopHandle::reads_workspace_shares`]): what a window's menu says of its share.
    pub workspace_shares: Option<WorkspaceShares>,
    /// The windows whose menu asked to be shared with their workspace, or to stop, waiting for
    /// the host ([`DesktopHandle::take_workspace_share_asks`]).
    /// Each with the person's press that chose the row, when one did.
    pub workspace_share_asks: Vec<(TabId, bool, Option<u32>)>,
    /// The person's other devices' screens a window may move to, as the host last read them
    /// ([`DesktopHandle::set_other_screens`]).
    pub other_screens: Vec<window_menu::OtherScreen>,
    /// The windows moved to another of the person's devices, waiting for the host
    /// ([`DesktopHandle::take_device_moves`]): the tab and the screen.
    pub device_moves: Vec<(TabId, window_menu::OtherScreen)>,
    /// What the person may change and who made each strip, as the host answers it
    /// ([`DesktopHandle::set_rules`]); with none, nothing is refused.
    pub rules: Option<Rc<dyn crate::desktop::host::contract::DesktopRules>>,
    /// Where the desktop leaves what it asks of its host ([`DesktopHandle::set_asks`]).
    pub asks: Option<Rc<RefCell<Vec<crate::desktop::host::contract::DesktopAsk>>>>,
    /// The windows waiting for the person ([`DesktopHandle::ask_attention`]),
    /// until each has their focus.
    pub asking: std::collections::BTreeSet<TabId>,
    /// The bars the person asked a menu of (a secondary press), and where they pressed, waiting for
    /// the host.
    pub menus: Vec<(TabId, Point)>,
    /// Its person opens windows and tabs here ([`DesktopHandle::set_opens`]): the windows offer a
    /// new tab.
    pub opens: bool,
    /// Bumped on every tree change, so a host republishes navigation only when something moved.
    pub revision: u64,
    pub queue: Vec<Admitted>,
    /// What the Activity app was last asked to show (a view, and whose doings when one person's),
    /// until the activity model is up to offer it.
    pub activity_focus: Option<(ActivityView, crate::desktop::activity::ActivityDeepLink)>,
    pub minter: IdMinter,
    /// Logical width the strip lays out against.
    pub viewport_width: f64,
    /// The displays the host last named, primary first, in the surface's coordinates: the one a
    /// device with a single display lays its strip out on, which the tree's partition does not
    /// keep ([`DesktopHandle::set_displays`]).
    pub displays: Vec<tilo_desktop_model::Display>,
    /// Where each strip shown on a display is panned to, by strip and display (`None` on one
    /// display), and the window last brought into view there ([`shown::lay_out`]).
    pub pans: HashMap<(StripId, Option<u32>), shown::StripPan>,
    /// The windows asked into view since the last layout: each one opened, restored or moved.
    pub reveal_asked: Vec<(StripId, GroupId)>,
    /// How much of the shell's top edge the desktop bar overlays now
    /// ([`DesktopHandle::set_top_overlay_inset`]): every window moves down by it, whole, so its own
    /// bar stands below the bar shown over it rather than under it.
    pub top_overlay: f64,
    /// The tree was replaced from outside; rows must be reconciled.
    pub replaced: bool,
    /// Where each shown window stands and the bounds of the tab it shows, as last laid out.
    pub shown_tabs: ShownTabs,
    /// Where the shell that draws the windows itself stood each tab's content, in the surface's
    /// coordinates ([`DesktopHandle::place_contents`]): a tilolang shell puts every window in a
    /// well its own views lay out, so this, not the layout's reckoning, is where a window's
    /// controls are. `None` while no such shell said.
    pub placed: Option<HashMap<TabId, Rect>>,
    /// The floating window the person carries with a press, while the press lasts
    /// ([`DesktopHandle::carry_float_at`]).
    pub carried: Option<floats::Carried>,
    /// Each window floating over the shell, and what it is drawn whole over the windows under it —
    /// its bar, its body and the shadow that lifts it — as last drawn.
    pub float_covers: Vec<(TabId, Rect)>,
    /// The shown strip, the area it is drawn in and how far it is panned, as last laid out.
    pub shown_strip: Option<(StripId, Rect, f64)>,
    /// Carets and selections of the focused window's editors, as last laid out
    /// ([`tilo_ui_base::presence::capability::subtree_presence`]).
    pub focused_presence: Vec<(PresenceKey, PresencePayload)>,
    /// Tabs whose content the host asked to build again.
    pub rebuild: Vec<TabId>,
    pub search_request: Option<search::SearchRequest>,
    pub search_open: bool,
    /// What the person typed while the search they asked for was not up yet.
    pub typed_ahead: search::TypedAhead,
    /// The search palette itself, shared with the host that draws it over the windows
    /// (`DesktopHandle::search_palette`): the shell feeds it what its windows hold and carries out
    /// what is chosen there, while the host above the windows draws it and routes the person's
    /// input to it.
    pub search: Rc<RefCell<tilo_desktop_search::DesktopSearch>>,
    /// Bumped whenever what the palette shows or what opening it asks of the shell changes, so the
    /// shell takes up what the host above it did and the host above it draws what the shell
    /// published.
    pub search_revision: u64,
    /// Where search stands when it was opened for one window: under that window's bar, over the
    /// rest of it, as the shell last laid it out.
    pub search_area: Option<Rect>,
    /// What the host offers to do for the person, and what they chose.
    pub host_actions: Vec<search::HostAction>,
    pub host_choices: Vec<String>,
    /// What the host adds to search and carries out of it ([`DesktopHandle::set_search_host`]).
    pub search_host: Option<std::rc::Rc<dyn search::SearchHost>>,
    /// Rows chosen in search that the host carries out, in order, each with the compositor's
    /// serial of the person's press on the system's own surfaces that chose it, when one did.
    pub search_choices: Vec<(tilo_desktop_search::Target, Option<u32>)>,
    /// The search the host last carried a row out of: where it stood and what was typed, so a row
    /// it could not carry out is asked again with the answer on that row.
    pub search_choice_context: Option<(ui::search::Scope, String, tilo_desktop_search::Target)>,
    /// The reason a row the host carried out did not happen, for the search opened again over it to
    /// say on that row.
    pub search_refusal: Option<(tilo_desktop_search::Target, String)>,
    /// The rows a caller of the published search chose, with the argument each one's action asks
    /// for, the modifier it was chosen with (`here`, `beside` or `more`) and the compositor's
    /// serial of the person's press on the system's own surfaces that chose it, when one did,
    /// taken on the next update pass.
    pub search_asks: Vec<SearchAsk>,
    /// The open search, as last shown: its query and rows, and whether it is still filling in.
    pub shown_search: search::ShownSearch,
    /// The windows whose shown tab has drawn what it shows, as of the last update pass
    /// ([`DesktopHandle::drawn`]).
    pub drawn: std::collections::BTreeSet<TabId>,
    /// The field the person types into in the desktop itself, as last
    /// shown, and the text committed into it from outside the keyboard,
    /// taken on the next update pass.
    pub typed_field: Result<search::TypedField, search::TypedRefusal>,
    pub typed_ask: Option<search::TypedAsk>,
    /// The ask about the person themselves the bar shows from their avatar.
    pub personal_ask: Option<personal_ask::PersonalAsk>,
    pub personal_ask_revision: u64,
    /// Windows someone else opened lately, for the edge hint.
    pub arrivals: Vec<arrivals::Arrival>,
    /// The window to bring into view without its focus once the person's hands rest
    /// ([`DesktopHandle::reveal_tab`]).
    pub revealing: reveals::Revealing,
    /// The window the person just moved or resized: the strip pans to it once it stands where it went.
    pub followed: Option<GroupId>,
    /// Pan, scroll, history and activity state, as last presented.
    pub presentation: Presentation,
    /// Bumped whenever `presentation` changes. Unlike `revision` it never asks a host to save or
    /// share the tree.
    pub presented: u64,
    /// The artwork each window brought of its own, by the compositor's id for it
    /// ([`DesktopHandle::set_window_artwork`]).
    pub artwork: std::collections::BTreeMap<u64, tilo_ui_base::icons::AppImage>,
    /// The apps whose windows keep what they show to themselves
    /// ([`DesktopHandle::set_withheld_apps`]).
    pub withheld: std::collections::BTreeSet<String>,
    /// The windows whose app stopped answering ([`DesktopHandle::set_unresponsive_windows`]), by
    /// the compositor's id for them.
    pub unresponsive: std::collections::BTreeSet<u64>,
    /// Bumped whenever `unresponsive` changes, so a reading made from it is published again.
    pub unresponsive_revision: u64,
    /// The shape of each window the compositor shows
    /// ([`DesktopHandle::set_window_sizes`]), by its id for it: the size it draws at, so a place
    /// picturing it keeps its own aspect.
    pub window_sizes: std::collections::BTreeMap<u64, (i32, i32)>,
    /// Bumped whenever `window_sizes` changes, so a reading made from them is published again.
    pub window_sizes_revision: u64,
    /// What the person chose on a window that stopped answering, for the host to carry out
    /// ([`DesktopHandle::take_liveness_choices`]).
    pub liveness: Vec<(TabId, LivenessChoice)>,
    /// What each window whose app ended says in its place ([`DesktopHandle::set_ended_answer`]):
    /// the answer the host left in the tab, with the way to ask the app again.
    pub ended: std::collections::BTreeMap<TabId, EndedAnswer>,
    /// What the shell says over the windows for a while ([`DesktopHandle::notice`]).
    pub notice: Rc<RefCell<ui::widget::toast::Transient<ui::window_toolbar::ToolbarNotice>>>,
    /// The buttons of that notice the person pressed that the shell does not carry out itself,
    /// waiting for the layer that showed them ([`DesktopHandle::take_notice_actions`]).
    pub notice_actions: RefCell<Vec<String>>,
    /// The notifications the host installed: the shell carries out what the person chose there,
    /// and the host above the windows draws them ([`DesktopHandle::notifications`]).
    pub notifications: Option<DesktopNotifications>,
    /// The windows other devices share with this one, each decoded into its tile as it arrives:
    /// the surface that decodes them keeps them here, as only it knows their type.
    pub peers: Option<Rc<dyn std::any::Any>>,
    /// What the session's apps asked of their windows' bars since the last pass, and what the
    /// person did there, for the apps' connections ([`DesktopHandle::declare_bar`] and its
    /// siblings).
    pub bars: window_bars::Bars,
    /// Sideways wheel travel the apps of these windows left to the shell,
    /// by tab, taken on the next pass ([`DesktopHandle::sideways_wheel`]).
    pub sideways: Vec<(TabId, f64)>,
    /// What asks for this shell's next pass when a change waits for it
    /// ([`DesktopHandle::set_wake`]).
    pub wake: crate::desktop::wake::Waker,
    /// What saves and shares a changed tree ([`DesktopHandle::set_publish`]), and the revision
    /// it last had.
    pub publish: Option<crate::desktop::wake::Publish>,
    pub published: u64,
}

/// The host's side of a mounted [`DesktopSurface`]: read the tree, publish navigation, queue
/// commands. Cheap to clone; single-threaded.
#[derive(Clone)]
pub struct DesktopHandle {
    pub shared: Rc<RefCell<Shared>>,
}

impl DesktopHandle {
    /// The desktop holding `tree`, minting new ids salted with `salt`, this device's peer salt.
    #[must_use]
    pub fn new(tree: Layout, salt: u16) -> Self {
        let mut minter = tilo_desktop_model::IdMinter::new(salt);
        observe_ids(&mut minter, &tree);
        let search = Rc::new(RefCell::new(tilo_desktop_search::DesktopSearch::new()));
        Self {
            shared: Rc::new(RefCell::new(Shared::new(tree, minter, &search))),
        }
    }

    /// Say in passing what left nothing to see, or why nothing happened: a short-lived notice
    /// over the windows, said politely to screen readers, never a notification.
    pub fn acknowledge(&self, text: impl Into<String>) {
        self.acknowledge_with(ui::window_toolbar::ToolbarNotice::new(
            text,
            ui::widget::toast::ToastKind::Info,
        ));
    }

    /// Say `notice` in passing, as [`Self::acknowledge`] does, with what the person can still do
    /// about it: the shell carries out the buttons it knows, and [`Self::take_notice_actions`]
    /// hands the rest to whoever showed the notice.
    pub fn acknowledge_with(&self, notice: ui::window_toolbar::ToolbarNotice) {
        self.shared.borrow().say(notice);
    }

    /// The buttons of the notice the person pressed that the shell does not carry out itself,
    /// taken by the layer that showed it. One press each, in the order they were made.
    #[must_use]
    pub fn take_notice_actions(&self) -> Vec<String> {
        std::mem::take(&mut *self.shared.borrow().notice_actions.borrow_mut())
    }

    /// The notice the shell says over the windows: the host draws it
    /// ([`ui::widget::toast::NoticeLayer`]) and passes its button presses to
    /// [`Self::press_notice`].
    #[must_use]
    pub fn notice(
        &self,
    ) -> Rc<RefCell<ui::widget::toast::Transient<ui::window_toolbar::ToolbarNotice>>> {
        Rc::clone(&self.shared.borrow().notice)
    }

    /// The person pressed the notice's button for `action`: the notice goes and the action runs.
    /// Whether the notice up offered it.
    pub fn press_notice(&self, action: &str) -> bool {
        let offered = {
            let shared = self.shared.borrow();
            let mut notice = shared.notice.borrow_mut();
            let offered = notice
                .get()
                .is_some_and(|notice| notice.actions.iter().any(|offer| offer.id == action));
            offered && notice.dismiss()
        };
        if offered && action == REOPEN_CLOSED {
            // Undo after a close reopens as Shift+Super+T does, through the same command.
            let _ = self.dispatch(ChromeEffect::ReopenClosed {
                app: None,
                window: None,
            });
        } else if offered {
            // A notice the shell itself put up: whoever showed it takes its buttons.
            self.shared
                .borrow()
                .notice_actions
                .borrow_mut()
                .push(action.to_owned());
        }
        offered
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

    pub fn read<R>(&self, read: impl FnOnce(&Layout) -> R) -> R {
        read(&self.shared.borrow().tree)
    }

    /// The windows other devices host that this shell shows, with the device hosting each: the
    /// windows whose page a host asks the live session for.
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

    /// Call `wake` whenever what this handle commands or changes, or an answer to work the shell
    /// started, waits for the shell's next pass: `wake` asks for the frame that takes it up, however
    /// little of the shell shows.
    pub fn set_wake(&self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.shared.borrow_mut().wake.set(wake);
    }

    /// Have `publish` save and share the tree as soon as a pass changed it: right after the
    /// commands of a pass applied, before the windows they opened are built and drawn, and once
    /// more at the end of the pass.
    pub fn set_publish(&self, publish: Rc<dyn Fn(&DesktopHandle)>) {
        self.shared.borrow_mut().publish = Some(publish);
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

    /// Ask for the shell's next pass: something the shell was told to do waits
    /// for it — a command, a search opening, a card chosen on the empty shell,
    /// carried out on that pass. A page that drew the press and then went quiet
    /// would otherwise carry it out whenever something else woke it.
    pub fn wake(&self) {
        self.shared.borrow().wake.wake();
    }

    /// Admit `command` against the current tree and queue it for the next
    /// update pass, which an accepted command asks for.
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
            (Admitted::Close(ids), _) if keeps(&shared.holders, ids.tab) => {
                let refusal = holder_refusal(&shared.holders, ids.tab);
                shared.say(ui::window_toolbar::ToolbarNotice::new(
                    refusal.clone(),
                    ui::widget::toast::ToastKind::Info,
                ));
                return Err(refusal);
            }
            _ => {}
        }
        shared.queue.push(admitted);
        shared.wake.wake();
        Ok(())
    }

    /// Close `tab`, a window the system opened for something that is now over, on the next pass:
    /// the person did not close it, so nothing offers to undo it. Whether the tab was there.
    pub fn dismiss_window(&self, tab: TabId) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = route_of(&shared.tree, tab) else {
            return false;
        };
        shared.queue.push(Admitted::Dismiss(ids));
        shared.wake.wake();
        true
    }

    /// The logical width of the surface, which window widths are fractions of. Hosts call this on
    /// every resize.
    pub fn set_viewport_width(&self, width: f64) {
        self.shared.borrow_mut().viewport_width = width.max(1.0);
    }

    /// How much of the shell's top edge something overlays now: the desktop bar's height while it
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

    /// Say who else is at each window now, by tab: the members whose pointer stands over it, with
    /// the name and colour their face wears. Whether that changed anything.
    pub fn set_pointer_faces(&self, faces: HashMap<TabId, Vec<(String, String)>>) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.pointer_faces == faces {
            return false;
        }
        shared.pointer_faces = faces;
        shared.changed();
        true
    }

    /// Who else is at window `tab` now, as its tab draws them: each face's name and `#rrggbb`
    /// colour.
    #[must_use]
    pub fn pointer_faces(&self, tab: TabId) -> Vec<(String, String)> {
        self.shared
            .borrow()
            .pointer_faces
            .get(&tab)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_holders(&self, holders: HashMap<TabId, ui::widget::TabHolder>) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.holders == holders {
            return false;
        }
        shared.holders = holders;
        true
    }

    /// Give every window the resource it shows, by the address the workspace's layout keeps: what
    /// its bar carries. Whether that is news.
    pub fn set_window_resources(
        &self,
        resources: std::collections::BTreeMap<TabId, String>,
    ) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.resources == resources {
            return false;
        }
        shared.resources = resources;
        true
    }

    /// Give every window the item it was opened on, as the workspace's layout keeps it: the
    /// window a send landed in, which a restart opens there again. Whether that is news.
    pub fn set_window_items(&self, items: std::collections::BTreeMap<TabId, String>) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.items == items {
            return false;
        }
        shared.items = items;
        true
    }

    /// The item each window was opened on, by tab, as the workspace's layout keeps it
    /// ([`Self::set_window_items`]).
    #[must_use]
    pub fn window_items(&self) -> std::collections::BTreeMap<TabId, String> {
        self.shared.borrow().items.clone()
    }

    /// The resource the window `tab` shows, as the workspace's layout keeps it: a note's address,
    /// empty when it shows none of the workspace's documents.
    #[must_use]
    pub fn window_resource(&self, tab: TabId) -> Option<String> {
        self.shared.borrow().resources.get(&tab).cloned()
    }

    /// Replace the tree with one changed elsewhere. The next update pass reconciles the live rows
    /// with it: tabs still present keep their running content, new tabs get content from the host's
    /// [`TileSource`], and removed tabs' applications close. Queued commands stay queued and are
    /// re-checked against the new tree when applied. A tree changed elsewhere is a shell that moved
    /// ([`Layout::external_revision`]): a window filling the tab held for it, or a window's new
    /// title, reaches the workspace's shared desktop as any other change does.
    pub fn replace_tree(&self, mut tree: Layout) {
        let mut shared = self.shared.borrow_mut();
        observe_ids(&mut shared.minter, &tree);
        tree.external_revision = shared.tree.external_revision.wrapping_add(1);
        // A tree kept beside the state file carries no range of its own: it writes where this
        // shell's ids were minted.
        tree.salt = shared.minter.salt();
        shared.tree = tree;
        shared.replaced = true;
        shared.changed();
    }

    /// Report how tall the place `group` of the workspace `workspace` is drawn now: view state the
    /// person's own canvas says of what it sees, kept on this shell's tree where the canvas reads
    /// it on its next ordinary frame. It changes no window, strip or place, so it neither marks the
    /// tree replaced (which would build every view again) nor counts a change of it: the canvas
    /// that reported it goes on drawing, and a place's free area on the canvas is what the person
    /// sees of it (`canvas.md` §1, §7). Whether it changed anything.
    pub fn report_drawn(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        group: tilo_desktop_model::GroupId,
        height: u32,
    ) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(held) = shared
            .tree
            .workspaces
            .iter_mut()
            .find(|held| held.id() == workspace)
        else {
            return false;
        };
        if !held.layout.set_drawn_height(group, height) {
            return false;
        }
        shared.wake.wake();
        true
    }

    /// The workspace snapshot, excluding process-local stream proxies and device presentation.
    /// Durable profile saves still use the full tree.
    pub fn shared_tree(&self) -> Layout {
        self.read(|tree| crate::desktop::shared_tree::projection(tree.clone()))
    }

    /// Apply workspace topology without replacing this device's stream bindings
    /// or focus. Only `local_tabs` survive omission from the shared snapshot;
    /// removed workspaces and strips still retire their contents.
    pub fn replace_shared_tree(&self, tree: Layout, local_tabs: &[TabId]) {
        let (tree, opened) = self.read(|local| {
            let tree = crate::desktop::shared_tree::receive(local, tree, local_tabs);
            let opened = arrivals::opened_elsewhere(local, &tree, local_tabs);
            (tree, opened)
        });
        self.replace_tree(tree);
        self.note_arrivals(opened, ui::time::now());
    }

    /// Show `view`, the person's view of `workspace`'s layout as they left it: the strip
    /// and group in front, the tab in front of each group, and how wide each group is
    /// ([`tilo_desktop_model::person_view`]). Whether the shell changed.
    pub fn show_view(
        &self,
        view: &tilo_desktop_model::person_view::PersonView,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> bool {
        let mut tree = self.read(Clone::clone);
        if !view.applied(&mut tree, workspace) {
            return false;
        }
        self.replace_tree(tree);
        true
    }

    /// Ask the [`TileSource`] for `tab` 's content again on a coming update pass, as when the tab
    /// was first shown: for a host whose first answer was a notice (its record had not arrived).
    /// The host is asked for that pass, so a window waiting for its record fills in when the record
    /// comes rather than at the next frame something else asks for.
    pub fn rebuild(&self, tab: TabId) {
        let mut shared = self.shared.borrow_mut();
        if shared.rebuild.contains(&tab) {
            return;
        }
        shared.rebuild.push(tab);
        drop(shared);
        self.wake();
    }

    /// `tab` stands in for the app whose window did not come, saying why: what the person sees
    /// where the window would be, and closes when they are done with it. False when the tab is
    /// gone, or holds a live window rather than an app this shell opened.
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

    /// Apply a workspace or strip command to the active workspace as
    /// [`tilo_desktop_model::commands::apply_to_active_workspace`] does, with
    /// ids from the tree's id space. The caller has asked the
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
        let mut shared = self.shared.borrow_mut();
        let Shared { tree, minter, .. } = &mut *shared;
        let made =
            tilo_desktop_model::commands::apply_to_active_workspace(tree, command, &mut || {
                Ok(minter.mint())
            })?;
        // The session publishes the edit as it does any other change from outside the shell.
        tree.external_revision = tree.external_revision.wrapping_add(1);
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

    /// The notifications the host installed on this shell: what the host above the windows draws
    /// as the banners and the centre ([`NotificationLayer`]).
    #[must_use]
    pub fn notifications(&self) -> Option<DesktopNotifications> {
        self.shared.borrow().notifications.clone()
    }

    /// Focus a window of application `app_id` in the active workspace (the focused one when it is
    /// that application's), on the next update pass. False when the workspace has none.
    pub fn focus_app(&self, app_id: &str) -> bool {
        self.tab_of_app(app_id)
            .is_some_and(|tab| self.focus_tab(tab))
    }
}

/// Whether someone else keeps the window `tab`: this person may not close or move it.
pub fn keeps<S: std::hash::BuildHasher>(
    holders: &HashMap<TabId, ui::widget::TabHolder, S>,
    tab: TabId,
) -> bool {
    holders.get(&tab).is_some_and(|holder| holder.keeps)
}

/// Why the window `tab` someone else keeps cannot be closed here.
pub fn holder_refusal<S: std::hash::BuildHasher>(
    holders: &HashMap<TabId, ui::widget::TabHolder, S>,
    tab: TabId,
) -> String {
    holders.get(&tab).map_or_else(
        || "Only whoever opened this window, a member of the workspace or a moderator can close it".to_owned(),
        |holder| {
            format!(
                "Only {}, who opened this window, a member of the workspace or a moderator can close it",
                holder.name
            )
        },
    )
}

/// Answer what the windows asked of the activity with a host that keeps no history for the
/// workspace: each write that it isn't kept, each read that it is still to come, as a native
/// session reads a workspace it is not showing.
pub fn unavailable(requests: &ActivityRequests) {
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
            answer.complete(Err(crate::desktop::i18n::t("activity-served-unavailable")));
        }
    }
    requests.serve(&mut Unkept);
}

pub fn route_of(tree: &Layout, tab: TabId) -> Option<RouteIds> {
    tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group.tabs.iter().any(|t| t.id == tab).then_some(RouteIds {
                    workspace: workspace.id(),
                    strip: strip.id,
                    group: group.id,
                    tab,
                })
            })
        })
    })
}

/// What one app may ask of its host in one frame; more waits for the next.
pub const MAX_HOST_INTENTS: usize = 8;
/// How soon search looks again for an app's answer that is still coming.
pub const SEARCH_ANSWER_POLL: Duration = tilo_desktop_search::ANSWER_POLL;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::seed_tree;

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
        let tree = seed_tree(&mut minter, "Office", vec![tab]);
        DesktopHandle::new(tree, 7)
    }

    /// The shell mints its ids under the device's range, and a tree kept beside the device's state file
    /// carries none of its own: installing the kept tree lays this device's changes in this shell's
    /// range, not in "nobody's".
    #[test]
    fn a_tree_kept_beside_the_state_file_installs_in_this_shells_range() {
        let handle = desktop();
        let minted = handle.read(Clone::clone);
        assert_eq!(minted.salt, 7, "the shell mints in the device's range");
        let mut kept = minted.clone();
        kept.salt = 0;
        handle.replace_tree(kept);
        let installed = handle.read(Clone::clone);
        assert_eq!(
            installed.salt, minted.salt,
            "the tree installed writes in this shell's range"
        );
        assert_eq!(
            installed.workspaces[0].strips[0].id, minted.workspaces[0].strips[0].id,
            "and it is the tree that was kept"
        );
    }

    /// A person names a window: the name stands in the snapshot, an emptied name gives the window's
    /// own title back, and a name too long to show is refused.
    #[test]
    fn a_window_takes_the_name_the_person_gives_it_and_gives_it_back() {
        let handle = desktop();
        let route = handle
            .snapshot("g", vec![])
            .entries
            .first()
            .expect("a window")
            .route
            .clone();
        let label = |handle: &DesktopHandle| {
            handle
                .snapshot("g", vec![])
                .entries
                .iter()
                .find(|entry| entry.route == route)
                .map(|entry| entry.label.clone())
                .unwrap()
        };
        let rename = |name: &str| {
            handle
                .dispatch(ChromeEffect::Rename {
                    tab: None,
                    group: Some(route.group.clone()),
                    strip: None,
                    workspace: None,
                    name: name.into(),
                })
                .unwrap();
            DesktopPass::new(handle.clone()).step(&mut Notices);
        };
        assert_eq!(label(&handle), "App0");
        rename("Quarterly plan");
        assert_eq!(label(&handle), "Quarterly plan");
        rename("  ");
        assert_eq!(label(&handle), "App0", "a blank name clears theirs");
        let refusal = handle
            .dispatch(ChromeEffect::Rename {
                tab: None,
                group: Some(route.group),
                strip: None,
                workspace: None,
                name: "x".repeat(navigation::MAX_NAME_CHARS + 1),
            })
            .unwrap_err();
        assert!(refusal.contains("too long"), "{refusal}");
    }

    struct Notices;

    impl TileSource for Notices {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }
}
