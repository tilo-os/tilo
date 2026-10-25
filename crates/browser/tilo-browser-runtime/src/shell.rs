//! The desktop's chrome in the page, from `tilo-desktop`'s widgets: the
//! native shell's backdrop, its top bar over the strip, the launchpad on a
//! desk with no windows yet, the overview panel, and the menu behind
//! the bar's Tilo mark with the About sheet saying what this is.
//!
//! Desktop chords are the native shell's, read from the same shortcut
//! table ([`tilo_desktop::shortcuts`]), where the browser passes Super
//! through, and the same keys with the page's stand-in for Super, which no
//! browser or system binds: Alt+Shift, or Ctrl+Option on a Mac (Option+Shift
//! selects by words there). Super+Tab, the overview, is the system's in most
//! browsers, so O stands in for its Tab. Chords the page has no use for
//! (locking, the magnifier, …) reach the window as ordinary keys.
//!
//! The bar reads a [`DesktopChromeSource`] the page answers from the
//! surface's tree and the page's clock; what it asks for comes back as
//! [`ChromeIntent`]s, applied as the surface's own navigation commands on the
//! next update pass. What only the embedding page can do (inviting someone)
//! is queued as a [`HostRequest`] for the mount's status.
//!
//! Who is here comes from the page ([`crate::here`]): their avatars join
//! the bar's from a copy of the tree, the bar's avatars open the People
//! popover with each person's place, status and recent summary, and the
//! windows they are in carry their faces. Pinned, the roster floats over
//! the windows as the people overlay, and following someone tracks their
//! view ([`people`]).

mod people;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use settings_store::hardware::{ShortcutAction, ShortcutConfig};
use tilo_desktop::about::{AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    AvatarAsk, ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
};
use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, LineSource, RecentChange,
    RosterRow, RosterStatus,
};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction, LaunchpadCopy};
use tilo_desktop::overview::{OverviewHost, room_cards};
use tilo_desktop::overview_panel::{OverviewPanel, OverviewPick};
use tilo_desktop::profile_card::{
    ProfileAction, ProfileCard, ProfileChoice, ProfileGroup, ProfileModel,
};
use tilo_desktop::shortcuts::{page_chord, page_shortcut_action};
use tilo_desktop::strip_navigator::{StripFrame, StripNavigation, StripNavigator, Viewer};
use tilo_desktop::strip_switcher::Command as StripCommand;
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop::window_switcher::WindowSource;
use tilo_desktop::workspace_menu::{
    WorkspaceAction, WorkspaceChoice, WorkspaceMenu, WorkspaceMenuChoice, WorkspaceMenuModel,
};
use tilo_desktop_model::commands::{DesktopCommand, apply_to_active_workspace};
use tilo_desktop_model::{DesktopTree, Strip, StripId, TabId, Workspace, WorkspaceId};
use tilo_desktop_surface::HostActionSource;
use tilo_desktop_surface::navigation::{ActivityView, Command};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::widget::{AppModel, EventCtx, ModelPath, Stack, TaffyTreeExt as _, Widget};
use ui::winit::event::{ElementState, Modifiers, WindowEvent};
use ui::{TextRenderer, Theme};

/// What the desktop asks of the page that embeds it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HostRequest {
    /// Invite someone into this workspace (`mountDesktop({onInvite})`).
    Invite,
    /// Perform the host action the person chose in search or on a banner
    /// (`mountDesktop({hostActions, onHostAction})`), by id.
    Action(String),
    /// Go by this name, as the person typed it in their profile card: the
    /// page's naming action, already answered (`onHostAction('set-name',
    /// name)`).
    Named(String),
    /// Have the room's host run an action of a room record for the person
    /// (`peer.actions`), and answer it with `desktop_room_action_answer`.
    RoomAction(crate::desktop::RoomActionRequest),
}

/// The page's host action that lets the person choose their name; the
/// People panel offers it on the own name when the page lists it.
const RENAME_ACTION: &str = "set-name";
/// The avatar menu's "Report a problem…", which the page answers itself.
const REPORT_PROBLEM: &str = "report-problem";

/// Workspace menu keys: a workspace of this page's desktop, by id, and one
/// of the person's others the page opens, by its host action.
const WORKSPACE_KEY: &str = "workspace:";
const HOST_KEY: &str = "host:";

/// The popovers the bar opens, one at a time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BarPopover {
    Tilo,
    Workspace,
    Profile,
    People,
    /// The ask about the person, open beside their avatar.
    Ask,
    /// None of them: the page's own invite sheet takes over.
    Invite,
}

/// Requests the desktop queued since the page last read its status.
pub(crate) type HostRequests = Rc<RefCell<Vec<HostRequest>>>;

/// What the embedding page offers the desktop.
#[derive(Clone, Debug, Default)]
pub(crate) struct HostOffers {
    /// The page takes [`HostRequest::Invite`].
    pub invite: bool,
    /// The page runs on macOS: its command key is Cmd.
    pub mac: bool,
    /// The host of the room shown (`example.org`), named beside the
    /// workspace; `None` on the person's own desktop.
    pub host: Option<String>,
    /// The deployment's links the About sheet offers.
    pub about: tilo_desktop::about::AboutLinks,
    /// The build this page runs (its commit), which a report names.
    pub build: Option<String>,
}

/// Who this desktop's person is when they make something in the room,
/// and who made what there.
#[derive(Clone, Default)]
pub(crate) struct Room {
    /// Who made each strip and opened each window.
    pub authors: crate::desktop::Authors,
    /// The person this desktop acts for.
    pub author: String,
}

/// The bar's view of the page: the surface's tree, the wall clock, and the
/// intents it raised since the last update pass.
struct PageChrome {
    handle: DesktopHandle,
    shared: crate::desktop::Shared,
    here: tilo_desktop::here::Here,
    room: Room,
    notifications: tilo_desktop_surface::DesktopNotifications,
    intents: RefCell<Vec<ChromeIntent>>,
    /// `(tree revision, minute, who-is-here revision, notifications
    /// revision, standing)` the snapshot revision was last bumped for.
    seen: Cell<(u64, i64, u64, u64, u8)>,
    revision: Cell<u64>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The page takes invitations ([`HostOffers::invite`]): the bar keeps
    /// its invite button.
    invite: bool,
}

impl DesktopChromeSource for PageChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = clock_snapshot();
        let seen = (
            self.handle.revision(),
            clock.unix_seconds.div_euclid(60),
            self.here.borrow().revision(),
            self.notifications.revision(),
            self.shared.standing.code(),
        );
        if self.seen.get() != seen {
            self.seen.set(seen);
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            clock,
            watching: self.here.borrow().watching(),
            own: Some(self.own_chip()),
            // The page has no quick settings: the cluster opens the
            // notification centre, so it is that centre's bell.
            notifications_only: true,
            notification_badge: u32::try_from(self.notifications.unread()).unwrap_or(u32::MAX),
            invite: tilo_desktop::chrome::Invite::when(self.may_invite()),
            waved: self.here.borrow().wave_notice(),
            ..DesktopChromeSnapshot::default()
        }
    }

    /// The surface's tree with who is here as the active workspace's
    /// collaborators, for the bar's avatars.
    fn tree(&self) -> DesktopTree {
        let mut tree = self.handle.read(Clone::clone);
        let people = self.people(&tree);
        tilo_desktop::here::overlay_collaborators(&mut tree, &people);
        tree
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        (self.wake)();
        true
    }

    /// What the page asks about the person themselves (keeping who they
    /// are), waiting on their avatar rather than as a banner over windows.
    fn avatar_ask(&self) -> Option<AvatarAsk> {
        self.handle.personal_ask().map(|ask| AvatarAsk {
            question: ask.title,
            detail: ask.body,
            answer: ask.action.title,
            open: ask.open,
        })
    }
}

impl PageChrome {
    /// Whether the bar offers its invite button: the page takes invitations
    /// and the room lets this person write in it. Someone it lets only look,
    /// a spectator among them, brings nobody in; until the room answered,
    /// the button stands, as for everyone it lets in.
    fn may_invite(&self) -> bool {
        self.invite
            && self
                .shared
                .standing
                .role()
                .is_none_or(|role| role.includes(tilo_action_policy::Role::Editor))
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        let members = self.shared.members.borrow();
        let standing = self.shared.standing_of(&self.room.author, members.as_ref());
        let authors = self.room.authors.borrow();
        self.handle.read(|tree| {
            let workspace = tree
                .active_workspace()
                .ok_or_else(|| "There is no workspace open".to_owned())?;
            tilo_desktop_model::commands::may_command(&standing, &authors, workspace, command)
                .map_err(|refusal| refusal.to_string())
        })
    }

    /// Whether this desktop's person may add a workspace to the room, by the
    /// desktop's one policy.
    fn may_create_workspace(&self) -> Result<(), String> {
        let members = self.shared.members.borrow();
        let standing = self.shared.standing_of(&self.room.author, members.as_ref());
        tilo_desktop_model::commands::may_create_workspace(&standing)
            .map_err(|refusal| refusal.to_string())
    }

    /// What this desktop's person may change about the active workspace.
    fn offered_edits(&self) -> OfferedEdits {
        let members = self.shared.members.borrow();
        let standing = self.shared.standing_of(&self.room.author, members.as_ref());
        let authors = self.room.authors.borrow();
        self.handle
            .read(|tree| offered_edits(&standing, &authors, tree))
    }

    fn people(&self, tree: &DesktopTree) -> Vec<tilo_desktop::here::Person> {
        here_people(&self.here, &self.shared, tree)
    }

    /// This page's person as everyone here sees them: their name (numbered
    /// when someone else here shares it) and colour.
    fn own_chip(&self) -> tilo_desktop::chrome::OwnChip {
        let (name, rgb) = own_identity(&self.here.borrow());
        tilo_desktop::chrome::OwnChip { name, rgb }
    }
}

/// This page's person as everyone here sees them, and as everything on the
/// page names them: their name, a guest's while they gave none, and colour.
pub(crate) fn own_identity(here: &tilo_desktop::here::HereState) -> (String, (u8, u8, u8)) {
    let name = here
        .own_shown()
        .unwrap_or_else(tilo_desktop::here::guest_label);
    let rgb = here
        .own_rgb()
        .unwrap_or_else(|| ui::presence::user_color(&name));
    (name, rgb)
}

/// What a person may change about a workspace, as its menu and the bar
/// offer it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OfferedEdits {
    /// Rename the workspace.
    rename: bool,
    /// Add a desk (a strip) to it.
    add_desk: bool,
}

/// What `standing` may change about `tree`'s active workspace, whose strips
/// and windows `authors` made: asked of the desktop's one policy, so what
/// is offered is what would be allowed.
fn offered_edits(
    standing: &tilo_desktop_model::commands::Standing<'_>,
    authors: &BTreeMap<u64, String>,
    tree: &DesktopTree,
) -> OfferedEdits {
    let Some(workspace) = tree.active_workspace() else {
        return OfferedEdits::default();
    };
    let may = |command: DesktopCommand| {
        tilo_desktop_model::commands::may_command(standing, authors, workspace, &command).is_ok()
    };
    OfferedEdits {
        rename: may(DesktopCommand::RenameWorkspace {
            name: String::new(),
        }),
        add_desk: may(DesktopCommand::CreateStrip {
            name: String::new(),
        }),
    }
}

/// Who is here, placed in `tree` and the records its windows show.
pub(crate) fn here_people(
    here: &tilo_desktop::here::Here,
    shared: &crate::desktop::Shared,
    tree: &DesktopTree,
) -> Vec<tilo_desktop::here::Person> {
    let records: Vec<(TabId, String)> = shared
        .bound_records()
        .into_iter()
        .map(|(tab, checkout)| (tab, checkout.id.as_str().to_owned()))
        .collect();
    let app_name = |app: &str| crate::desktop::known_application(app).map(str::to_owned);
    here.borrow().people(tree, &records, &app_name, now_ms())
}

/// Unix milliseconds.
pub(crate) fn now_ms() -> u64 {
    ui::time::unix_now_ms()
}

/// One person as the People popover lists them.
fn roster_row(person: &tilo_desktop::here::Person, theme: &Theme) -> RosterRow {
    let status = |line: &tilo_desktop::here::Line| RosterStatus {
        text: line.text.clone(),
        source: LineSource::generated_if(line.automatic),
    };
    RosterRow {
        id: person.id.clone(),
        label: person.label.clone(),
        initial: person
            .label
            .chars()
            .next()
            .map_or('\u{2022}', |c| c.to_uppercase().next().unwrap_or(c)),
        color: theme.tokens.colors.presence.of_rgb(person.rgb),
        is_agent: person.agent,
        is_bot: false,
        role: None,
        invited: false,
        activity: person.place.clone(),
        status: person.status.as_ref().map(status),
        // A page has no History entries to open: what they did recently
        // is one line, opening the workspace's History.
        recent: person
            .recent
            .iter()
            .map(|line| RecentChange {
                text: line.text.clone(),
                when: String::new(),
                entry: None,
            })
            .collect(),
        // Following brings forward, here, the window of what they are in.
        followable: true,
        availability: ui::Availability::Available,
        call: None,
        join: None,
        own_device: false,
    }
}

/// The wall clock's minute, which the bar shows.
fn minute() -> i64 {
    ui::time::unix_now_i64().div_euclid(60)
}

/// The page's wall clock as the bar shows it.
fn clock_snapshot() -> ClockSnapshot {
    let now = ui::clock::local_now();
    ClockSnapshot {
        unix_seconds: now.timestamp(),
        clock: ui::clock::time_of_day(&now),
        weekday: now.format("%a").to_string(),
        date: now.format("%-d %B").to_string(),
    }
}

/// The launchpad, shown while the desk shown has no windows.
struct EmptyWorkspace {
    launchpad: CollaborationLaunchpad,
    shown: Rc<Cell<bool>>,
}

impl Widget for EmptyWorkspace {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.launchpad.build_layout(taffy, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.launchpad.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.shown.get() {
            self.launchpad.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if self.shown.get() {
            self.launchpad.handle_event(event, ctx);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.shown.get() {
            vec![&self.launchpad]
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.shown.get() {
            vec![&mut self.launchpad]
        } else {
            Vec::new()
        }
    }

    fn needs_redraw(&self) -> bool {
        self.shown.get() && self.launchpad.needs_redraw()
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "EmptyWorkspace"
    }

    fn get_rect(&self) -> Rect {
        if self.shown.get() {
            self.launchpad.get_rect()
        } else {
            Rect::ZERO
        }
    }
}

/// The desktop the page mounts: backdrop, top bar, and the strip (with the
/// launchpad beneath it while the desk shown is empty).
pub(crate) struct BrowserShell {
    backdrop: tilo_desktop::backdrop::DesktopBackdrop,
    bar: TopBarChrome,
    area: Stack,
    overview: OverviewPanel,
    tilo_menu: TiloMenu,
    /// The tree revision the open overview shows.
    overview_revision: u64,
    /// The minute the bar's clock last showed.
    drawn_minute: Cell<i64>,
    rect: Rect,
    chrome: Rc<PageChrome>,
    handle: DesktopHandle,
    generation: String,
    empty: Rc<Cell<bool>>,
    /// Whether the bar last drew its Tilo mark pressed (its menu open).
    menu_shown: Cell<bool>,
    offers: HostOffers,
    requests: HostRequests,
    workspace_menu: WorkspaceMenu,
    profile: ProfileCard,
    /// The People popover the bar's avatars open.
    people: CollaboratorsOverlay,
    people_open: bool,
    /// The roster pinned out of the popover, and following someone.
    pinned: people::PagePeople,
    /// The linked accounts' revision the card last showed.
    linked_seen: Cell<u64>,
    /// Asks again while the card shows a code, until the account links.
    linking: tilo_app::linked_accounts::LinkingRecheck,
    /// The person whose recent summary the popover shows.
    expanded: Option<String>,
    /// The pointer, in logical pixels, for the popover's targets.
    cursor: Option<Point>,
    /// The desktop's shortcut table, the native shell's defaults: the page
    /// keeps no device settings to rebind them in.
    shortcuts: ShortcutConfig,
    /// Where the person is in the workspace (the strip ribbon grown into
    /// the strip switcher, and the window rail), the native shell's.
    navigator: StripNavigator,
    /// Where the navigator's ribbon and rail stand and what they report.
    navigator_frame: StripFrame,
    /// What the navigator last read: the navigation and who-is-here
    /// revisions, whether the overview covered the desktop, and the content
    /// area.
    navigator_seen: Option<(u64, u64, bool, Rect)>,
    /// Pointing at what is wrong, to report it.
    picker: crate::feedback_pick::PagePicker,
}

/// What the top bar's chrome reads from the page besides the desktop tree.
pub struct ChromeSources {
    pub here: tilo_desktop::here::Here,
    pub shared: crate::desktop::Shared,
    pub room: Room,
    /// The page's notifications, whose unread ones the bell counts.
    pub notifications: tilo_desktop_surface::DesktopNotifications,
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

impl BrowserShell {
    pub fn new(
        surface: DesktopSurface,
        handle: DesktopHandle,
        generation: String,
        offers: HostOffers,
        requests: HostRequests,
        sources: ChromeSources,
    ) -> Self {
        let ChromeSources {
            here,
            shared,
            room,
            notifications,
            wake,
        } = sources;
        let chrome = Rc::new(PageChrome {
            handle: handle.clone(),
            shared,
            here,
            room,
            notifications,
            intents: RefCell::default(),
            seen: Cell::new((u64::MAX, 0, 0, 0, u8::MAX)),
            revision: Cell::new(0),
            wake,
            invite: offers.invite,
        });
        let tree = handle.read(Clone::clone);
        let bar = TopBarChrome::from_tree(&tree)
            .with_chrome(chrome.clone())
            .with_tilo_mark();
        let mut tilo_menu = TiloMenu::new(AboutVariant::Browser, &offers.about);
        // Offered only where the page holds a destination for reports.
        if chrome.shared.feedback.borrow().is_some() {
            tilo_menu = tilo_menu.with_feedback();
        }
        let empty = Rc::new(Cell::new(desk_is_empty(&tree)));
        let launchpad = CollaborationLaunchpad::new().with_copy(welcome_copy(offers.mac));
        let area = Stack::new()
            .push(Box::new(EmptyWorkspace {
                launchpad,
                shown: Rc::clone(&empty),
            }))
            .push(Box::new(surface));
        Self {
            backdrop: tilo_desktop::backdrop::DesktopBackdrop::new(),
            bar,
            area,
            overview: OverviewPanel::new(),
            tilo_menu,
            overview_revision: 0,
            drawn_minute: Cell::new(i64::MIN),
            rect: Rect::ZERO,
            chrome,
            handle,
            generation,
            empty,
            menu_shown: Cell::new(false),
            offers,
            requests,
            workspace_menu: WorkspaceMenu::new(),
            profile: ProfileCard::new(),
            people: CollaboratorsOverlay::default(),
            people_open: false,
            pinned: people::PagePeople::new(),
            linked_seen: Cell::new(0),
            linking: tilo_app::linked_accounts::LinkingRecheck::default(),
            expanded: None,
            cursor: None,
            shortcuts: ShortcutConfig::default(),
            navigator: StripNavigator::default(),
            navigator_frame: StripFrame::empty(Rect::ZERO),
            navigator_seen: None,
            picker: crate::feedback_pick::PagePicker::default(),
        }
    }

    fn draw_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tree = self.handle.read(Clone::clone);
        let rows: Vec<RosterRow> = self
            .chrome
            .people(&tree)
            .iter()
            .map(|person| roster_row(person, theme))
            .collect();
        if self
            .expanded
            .as_ref()
            .is_some_and(|id| !rows.iter().any(|row| row.id == *id))
        {
            self.expanded = None;
        }
        let watching = usize::try_from(self.chrome.here.borrow().watching()).unwrap_or(usize::MAX);
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|w| w.name.clone()))
            .unwrap_or_default();
        self.people.draw(
            scene,
            transform,
            text,
            theme,
            &CollaboratorsModel {
                host: self.rect,
                workspace: &workspace,
                rows: &rows,
                unavailable: None,
                exposure: None,
                may_invite: self.chrome.may_invite(),
                may_pin: !self.pinned.is_pinned(),
                followed_id: self.pinned.followed(),
                following_pairs: &[],
                expanded_id: self.expanded.as_deref(),
                watching,
                waved: &self.chrome.here.borrow().waved(now_ms()),
                spectating: self.chrome.shared.watching.get(),
            },
            self.cursor,
        );
    }

    /// The pinned roster and the following banner, over the windows and
    /// under what the bar opens. While following, the window of what they
    /// are in comes forward here first.
    fn draw_pinned_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tree = self.handle.read(Clone::clone);
        let people = self.chrome.people(&tree);
        let (handle, generation) = (&self.handle, &self.generation);
        // As following a link to their app does: its window here, else one.
        let open_app = |app: &str| {
            crate::desktop::known_application(app).is_some()
                && handle
                    .dispatch(
                        &Command::Open {
                            generation: generation.clone(),
                            app: app.to_owned(),
                        },
                        generation,
                    )
                    .is_ok()
        };
        if self.pinned.track(&people, handle, &open_app) {
            (self.chrome.wake)();
        }
        let rows: Vec<RosterRow> = people
            .iter()
            .map(|person| roster_row(person, theme))
            .collect();
        // Every window here, by the record it shows as the live layer names
        // it: a window on someone's screen showing one of these is shared.
        let records: Vec<(TabId, String, String)> = tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| {
                let (app, record) = self.chrome.shared.live_record_of(&tree, tab.id)?;
                Some((tab.id, app, record))
            })
            .collect();
        self.pinned.draw(
            scene,
            transform,
            text,
            theme,
            &people::Frame {
                rect: self.rect,
                rows: &rows,
                people: &people,
                tree: &tree,
                records: &records,
                cursor: self.cursor,
                content_top: self
                    .handle
                    .shown_tabs()
                    .iter()
                    .map(|(_, rect)| rect.y0)
                    .fold(0.0, f64::max),
            },
        );
    }

    /// The pinned roster's and following's share of `event`: what lands on
    /// the overlay or the banner, the message box's keys, and the person's
    /// own input, which takes their view back from whom they follow.
    /// Returns whether the event was taken.
    fn pinned_people_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let (taken, hit) = self.pinned.key(event, ctx.modifiers);
        if let Some(hit) = hit {
            self.apply_pinned_hit(hit);
        }
        if taken {
            *ctx.handled = true;
            return true;
        }
        let mut handled = false;
        if let Some(hit) =
            self.pinned
                .handle_event(event, ctx.scale_factor, self.cursor, &mut handled)
        {
            self.apply_pinned_hit(hit);
        }
        if handled {
            *ctx.handled = true;
            return true;
        }
        // A press on the desktop itself, not the bar: the person's own.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && self
                .cursor
                .is_some_and(|point| point.y >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX))
        {
            self.pinned.pressed_elsewhere();
        }
        false
    }

    fn apply_pinned_hit(&mut self, hit: tilo_desktop::people_overlay::PeopleHit) {
        use tilo_desktop::people_overlay::PeopleHit;
        match hit {
            PeopleHit::ToggleFollow(id) => self.pinned.toggle_follow(&id),
            PeopleHit::Unpin => self.pinned.pin(false),
            // The page has no direct messages: the box says so instead of
            // offering to send, and the edge was kept as it moved.
            PeopleHit::Send { .. } | PeopleHit::Moved(_) => {}
        }
        (self.chrome.wake)();
    }

    fn apply_people_hit(&mut self, hit: CollaboratorsHit) {
        match hit {
            CollaboratorsHit::Dismiss => self.people_open = false,
            CollaboratorsHit::Invite => {
                self.people_open = false;
                self.invite();
            }
            // History at what this person did, as the native panel opens it.
            CollaboratorsHit::OpenChange(index, _) | CollaboratorsHit::OpenChanges(index) => {
                let author = self.people.changes_author(index).map(str::to_owned);
                self.people_open = false;
                self.activity_by(author);
            }
            CollaboratorsHit::ToggleDetails(index) => {
                let id = self.people.details_target(index).map(str::to_owned);
                self.expanded = if self.expanded == id { None } else { id };
            }
            CollaboratorsHit::Wave(index) => {
                // Through the room's live presence: the person waved at
                // sees it, and the row shows it sent.
                if let Some(target) = self.people.wave_target(index).map(str::to_owned)
                    && self.chrome.here.borrow_mut().wave_at(&target, now_ms())
                {
                    (self.chrome.wake)();
                }
            }
            CollaboratorsHit::ToggleFollow(index) => {
                if let Some(id) = self.people.follow_target(index).map(str::to_owned) {
                    self.pinned.toggle_follow(&id);
                }
            }
            CollaboratorsHit::Pin => {
                self.people_open = false;
                self.people.clear();
                self.pinned.pin(true);
            }
            CollaboratorsHit::JoinCall(_) | CollaboratorsHit::Absorb => {}
        }
    }

    fn dispatch(&self, command: &Command) {
        if let Err(error) = self.handle.dispatch(command, &self.generation) {
            web_sys::console::warn_1(&format!("desktop chrome: {error}").into());
        }
    }

    fn active_workspace(&self) -> Option<String> {
        self.handle
            .read(|tree| tree.active_workspace().map(|w| w.id.0.to_string()))
    }

    fn launch(&self, app: &str) {
        if crate::desktop::known_application(app).is_some() {
            self.dispatch(&Command::Launch {
                generation: self.generation.clone(),
                app: app.into(),
            });
        } else {
            self.handle.open_search(None);
        }
    }

    /// Feedback about the window in front, in the one private composer.
    fn open_feedback(&self) {
        let focused = self.handle.read(|tree| {
            tilo_desktop::feedback::focused(tree, |app| {
                crate::desktop::known_application(app).map(str::to_owned)
            })
        });
        let subject = match focused {
            // Asked again from the composer: it keeps its subject.
            tilo_desktop::feedback::Focused::Composer => None,
            tilo_desktop::feedback::Focused::Nothing => Some(None),
            tilo_desktop::feedback::Focused::App(subject) => Some(Some(subject)),
        };
        if let (Some(subject), Some(feedback)) =
            (subject, self.chrome.shared.feedback.borrow().as_ref())
            && let Err(error) = feedback.about(subject)
        {
            web_sys::console::warn_1(&format!("feedback: {error}").into());
        }
        match self.handle.read(tilo_desktop::feedback::composer_tab) {
            Some((.., tab)) => {
                self.handle.focus_tab(tab);
            }
            None => self.launch(tilo_desktop::feedback::PACKAGE),
        }
    }

    /// Point at what is wrong: the picker, where the page holds a
    /// destination for reports.
    fn report_problem(&mut self) {
        if self.chrome.shared.feedback.borrow().is_some() {
            self.profile.close();
            self.picker.open();
        }
    }

    /// The windows on screen, as a report names what is in them.
    fn pick_windows(&self) -> Vec<crate::feedback_pick::PickWindow> {
        let records = self.chrome.shared.bound_records();
        let shown = self.handle.shown_tabs();
        self.handle.read(|tree| {
            shown
                .into_iter()
                .filter_map(|(tab, rect)| {
                    let subject = tilo_desktop::feedback::tab_subject(tree, tab, |app| {
                        crate::desktop::known_application(app).map(str::to_owned)
                    })?;
                    Some(crate::feedback_pick::PickWindow {
                        rect,
                        instance: tab,
                        app: subject.app,
                        name: subject.name,
                        record: records
                            .iter()
                            .find(|(shown, _)| *shown == tab)
                            .map(|(_, checkout)| checkout.id.as_str().to_owned()),
                    })
                })
                .collect()
        })
    }

    /// Hand what the person picked to the composer, and open it on it.
    fn follow_picker(&mut self) -> bool {
        let names = {
            let tree = self.chrome.tree();
            let mut names: Vec<String> = self
                .chrome
                .people(&tree)
                .into_iter()
                .map(|person| person.label)
                .collect();
            names.push(self.chrome.own_chip().name);
            names
        };
        let cx = crate::feedback_pick::ReportContext {
            windows: self.pick_windows(),
            build: self.offers.build.clone(),
            names,
        };
        if let Some((draft, crops)) = self.picker.take_report(&self.area, &cx) {
            // The frame drawn next has no picker over it: the parts'
            // pictures are copied from it, then the composer opens.
            crate::feedback_crops::want(crate::feedback_crops::Wanted {
                draft,
                crops,
                wake: Arc::clone(&self.chrome.wake),
            });
            return true;
        }
        let Some(report) = crate::feedback_crops::take_ready() else {
            return false;
        };
        let started = self
            .chrome
            .shared
            .feedback
            .borrow()
            .as_ref()
            .map(|feedback| feedback.services().begin_report(report));
        match started {
            Some(Ok(())) => match self.handle.read(tilo_desktop::feedback::composer_tab) {
                Some((.., tab)) => {
                    self.handle.focus_tab(tab);
                }
                None => self.launch(tilo_desktop::feedback::PACKAGE),
            },
            Some(Err(error)) => {
                web_sys::console::warn_1(&format!("feedback: {error}").into());
            }
            None => {}
        }
        true
    }

    fn activity(&self) {
        self.activity_by(None);
    }

    /// History, showing only what `author` did when one is named.
    fn activity_by(&self, author: Option<String>) {
        if let Some(workspace) = self.active_workspace() {
            self.dispatch(&Command::WorkspaceActivity {
                generation: self.generation.clone(),
                workspace,
                view: ActivityView::CatchUp,
                author,
            });
        }
    }

    fn invite(&self) {
        if self.offers.invite {
            self.requests.borrow_mut().push(HostRequest::Invite);
        }
    }

    fn apply_intent(&mut self, intent: ChromeIntent) {
        match intent {
            ChromeIntent::SwitchWorkspace(delta) => {
                let next = self.handle.read(|tree| {
                    let index = tree.workspaces.iter().position(|w| w.id == tree.active)?;
                    let next = index.checked_add_signed(isize::try_from(delta).ok()?)?;
                    tree.workspaces.get(next).map(|w| w.id.0.to_string())
                });
                if let Some(workspace) = next {
                    self.dispatch(&Command::Workspace {
                        generation: self.generation.clone(),
                        workspace,
                    });
                }
            }
            ChromeIntent::ShowActivity(_) => self.activity(),
            ChromeIntent::ToggleQuickSettings | ChromeIntent::ShowQuickSettings => {
                let open = self
                    .handle
                    .presentation()
                    .notifications
                    .is_some_and(|n| n.center_open);
                self.dispatch(&Command::NotificationCenter {
                    generation: self.generation.clone(),
                    open: !open || matches!(intent, ChromeIntent::ShowQuickSettings),
                });
            }
            ChromeIntent::ToggleCollaborators => {
                self.close_bar_popovers_except(BarPopover::People);
                self.people_open = !self.people_open && !self.tilo_menu.is_open();
                if !self.people_open {
                    self.people.clear();
                }
            }
            ChromeIntent::Invite => {
                self.close_bar_popovers_except(BarPopover::Invite);
                self.invite();
            }
            ChromeIntent::ShowPerson(id) => {
                // Who is here, open at them: a second press on the same
                // face closes it, as the list's own toggle does.
                let again = self.people_open && self.expanded.as_deref() == Some(id.as_str());
                self.close_bar_popovers_except(BarPopover::People);
                self.people_open = !again && !self.tilo_menu.is_open();
                self.expanded = self.people_open.then_some(id);
                if !self.people_open {
                    self.people.clear();
                }
            }
            ChromeIntent::ToggleWorkspaceMenu => {
                self.close_bar_popovers_except(BarPopover::Workspace);
                let model = self.workspace_menu_model();
                self.workspace_menu
                    .toggle(&model, self.bar.workspace_menu_rect(), self.rect);
            }
            ChromeIntent::OpenAvatarAsk => {
                self.close_bar_popovers_except(BarPopover::Ask);
                self.handle.set_personal_ask_open(true);
            }
            ChromeIntent::CollapseAvatarAsk => {
                self.handle.set_personal_ask_open(false);
            }
            ChromeIntent::AnswerAvatarAsk => self.handle.answer_personal_ask(),
            ChromeIntent::PutOffAvatarAsk => self.handle.put_off_personal_ask(),
            ChromeIntent::ToggleProfile => {
                self.close_bar_popovers_except(BarPopover::Profile);
                if !self.profile.is_open() {
                    // Where the person stands with each connected service
                    // is asked of the room's host; the card shows it once
                    // answered.
                    self.chrome.shared.linked.refresh();
                }
                let model = self.profile_model();
                self.profile
                    .toggle(&model, self.bar.own_avatar_rect(), self.rect);
            }
            ChromeIntent::ToggleOverview => self.toggle_overview(),
            ChromeIntent::RenameWorkspace { name } => self.rename_workspace(name),
            ChromeIntent::ToggleTiloMenu => {
                // One popover from the bar at a time.
                self.close_bar_popovers_except(BarPopover::Tilo);
                self.tilo_menu.toggle(self.bar.tilo_mark_rect(), self.rect);
            }
            // The bar asks for these only for what this page never shows it,
            // so none reaches here: no clock overlay or clock activity in
            // the snapshot (`HideClock`, `ClockCommand`), no continuity offer
            // from another device (`AcceptContinuity`, `DismissContinuity`),
            // and no tray items from running applications (`AppTray`).
            // Listed rather than caught by a wildcard, so a new request is
            // handled or explained here.
            ChromeIntent::HideClock
            | ChromeIntent::ClockCommand { .. }
            | ChromeIntent::AcceptContinuity
            | ChromeIntent::DismissContinuity
            | ChromeIntent::AppTray { .. } => {}
            // The strip's name in the bar opens the strip switcher, as a
            // menu on the shown strip.
            ChromeIntent::ShowStrips => self
                .navigator
                .open_for_choosing(&self.navigator_frame, ui::time::now()),
        }
    }

    /// Close every popover the bar opens but `keep`: one at a time.
    fn close_bar_popovers_except(&mut self, keep: BarPopover) {
        if keep != BarPopover::People && self.people_open {
            self.people_open = false;
            self.people.clear();
        }
        if keep != BarPopover::Tilo && self.tilo_menu.is_menu_open() {
            self.tilo_menu.toggle(Rect::ZERO, self.rect);
        }
        if keep != BarPopover::Workspace {
            self.workspace_menu.close();
        }
        if keep != BarPopover::Profile {
            self.profile.close();
        }
        if keep != BarPopover::Ask {
            // The ask waits as the avatar's dot while another popover is up.
            self.handle.set_personal_ask_open(false);
        }
    }

    /// What the workspace menu offers now: this page's workspaces and the
    /// person's others the page knows, where it is hosted, the page's own
    /// workspace commands, and what this person may change.
    fn workspace_menu_model(&self) -> WorkspaceMenuModel {
        let actions = self.handle.host_actions();
        let mut workspaces = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| WorkspaceChoice {
                    key: format!("{WORKSPACE_KEY}{}", workspace.id.0),
                    name: workspace.name.clone(),
                    current: workspace.id == tree.active,
                })
                .collect::<Vec<WorkspaceChoice>>()
        });
        let edits = self.chrome.offered_edits();
        workspaces.extend(
            actions
                .iter()
                .filter(|action| action.source == Some(HostActionSource::Workspaces))
                .map(|action| WorkspaceChoice {
                    key: format!("{HOST_KEY}{}", action.id),
                    name: action.label.clone().unwrap_or_else(|| action.title.clone()),
                    current: false,
                }),
        );
        WorkspaceMenuModel {
            workspaces,
            host: self.offers.host.clone(),
            actions: actions
                .iter()
                .filter(|action| action.source == Some(HostActionSource::Workspace))
                .map(|action| WorkspaceAction {
                    id: action.id.clone(),
                    title: action.label.clone().unwrap_or_else(|| action.title.clone()),
                    destructive: action.id.starts_with("leave"),
                })
                .collect(),
            can_rename: edits.rename,
            can_add_strip: edits.add_desk,
        }
    }

    fn apply_workspace_choice(&mut self, choice: WorkspaceMenuChoice) {
        match choice {
            WorkspaceMenuChoice::Switch(key) => {
                if let Some(workspace) = key.strip_prefix(WORKSPACE_KEY) {
                    self.dispatch(&Command::Workspace {
                        generation: self.generation.clone(),
                        workspace: workspace.to_owned(),
                    });
                } else if let Some(id) = key.strip_prefix(HOST_KEY) {
                    self.requests
                        .borrow_mut()
                        .push(HostRequest::Action(id.to_owned()));
                }
            }
            WorkspaceMenuChoice::Action(id) => {
                self.requests.borrow_mut().push(HostRequest::Action(id));
            }
            WorkspaceMenuChoice::Overview => {
                if !self.overview.is_open() {
                    self.toggle_overview();
                }
            }
            // Made in the strip switcher, the one place strips are.
            WorkspaceMenuChoice::NewStrip => {
                if let Some(navigation) = self
                    .navigator
                    .new_strip(&self.navigator_frame, ui::time::now())
                {
                    self.apply_navigation(navigation);
                }
            }
            WorkspaceMenuChoice::Rename => {
                self.bar.begin_workspace_rename();
            }
        }
    }

    /// Who the profile card is about, and what the page offers about them.
    fn profile_model(&self) -> ProfileModel {
        let offered = self.handle.host_actions();
        // Keeping (a passkey) and signing in as someone kept elsewhere are
        // offered only to a person this tab has not kept yet. Until then
        // they live in this tab alone, and pairing would hand another
        // device a guest that clearing this tab's storage ends: the card
        // offers keeping them first, pairing after.
        let unkept = offered
            .iter()
            .any(|action| matches!(action.id.as_str(), "keep-identity" | "sign-in-passkey"));
        let actions = offered
            .into_iter()
            .filter(|action| action.id != RENAME_ACTION)
            .filter(|action| !(unkept && action.source == Some(HostActionSource::Devices)))
            .filter_map(|action| {
                let group = match action.source? {
                    HostActionSource::Identity => ProfileGroup::Identity,
                    HostActionSource::Devices => ProfileGroup::Devices,
                    HostActionSource::Workspaces | HostActionSource::Workspace => return None,
                };
                let icon = match action.id.as_str() {
                    "save-recovery-key" => ui::icons::Icon::Download,
                    "sign-in-passkey" => ui::icons::Icon::User,
                    _ if group == ProfileGroup::Devices => ui::icons::Icon::MonitorSmartphone,
                    _ => ui::icons::Icon::Fingerprint,
                };
                Some(ProfileAction {
                    id: action.id,
                    title: action.title,
                    group,
                    icon,
                })
            })
            .collect::<Vec<_>>();
        // Reporting a problem, where the page holds a destination for it.
        let mut actions = actions;
        if self.chrome.shared.feedback.borrow().is_some() {
            actions.push(ProfileAction {
                id: REPORT_PROBLEM.into(),
                title: tilo_desktop::i18n::t("feedback-report-problem"),
                group: ProfileGroup::Together,
                icon: ui::icons::Icon::Flag,
            });
        }
        let rgb = Some(self.chrome.own_chip().rgb);
        // The name in the field is the person's own, without the number
        // others are shown beside it.
        let name = self
            .chrome
            .here
            .borrow()
            .own_label()
            .map_or_else(tilo_desktop::here::guest_label, str::to_owned);
        let watching = self.chrome.shared.watching.get();
        ProfileModel {
            identity: ui::collaboration::local_user_id().0,
            name,
            colour: rgb.map(|(r, g, b)| ui::vello::peniko::Color::from_rgb8(r, g, b)),
            can_rename: self.handle.offers_host_action(RENAME_ACTION),
            ask_name: false,
            actions,
            linked: self.chrome.shared.linked.accounts(),
            // What the page shares of what the person is doing is theirs to
            // choose here, beside their name; a spectator shares nothing,
            // since nobody here sees them.
            sharing: (!watching).then(|| self.chrome.here.borrow().sharing()),
            seen: if watching {
                tilo_desktop::profile_card::Seen::Watching
            } else {
                tilo_desktop::profile_card::Seen::Here
            },
            // Status, colour and what people see are the desktop's to keep;
            // a page shows the person as their host names them.
            ..ProfileModel::default()
        }
    }

    fn apply_profile_choice(&mut self, choice: ProfileChoice) {
        let request = match choice {
            ProfileChoice::Rename(name) => HostRequest::Named(name),
            ProfileChoice::Action(id) if id == REPORT_PROBLEM => {
                self.report_problem();
                return;
            }
            ProfileChoice::Action(id) => HostRequest::Action(id),
            ProfileChoice::Link(app) => return self.link_account(&app, true),
            ProfileChoice::Unlink(app) => return self.link_account(&app, false),
            ProfileChoice::Sharing(mode) => {
                self.chrome.here.borrow_mut().set_sharing(mode);
                return;
            }
            ProfileChoice::Availability(_)
            | ProfileChoice::Status(_)
            | ProfileChoice::Colour(_)
            | ProfileChoice::Visibility(_) => return,
        };
        self.requests.borrow_mut().push(request);
    }

    /// Ask the room's host to link (or unlink) the person's account with
    /// `app`'s service; the open card shows what came of it once the host
    /// answered ([`Self::follow_room_actions`]).
    fn link_account(&self, app: &str, link: bool) {
        let linked = &self.chrome.shared.linked;
        if link {
            linked.link(app);
        } else {
            linked.unlink(app);
        }
    }

    /// Hand the room actions the chrome asked for to the page, which has the
    /// room's host run them, and show the linked accounts again when an
    /// answer changed them. Whether anything changed.
    fn follow_room_actions(&mut self) -> bool {
        // Before the requests are taken, so the asks it makes go out now.
        self.linking.follow(
            &self.chrome.shared.linked,
            self.profile.is_open(),
            ui::time::now(),
        );
        let asked = self.chrome.shared.room_actions.take_requests();
        let changed = !asked.is_empty();
        self.requests
            .borrow_mut()
            .extend(asked.into_iter().map(HostRequest::RoomAction));
        let linked = &self.chrome.shared.linked;
        let revision = linked.revision();
        if self.linked_seen.replace(revision) != revision {
            self.profile.set_linked(linked.accounts());
            return true;
        }
        changed
    }

    /// Run a workspace or strip `command` on the active workspace as the
    /// model's command, once the desktop's policy allows it; a refusal is
    /// said at once. The strip it made, if any.
    fn command(&self, command: &DesktopCommand) -> Option<StripId> {
        let applied = self
            .chrome
            .may(command)
            .and_then(|()| self.edit_workspace(command));
        match applied {
            Ok(made) => made,
            Err(refusal) => {
                self.handle.acknowledge(refusal);
                None
            }
        }
    }

    /// Add a strip named `name` to the active workspace, kept as this
    /// person's, and go there.
    /// Whether it was made.
    fn add_strip(&self, name: String) -> bool {
        let Some(strip) = self.command(&DesktopCommand::CreateStrip { name }) else {
            return false;
        };
        self.chrome
            .room
            .authors
            .borrow_mut()
            .insert(strip.0, self.chrome.room.author.clone());
        true
    }

    fn rename_workspace(&self, name: String) {
        let unchanged = self.handle.read(|tree| {
            tree.active_workspace()
                .is_some_and(|workspace| workspace.name == name.trim())
        });
        if !unchanged {
            self.command(&DesktopCommand::RenameWorkspace { name });
        }
    }

    /// Apply a strip or workspace edit through the desktop's one command
    /// path, the native shell's, and replicate the result; the strip it
    /// made, if any.
    fn edit_workspace(&self, command: &DesktopCommand) -> Result<Option<StripId>, String> {
        let mut tree = self.handle.read(Clone::clone);
        let made =
            apply_to_active_workspace(&mut tree, command, &mut || Ok(self.handle.mint_id()))?;
        self.handle.replace_tree(tree);
        Ok(made)
    }

    fn toggle_overview(&mut self) {
        let open = !self.overview.is_open();
        let cards = if open { self.cards() } else { Vec::new() };
        self.overview_revision = self.handle.revision();
        self.overview
            .set_new_workspace(self.chrome.may_create_workspace().is_ok());
        self.overview.set_open(open, cards);
    }

    fn cards(&self) -> Vec<tilo_desktop::overview::RoomCard> {
        let host = PageOverview {
            user: ui::collaboration::local_user_id().0,
        };
        let now = ui::time::unix_now();
        self.handle.read(|tree| room_cards(tree, &host, now))
    }

    fn apply_pick(&mut self, pick: &OverviewPick) {
        self.overview.set_open(false, Vec::new());
        match pick {
            OverviewPick::Window(tab) => {
                self.handle.focus_tab(*tab);
            }
            OverviewPick::Workspace(id) => self.dispatch(&Command::Workspace {
                generation: self.generation.clone(),
                workspace: id.0.to_string(),
            }),
            OverviewPick::NewWorkspace => self.new_workspace(),
            OverviewPick::Dismiss => {}
        }
    }

    /// Add an empty workspace after the others and go there; its launchpad
    /// offers what to open.
    fn new_workspace(&self) {
        if let Err(refusal) = self.chrome.may_create_workspace() {
            self.handle.acknowledge(refusal);
            return;
        }
        let mut tree = self.handle.read(Clone::clone);
        let name = tilo_desktop_surface::i18n::t_args(
            "browser-new-workspace-name",
            &[("number", tree.workspaces.len().saturating_add(1).into())],
        );
        let mut workspace = Workspace::new(WorkspaceId(self.handle.mint_id()), name);
        let strip = Strip::new(StripId(self.handle.mint_id()), "Main");
        let strip_id = strip.id;
        workspace.strips.push(strip);
        let id = workspace.id;
        tree.workspaces.push(workspace);
        tree.active_strip.insert(id, strip_id);
        tree.active = id;
        self.handle.replace_tree(tree);
    }

    /// The window `step` places from the focused one in the shown strip, or
    /// the strip's `nth` window.
    fn window_tab(&self, step: Option<isize>, nth: Option<usize>) -> Option<u64> {
        self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip_id = tree.active_strip.get(&workspace.id).copied()?;
            let strip = workspace.strips.iter().find(|s| s.id == strip_id)?;
            let index = match (step, nth) {
                (_, Some(nth)) => nth,
                (Some(step), None) => {
                    let focused = tree
                        .active_group_for(strip.id)
                        .and_then(|g| strip.window_groups.iter().position(|w| w.id == g))
                        .unwrap_or(0);
                    focused.checked_add_signed(step)?
                }
                (None, None) => return None,
            };
            strip.window_groups.get(index).map(|group| group.active_tab)
        })
    }

    /// Go to the strip `pick` chooses from the shown one's index, in the
    /// active workspace.
    fn go_to_strip(&self, pick: impl FnOnce(usize) -> Option<usize>) {
        let next = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let current = tree
                .active_strip
                .get(&workspace.id)
                .and_then(|id| workspace.strips.iter().position(|s| s.id == *id))
                .unwrap_or(0);
            let strip = workspace.strips.get(pick(current)?)?;
            Some((workspace.id.0.to_string(), strip.id.0.to_string()))
        });
        if let Some((workspace, strip)) = next {
            self.dispatch(&Command::Strip {
                generation: self.generation.clone(),
                workspace,
                strip,
            });
        }
    }

    /// Maximize the focused window (`Some(true)`), restore it
    /// (`Some(false)`), or toggle it (`None`).
    fn maximize_focused(&self, want: Option<bool>) {
        let maximized = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip = *tree.active_strip.get(&workspace.id)?;
            let group = tree.active_group_for(strip)?;
            workspace
                .strips
                .iter()
                .flat_map(|s| &s.window_groups)
                .find(|g| g.id == group)
                .map(|g| g.maximized)
        });
        let Some(maximized) = maximized else {
            return;
        };
        let target = want.unwrap_or(!maximized);
        if target == maximized {
            return;
        }
        let Some(route) = self.handle.snapshot(&self.generation, Vec::new()).active else {
            return;
        };
        self.dispatch(&Command::Maximize {
            generation: self.generation.clone(),
            route,
            maximized: target,
        });
    }

    /// A desktop chord (see the module docs), applied; false when `event`
    /// is not one. The chord is looked up in the desktop's one shortcut
    /// table, as the native shell looks it up, with the page's stand-in
    /// for Super counting as Super ([`page_shortcut_action`]).
    fn chord(&mut self, event: &ui::winit::event::KeyEvent, modifiers: &Modifiers) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        let action = page_shortcut_action(
            &self.shortcuts,
            &event.logical_key,
            event.physical_key,
            *modifiers,
            self.offers.mac,
        );
        if let Some(action) = action {
            return self.shortcut(action);
        }
        // Super+S opens the strip switcher for the keyboard, as in the
        // native shell, where it is fixed rather than rebindable.
        let strips = page_chord(
            &event.logical_key,
            event.physical_key,
            *modifiers,
            self.offers.mac,
        )
        .is_some_and(|(key, held)| {
            held.state() == ui::winit::keyboard::ModifiersState::SUPER
                && key == ui::winit::keyboard::Key::Character("s".into())
        });
        if strips {
            self.navigator
                .toggle_from_keyboard(&self.navigator_frame, ui::time::now());
        }
        strips
    }

    /// Read where the person is into the navigator, when the tree, the
    /// strip's pan, who is here, the overview or the content area changed
    /// since it last read.
    fn observe_navigation(&mut self) {
        let content = self.area.get_rect();
        let covered = self.overview.is_open();
        let seen = (
            self.handle.navigation_revision(),
            self.chrome.here.borrow().revision(),
            covered,
            content,
        );
        if self.navigator_seen.replace(seen) == Some(seen) {
            return;
        }
        let tree = self.chrome.tree();
        let author = self.chrome.room.author.clone();
        // Who names a strip is the desktop's one policy: whoever governs the
        // workspace, and anyone the strips they made.
        let may_rename = |strip: StripId| {
            self.chrome
                .may(&DesktopCommand::RenameStrip {
                    strip: strip.0,
                    name: String::new(),
                })
                .is_ok()
        };
        let shown = self.handle.shown_tabs();
        let scroll = self.handle.presentation().pan.map_or(0.0, |pan| pan.offset);
        self.navigator.set_model(StripNavigator::model_of(
            &tree,
            &Viewer {
                local_user: &author,
                own_id: &author,
                may_rename: &may_rename,
                // The new strip label is offered as the workspace menu's
                // is: to whom the policy lets add one.
                may_add: self.chrome.offered_edits().add_desk,
            },
        ));
        self.navigator_frame = StripFrame::of_tree(&tree, content, (&shown, scroll), covered);
        self.navigator
            .observe(&self.navigator_frame, ui::time::now());
    }

    /// Carry out what the strip ribbon, its labels or the rail asked for,
    /// through the same command path as the native shell.
    fn apply_navigation(&mut self, navigation: StripNavigation) {
        let refused = match navigation {
            StripNavigation::Window { strip, group } => {
                let tab = self.handle.read(|tree| {
                    tree.active_workspace()?
                        .strips
                        .iter()
                        .find(|s| s.id == strip)?
                        .window_groups
                        .iter()
                        .find(|g| g.id == group)
                        .map(|g| g.active_tab)
                });
                if let Some(tab) = tab {
                    self.handle.focus_tab(tab);
                }
                false
            }
            StripNavigation::Strip(StripCommand::Focus(strip)) => {
                let index = self.handle.read(|tree| {
                    tree.active_workspace()?
                        .strips
                        .iter()
                        .position(|s| s.id == strip)
                });
                self.go_to_strip(|_| index);
                false
            }
            StripNavigation::Strip(StripCommand::Step(steps)) => {
                let steps = isize::try_from(steps).unwrap_or_default();
                self.go_to_strip(|current| current.checked_add_signed(steps));
                false
            }
            StripNavigation::Strip(StripCommand::Create(name)) => !self.add_strip(name),
            StripNavigation::Strip(StripCommand::Rename(strip, name)) => self
                .edit_workspace(&DesktopCommand::RenameStrip {
                    strip: strip.0,
                    name,
                })
                .is_err(),
            StripNavigation::Strip(StripCommand::Remove(strip)) => self
                .edit_workspace(&DesktopCommand::RemoveStrip { strip: strip.0 })
                .is_err(),
        };
        if refused {
            self.handle
                .acknowledge(tilo_desktop::i18n::t("strip-switcher-refused"));
        }
        // A new strip is listed at once, so its name goes in place.
        self.navigator_seen = None;
        self.observe_navigation();
    }

    /// Whether the strip ribbon, its labels or the rail took `event`.
    fn navigation_event(&mut self, event: &WindowEvent, scale: f64, modifiers: &Modifiers) -> bool {
        use ui::winit::event::{Ime, MouseButton};
        let now = ui::time::now();
        let navigation = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale, position.y / scale);
                self.navigator.pointer_moved(&self.navigator_frame, p, now);
                return false;
            }
            WindowEvent::CursorLeft { .. } => {
                self.navigator.pointer_left(now);
                return false;
            }
            WindowEvent::KeyboardInput { event: key, .. } if self.navigator.takes_keys() => {
                if key.state != ElementState::Pressed {
                    return true;
                }
                self.navigator
                    .key(&self.navigator_frame, &key.logical_key, modifiers.state())
            }
            WindowEvent::Ime(Ime::Commit(text)) if self.navigator.takes_keys() => {
                return self.navigator.commit_text(text);
            }
            WindowEvent::MouseInput {
                state,
                button: button @ (MouseButton::Left | MouseButton::Right),
                ..
            } => {
                if *state == ElementState::Released {
                    return self.navigator.release();
                }
                let Some(p) = self.cursor else {
                    return false;
                };
                // A secondary press (a long-press by touch) puts a strip's
                // actions out beside its label.
                let answer = if *button == MouseButton::Right {
                    self.navigator
                        .secondary_press(&self.navigator_frame, p, now)
                } else {
                    self.navigator.press(&self.navigator_frame, p, now)
                };
                let Some(navigation) = answer else {
                    return false;
                };
                navigation
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(p) = self.cursor else {
                    return false;
                };
                let Some(navigation) = self.navigator.wheel(&self.navigator_frame, p, *delta, now)
                else {
                    return false;
                };
                navigation
            }
            _ => return false,
        };
        if let Some(navigation) = navigation {
            self.apply_navigation(navigation);
        }
        true
    }

    /// Perform a desktop shortcut the page has: false for one it does not
    /// (locking the device, the magnifier, dictation, …), which then
    /// reaches the window as an ordinary key.
    fn shortcut(&mut self, action: ShortcutAction) -> bool {
        match action {
            ShortcutAction::WorkspaceOverview => self.toggle_overview(),
            ShortcutAction::FocusPreviousWindow | ShortcutAction::FocusNextWindow => {
                let step = if action == ShortcutAction::FocusPreviousWindow {
                    -1
                } else {
                    1
                };
                if let Some(tab) = self.window_tab(Some(step), None) {
                    self.handle.focus_tab(tab);
                }
            }
            ShortcutAction::PreviousStrip => self.go_to_strip(|current| current.checked_sub(1)),
            ShortcutAction::NextStrip => self.go_to_strip(|current| current.checked_add(1)),
            ShortcutAction::PreviousWorkspace => {
                self.apply_intent(ChromeIntent::SwitchWorkspace(-1));
            }
            ShortcutAction::NextWorkspace => self.apply_intent(ChromeIntent::SwitchWorkspace(1)),
            ShortcutAction::MaximizeWindow => self.maximize_focused(Some(true)),
            ShortcutAction::RestoreWindow => self.maximize_focused(Some(false)),
            ShortcutAction::ToggleMaximize => self.maximize_focused(None),
            ShortcutAction::ReportProblem => self.report_problem(),
            other => {
                if let Some(index) = other.focus_strip_index() {
                    self.go_to_strip(|_| Some(index));
                } else if let Some(index) = other.focus_group_index() {
                    if let Some(tab) = self.window_tab(None, Some(index)) {
                        self.handle.focus_tab(tab);
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn apply_launchpad(&self, action: LaunchpadAction) {
        match action {
            LaunchpadAction::Invite => self.invite(),
            LaunchpadAction::Activity => self.activity(),
            other => {
                if let Some((app, _)) = other.app_target() {
                    self.launch(app);
                }
            }
        }
    }
}

/// What the overview needs beyond the tree: the page knows no app artwork
/// and no workspace settings, so shares are named by their scope, and each
/// window wears its app's mark as on every host.
struct PageOverview {
    user: String,
}

impl WindowSource for PageOverview {}

impl OverviewHost for PageOverview {
    fn local_user(&self) -> &str {
        &self.user
    }

    fn scope_label(&self, scope: &collab_types::caps::Scope) -> String {
        tilo_desktop::workspace_settings::scope_label(
            scope,
            &tilo_desktop::workspace_settings::NoAppDirectory,
        )
    }

    fn node_label(&self, node: &tilo_action_policy::TreePath) -> String {
        tilo_desktop::workspace_settings::node_phrase(
            node,
            &tilo_desktop::workspace_settings::NoAppDirectory,
        )
    }
}

/// Whether the desk shown has no window. Every desk, not only a workspace's
/// first: a new desk beside others with windows is as blank as a new
/// workspace, and the launchpad is the way in from either.
fn desk_is_empty(tree: &DesktopTree) -> bool {
    tree.active_workspace().is_some_and(|workspace| {
        tilo_desktop_surface::navigation::active_strip(tree, workspace)
            .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id))
            .is_none_or(|strip| strip.window_groups.is_empty())
    })
}

/// The launchpad's words in the page: what Tilo is and that work is kept
/// safe, and the page's own search chord.
fn welcome_copy(mac: bool) -> LaunchpadCopy {
    let t = tilo_desktop_surface::i18n::t;
    LaunchpadCopy {
        title: Some(t("browser-welcome-title")),
        subtitle: Some(t("browser-welcome-subtitle")),
        keyboard_hint: Some(t(if mac {
            "browser-welcome-hint-mac"
        } else {
            "browser-welcome-hint"
        })),
    }
}

impl Widget for BrowserShell {
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let intents = std::mem::take(&mut *self.chrome.intents.borrow_mut());
        let mut changed = !intents.is_empty();
        for intent in intents {
            self.apply_intent(intent);
        }
        if let Some(pick) = self.overview.take_pick() {
            self.apply_pick(&pick);
            changed = true;
        }
        if self.overview.is_open() && self.overview_revision != self.handle.revision() {
            self.overview_revision = self.handle.revision();
            let cards = self.cards();
            self.overview.show(cards);
            changed = true;
        }
        let empty = self.handle.read(desk_is_empty);
        if self.empty.replace(empty) != empty {
            changed = true;
        }
        changed |= self.tilo_menu.settle();
        if let Some(choice) = self.workspace_menu.take_choice() {
            self.apply_workspace_choice(choice);
            changed = true;
        }
        if let Some(choice) = self.profile.take_choice() {
            self.apply_profile_choice(choice);
            changed = true;
        }
        changed |= self.follow_room_actions();
        if self.tilo_menu.take_feedback() {
            self.open_feedback();
            changed = true;
        }
        if self.chrome.shared.adopt_feedback() {
            // The page holds a destination now: the Tilo menu offers it too.
            self.tilo_menu.offer_feedback();
            changed = true;
        }
        changed |= self.follow_picker();
        let menu_open = self.tilo_menu.is_menu_open();
        if self.menu_shown.replace(menu_open) != menu_open {
            self.bar.set_tilo_menu_open(menu_open);
            changed = true;
        }
        changed |= self.bar.update(state, path);
        changed |= self.area.update(state, path);
        self.observe_navigation();
        // Chosen while the surface updated (search, a banner): the page
        // performs them when it reads this frame's status.
        let choices = self.handle.take_host_choices();
        changed |= !choices.is_empty();
        self.requests
            .borrow_mut()
            .extend(choices.into_iter().map(HostRequest::Action));
        changed
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ui::automerge::ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        if tilo_desktop::launchpad::take_open_app(edits, path) {
            self.handle.open_applications();
        }
        if let Some(action) = LaunchpadAction::take(edits, path) {
            self.apply_launchpad(action);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::taffy::{Dimension, Display, FlexDirection, Position, Size};
        let backdrop = self.backdrop.build_layout(taffy, theme, map);
        let mut style = taffy.style(backdrop).cloned().unwrap_or_default();
        style.position = Position::Absolute;
        let _ = taffy.set_style(backdrop, style);
        let bar = self.bar.build_layout(taffy, theme, map);
        let area = self.area.build_layout(taffy, theme, map);
        let mut style = taffy.style(area).cloned().unwrap_or_default();
        style.flex_grow = 1.0;
        style.flex_basis = Dimension::length(0.0);
        style.size.height = Dimension::auto();
        style.min_size.height = Dimension::length(0.0);
        let _ = taffy.set_style(area, style);
        // Over the area under the bar, which stays usable while it is open.
        let overview = self.overview.build_layout(taffy, theme, map);
        let mut style = taffy.style(overview).cloned().unwrap_or_default();
        style.position = Position::Absolute;
        style.inset = ui::taffy::Rect {
            left: ui::taffy::LengthPercentageAuto::length(0.0),
            right: ui::taffy::LengthPercentageAuto::length(0.0),
            top: ui::taffy::LengthPercentageAuto::length(tilo_desktop::topbar::TOPBAR_HEIGHT_PX),
            bottom: ui::taffy::LengthPercentageAuto::length(0.0),
        };
        style.size = Size::auto();
        let _ = taffy.set_style(overview, style);
        let tilo_menu = self.tilo_menu.build_layout(taffy, theme, map);
        let workspace_menu = self.workspace_menu.build_layout(taffy, theme, map);
        let profile = self.profile.build_layout(taffy, theme, map);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(
            style,
            &[
                backdrop,
                bar,
                area,
                overview,
                tilo_menu,
                workspace_menu,
                profile,
            ],
            map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let nodes = taffy.children(node).unwrap_or_default();
        if let [
            backdrop,
            bar,
            area,
            overview,
            tilo_menu,
            workspace_menu,
            profile,
        ] = nodes[..]
        {
            self.backdrop.apply_layout(taffy, backdrop, inner);
            self.bar.apply_layout(taffy, bar, inner);
            self.area.apply_layout(taffy, area, inner);
            self.overview.apply_layout(taffy, overview, inner);
            self.tilo_menu.apply_layout(taffy, tilo_menu, inner);
            self.workspace_menu
                .apply_layout(taffy, workspace_menu, inner);
            self.profile.apply_layout(taffy, profile, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let started = ui::time::now();
        self.drawn_minute.set(minute());
        self.backdrop.draw(scene, transform, text, theme, presence);
        ui::widget::draw_child(&mut self.area, scene, transform, text, theme, presence);
        self.navigator.draw(
            &self.navigator_frame,
            scene,
            transform,
            text,
            theme,
            ui::time::now(),
        );
        self.overview.draw(scene, transform, text, theme, presence);
        // Last, so what the bar opens below its band stays above windows.
        self.bar.draw(scene, transform, text, theme, presence);
        self.draw_pinned_people(scene, transform, text, theme);
        if self.people_open {
            self.draw_people(scene, transform, text, theme);
        } else {
            self.people.clear();
        }
        // Over everything, while picking what to report.
        self.picker.draw(scene, transform, text, theme, self.rect);
        let built = ui::time::now().saturating_duration_since(started);
        self.picker
            .frame_built(u32::try_from(built.as_micros()).unwrap_or(u32::MAX));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [_, bar, area, overview, tilo_menu, workspace_menu, profile] = nodes[..] else {
            return;
        };
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        } else if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.cursor = None;
        }
        // Picking what to report takes every event: nothing underneath
        // reacts to a pick.
        if self.picker.is_open() {
            let windows = self.pick_windows();
            let modifiers = ctx.modifiers.state();
            *ctx.handled =
                self.picker
                    .handle_event(event, self.cursor, modifiers, &self.area, &windows);
            return;
        }
        // The open popover takes the keyboard and the pointer over it; a
        // press elsewhere closes it, and the bar's avatars toggle it.
        if self.people_open || self.people.wants_pointer_capture() {
            let on_bar = self
                .cursor
                .is_some_and(|point| point.y < f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX));
            let hit = self
                .people
                .handle_event(event, ctx, self.people_open, self.cursor);
            if let Some(hit) = hit {
                if !(hit == CollaboratorsHit::Dismiss && on_bar) {
                    self.apply_people_hit(hit);
                }
                *ctx.handled = !(hit == CollaboratorsHit::Dismiss && on_bar);
            }
            if let WindowEvent::KeyboardInput { event: key, .. } = event
                && self.people_open
                && !*ctx.handled
                && key.state == ElementState::Pressed
                && let Some(hit) = self
                    .people
                    .key(&key.logical_key, ctx.modifiers.state().shift_key())
            {
                self.apply_people_hit(hit);
                *ctx.handled = true;
            }
            if *ctx.handled {
                return;
            }
        }
        if self.pinned_people_event(event, ctx) {
            return;
        }
        // The workspace menu and the profile card, like the Tilo menu, take
        // what they cover, the keyboard and the press that dismisses them.
        if self.workspace_menu.is_open() {
            self.workspace_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, workspace_menu));
            if *ctx.handled {
                return;
            }
        }
        if self.profile.is_open() {
            self.profile
                .handle_event(event, &mut ctx.reborrow(ctx.path, profile));
            if *ctx.handled {
                return;
            }
        }
        // The Tilo menu and its sheet take what they cover, the keyboard
        // and the press that dismisses them.
        if self.tilo_menu.is_open() {
            self.tilo_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, tilo_menu));
            if *ctx.handled {
                return;
            }
        }
        // The strip switcher's name field and keyboard choosing come before
        // the desktop's chords, as any field's typing does.
        if self.navigation_event(event, ctx.scale_factor, ctx.modifiers) {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.chord(key, ctx.modifiers)
        {
            *ctx.handled = true;
            return;
        }
        // The open overview takes the keyboard and what it covers; the bar
        // above it stays usable.
        if self.overview.is_open() {
            self.overview
                .handle_event(event, &mut ctx.reborrow(ctx.path, overview));
        }
        if !*ctx.handled {
            self.bar
                .handle_event(event, &mut ctx.reborrow(ctx.path, bar));
        }
        if !*ctx.handled && !self.overview.is_open() {
            self.area
                .handle_event(event, &mut ctx.reborrow(ctx.path, area));
        }
    }

    /// The desktop surface's strip pan is view-only, while nothing above
    /// the strip takes the wheel first: the People popover, the Tilo menu,
    /// the overview, or the bar under the pointer (its centre cycles
    /// widgets). The surface is the area's top layer, so a wheel it takes
    /// never reaches the empty desk's launchpad beneath.
    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        !self.people_open
            && !self.people.wants_pointer_capture()
            && !self.pinned.wants_pointer_capture()
            && self.cursor.is_none_or(|point| !self.pinned.contains(point))
            && !self.tilo_menu.is_open()
            && !self.workspace_menu.is_open()
            && !self.profile.is_open()
            && !self.overview.is_open()
            && self
                .cursor
                .is_none_or(|point| point.y >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX))
            // A wheel over the strip ribbon or its labels steps strips.
            && self
                .cursor
                .is_none_or(|point| !self.navigator.takes_wheel_at(&self.navigator_frame, point))
            && self
                .area
                .children()
                .last()
                .is_some_and(|surface| surface.view_only_scroll(event))
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![
            &self.backdrop,
            &self.bar,
            &self.area,
            &self.overview,
            &self.tilo_menu,
            &self.workspace_menu,
            &self.profile,
        ]
    }

    fn visit_retained_components(
        &mut self,
        request: &mut ui::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), ui::bound_dispatch::BoundDispatchError> {
        for child in self.children_mut() {
            request.visit(child)?;
        }
        Ok(())
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![
            &mut self.backdrop,
            &mut self.bar,
            &mut self.area,
            &mut self.overview,
            &mut self.tilo_menu,
            &mut self.workspace_menu,
            &mut self.profile,
        ]
    }

    fn needs_redraw(&self) -> bool {
        self.drawn_minute.get() != minute()
            || self.picker.needs_animation_frame()
            || (self.people_open && self.people.needs_animation_frame())
            || self.pinned.needs_animation_frame()
            || self.navigator.needs_animation_frame(ui::time::now())
            || [
                &self.backdrop as &dyn Widget,
                &self.bar,
                &self.area,
                &self.overview,
                &self.tilo_menu,
                &self.workspace_menu,
                &self.profile,
            ]
            .iter()
            .any(|child| child.needs_redraw())
    }

    /// The earliest wake any part of the page asked for. The bar's own
    /// deadline turns its clock over on the minute: it stays due once the
    /// minute passes, where one recomputed here from "now" always lay a
    /// minute ahead and never came due.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [
            &self.backdrop as &dyn Widget,
            &self.bar,
            &self.area,
            &self.overview,
            &self.tilo_menu,
        ]
        .iter()
        .filter_map(|child| child.next_wake_deadline())
        .chain(
            self.people_open
                .then(|| self.people.next_wake_deadline())
                .flatten(),
        )
        .chain(self.pinned.next_wake_deadline())
        .chain(
            self.navigator
                .next_wake_deadline(&self.navigator_frame, ui::time::now()),
        )
        .chain(self.linking.due())
        .min()
    }

    fn name(&self) -> &'static str {
        "BrowserShell"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::Role;
    use tilo_desktop_model::IdMinter;
    use tilo_desktop_model::commands::Standing;

    const GUEST: &str = "guest";
    const OTHER_GUEST: &str = "other-guest";

    /// The public Lounge as a page holds it: one strip the room lists, a
    /// window the guest opened and one another guest opened.
    fn lounge() -> (DesktopTree, BTreeMap<u64, String>) {
        let mut minter = IdMinter::new(3);
        let tabs = ["so.tilo.notes", "so.tilo.chat"]
            .into_iter()
            .map(|app| tilo_desktop_model::Tab {
                id: minter.mint(),
                title: app.into(),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: app.into(),
                    label: app.into(),
                    note: String::new(),
                },
                app_title: None,
            })
            .collect::<Vec<_>>();
        let authors = [(tabs[0].id, GUEST), (tabs[1].id, OTHER_GUEST)]
            .into_iter()
            .map(|(id, who)| (id, who.to_owned()))
            .collect();
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Lounge", tabs);
        (tree, authors)
    }

    fn standing(person: &str, role: Option<Role>) -> Standing<'_> {
        Standing {
            person,
            role,
            layout_shared: true,
            members: None,
        }
    }

    #[test]
    fn the_workspace_menu_offers_rename_and_new_desk_only_to_who_governs() {
        let (tree, authors) = lounge();
        let offers = |person, role| offered_edits(&standing(person, role), &authors, &tree);
        let none = OfferedEdits {
            rename: false,
            add_desk: false,
        };
        let all = OfferedEdits {
            rename: true,
            add_desk: true,
        };
        // A guest and a member of the public workspace write in it, and
        // leave its name and desks to its owner and moderators.
        assert_eq!(offers(GUEST, Some(Role::Editor)), none);
        assert_eq!(offers("member", Some(Role::Editor)), none);
        assert_eq!(offers("moderator", Some(Role::Moderator)), all);
        assert_eq!(offers("owner", Some(Role::Owner)), all);
        // Until the room has answered, nothing is offered to take back.
        assert_eq!(offers(GUEST, None), none);
        let own = crate::desktop::Standing::default();
        assert_eq!(offered_edits(&own.of(GUEST), &authors, &tree), all);
    }

    #[test]
    fn a_guest_is_offered_no_close_on_another_guests_window() {
        let (tree, authors) = lounge();
        let shared = crate::desktop::Shared::default();
        shared.authors.borrow_mut().clone_from(&authors);
        let mut person = |id: &str| (id.to_owned(), (1, 2, 3));
        let theirs = authors
            .iter()
            .find(|(_, who)| *who == OTHER_GUEST)
            .map(|(id, _)| *id)
            .unwrap();
        for role in [Role::Editor, Role::Moderator] {
            shared.standing.set(Some(role));
            let held = shared.holders(&tree, GUEST, "Lounge", &mut person);
            if role == Role::Moderator {
                assert!(held.is_empty(), "a moderator closes anyone's window");
            } else {
                // Their own window offers its close; the other guest's
                // names who opened it instead.
                assert_eq!(held.keys().copied().collect::<Vec<_>>(), vec![theirs]);
                assert_eq!(held[&theirs].name, OTHER_GUEST);
            }
        }
    }

    /// A window whose opener has left the room holds nobody: once the
    /// room's members are heard without them, it offers its close.
    #[test]
    fn a_window_of_someone_who_left_offers_its_close() {
        let (tree, authors) = lounge();
        let shared = crate::desktop::Shared::default();
        shared.authors.borrow_mut().clone_from(&authors);
        shared.standing.set(Some(Role::Editor));
        let mut person = |id: &str| (id.to_owned(), (1, 2, 3));
        shared
            .members
            .replace(Some([GUEST, OTHER_GUEST].map(str::to_owned).into()));
        assert!(
            !shared
                .holders(&tree, GUEST, "Lounge", &mut person)
                .is_empty(),
            "the other guest is still here"
        );
        shared.members.replace(Some([GUEST.to_owned()].into()));
        let held = shared.holders(&tree, GUEST, "Lounge", &mut person);
        assert!(
            held.values().all(|holder| holder.name != OTHER_GUEST),
            "the other guest left: {held:?}"
        );
    }

    /// A public room shares no member's layout: once it said so, every
    /// window on the visitor's desktop is theirs to close (nothing is ever
    /// undone), and the workspace's name and desks are offered to nobody.
    #[test]
    fn in_a_public_room_the_desktop_is_the_visitors_own() {
        let (tree, authors) = lounge();
        let shared = crate::desktop::Shared::default();
        shared.authors.borrow_mut().clone_from(&authors);
        shared.own_layout.set(true);
        let mut person = |id: &str| (id.to_owned(), (1, 2, 3));
        shared.standing.set(Some(Role::Editor));
        assert!(
            shared
                .holders(&tree, GUEST, "Lounge", &mut person)
                .is_empty()
        );
        for role in [Role::Editor, Role::Moderator] {
            shared.standing.set(Some(role));
            let edits = offered_edits(&shared.standing_of(GUEST, None), &authors, &tree);
            assert_eq!(edits, OfferedEdits::default(), "{role:?}");
        }
    }
}
