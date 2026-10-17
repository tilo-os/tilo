//! The desktop's chrome in the page, from `tilo-desktop`'s widgets: the
//! native shell's backdrop, its top bar over the strip, the launchpad on a
//! workspace with no windows yet, the overview panel, and the menu behind
//! the bar's Tilo mark with the About sheet saying what this is.
//!
//! Desktop chords are the native shell's Super chords where the browser
//! passes Super through, and the same keys with the page's stand-in for
//! Super, which no browser or system binds: Alt+Shift, or Ctrl+Option on a
//! Mac (Option+Shift selects by words there). Left/Right go to the
//! previous/next window, Up/Down to the previous/next strip, Page Up/Down to
//! the previous/next workspace, 1…9 to a strip's window, and O (or
//! Super+Tab) opens the overview.
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
//! windows they are in carry their faces.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop::about::{AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource, StripPlaces,
};
use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, LineSource, RosterRow, RosterStatus,
};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction, LaunchpadCopy};
use tilo_desktop::overview::{OverviewHost, room_cards};
use tilo_desktop::overview_panel::{OverviewPanel, OverviewPick};
use tilo_desktop::profile_card::{
    ProfileAction, ProfileCard, ProfileChoice, ProfileGroup, ProfileModel,
};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop::window_switcher::WindowSource;
use tilo_desktop::workspace_menu::{
    WorkspaceAction, WorkspaceChoice, WorkspaceMenu, WorkspaceMenuChoice, WorkspaceMenuModel,
};
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
use ui::winit::keyboard::{Key, NamedKey};
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
}

/// Whether `user` may rename `workspace`: whoever governs it (its owner and
/// moderators), or anyone in a workspace nobody owns yet.
fn may_rename(workspace: &Workspace, user: &str) -> bool {
    workspace.policy.owner.is_none()
        || matches!(
            workspace.role_of(user),
            Some(tilo_action_policy::Role::Owner | tilo_action_policy::Role::Moderator)
        )
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
    intents: RefCell<Vec<ChromeIntent>>,
    /// `(tree revision, minute, who-is-here revision)` the snapshot revision
    /// was last bumped for.
    seen: Cell<(u64, i64, u64)>,
    revision: Cell<u64>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl DesktopChromeSource for PageChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = clock_snapshot();
        let seen = (
            self.handle.revision(),
            clock.unix_seconds.div_euclid(60),
            self.here.borrow().revision(),
        );
        if self.seen.get() != seen {
            self.seen.set(seen);
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        let others = self.here.borrow().strip_places();
        let places = self
            .handle
            .read(|tree| StripPlaces::in_active_workspace(tree, &others));
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            clock,
            strip_places: Some(places),
            watching: self.here.borrow().watching(),
            own: Some(self.own_chip()),
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
}

impl PageChrome {
    /// Who is here, placed in `tree`.
    fn people(&self, tree: &DesktopTree) -> Vec<tilo_desktop::here::Person> {
        here_people(&self.here, &self.shared, tree)
    }

    /// This page's person as everyone here sees them: their name (numbered
    /// when someone else here shares it) and colour.
    fn own_chip(&self) -> tilo_desktop::chrome::OwnChip {
        let here = self.here.borrow();
        let name = here
            .own_shown()
            .unwrap_or_else(tilo_desktop::here::guest_label);
        let rgb = here
            .own_rgb()
            .unwrap_or_else(|| ui::presence::user_color(&name));
        tilo_desktop::chrome::OwnChip { name, rgb }
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
        role: None,
        invited: false,
        activity: person.place.clone(),
        status: person.status.as_ref().map(status),
        recent: person.recent.as_ref().map(status),
        followable: false,
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

/// The launchpad, shown while the active workspace has no windows.
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
/// launchpad beneath it while the workspace is empty).
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
    /// The menu behind the bar's workspace name.
    workspace_menu: WorkspaceMenu,
    /// The card behind the bar's own avatar.
    profile: ProfileCard,
    /// The People popover the bar's avatars open.
    people: CollaboratorsOverlay,
    people_open: bool,
    /// The linked accounts' revision the card last showed.
    linked_seen: Cell<u64>,
    /// The person whose recent summary the popover shows.
    expanded: Option<String>,
    /// The pointer, in logical pixels, for the popover's targets.
    cursor: Option<Point>,
}

/// What the top bar's chrome reads from the page besides the desktop tree.
pub struct ChromeSources {
    pub here: tilo_desktop::here::Here,
    pub shared: crate::desktop::Shared,
    pub room: Room,
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
            wake,
        } = sources;
        let chrome = Rc::new(PageChrome {
            handle: handle.clone(),
            shared,
            here,
            room,
            intents: RefCell::default(),
            seen: Cell::new((u64::MAX, 0, 0)),
            revision: Cell::new(0),
            wake,
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
        let empty = Rc::new(Cell::new(workspace_is_empty(&tree)));
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
            linked_seen: Cell::new(0),
            expanded: None,
            cursor: None,
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
        let here = self.chrome.here.borrow();
        let own_label = here
            .own_shown()
            .unwrap_or_else(tilo_desktop::here::guest_label);
        let own_rgb = here
            .own_rgb()
            .unwrap_or_else(|| ui::presence::user_color(&own_label));
        let own_status = here.own_status();
        let watching = usize::try_from(here.watching()).unwrap_or(usize::MAX);
        let sharing = here.sharing();
        drop(here);
        self.people.draw(
            scene,
            transform,
            text,
            theme,
            &CollaboratorsModel {
                host: self.rect,
                rows: &rows,
                own_label: &own_label,
                own_color: theme.tokens.colors.presence.of_rgb(own_rgb),
                own_rename: self.handle.offers_host_action(RENAME_ACTION),
                unavailable: None,
                may_invite: self.offers.invite,
                followed_id: None,
                following_pairs: &[],
                own_status: None,
                own_activity: own_status.as_ref().map(|line| line.text.as_str()),
                own_activity_generated: own_status.as_ref().is_some_and(|line| line.automatic),
                own_status_automatic: false,
                status_sharing: sharing,
                status_editing: false,
                status_editable: false,
                status_selection: ui::text_selection::Selection::default(),
                screen_exposure: None,
                expanded_id: self.expanded.as_deref(),
                watching,
            },
            self.cursor,
        );
    }

    fn apply_people_hit(&mut self, hit: CollaboratorsHit) {
        match hit {
            CollaboratorsHit::Dismiss => self.people_open = false,
            CollaboratorsHit::Invite => {
                self.people_open = false;
                self.invite();
            }
            CollaboratorsHit::RoomActivity => {
                self.people_open = false;
                self.activity();
            }
            CollaboratorsHit::Rename => {
                self.people_open = false;
                self.requests
                    .borrow_mut()
                    .push(HostRequest::Action(RENAME_ACTION.to_owned()));
            }
            CollaboratorsHit::ChangeSharing => self.people.toggle_sharing_choices(),
            CollaboratorsHit::SetStatusSharing(mode) => {
                self.chrome.here.borrow_mut().set_sharing(mode);
            }
            CollaboratorsHit::ToggleDetails(index) => {
                let id = self.people.details_target(index).map(str::to_owned);
                self.expanded = if self.expanded == id { None } else { id };
            }
            _ => {}
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

    fn activity(&self) {
        if let Some(workspace) = self.active_workspace() {
            self.dispatch(&Command::WorkspaceActivity {
                generation: self.generation.clone(),
                workspace,
                view: ActivityView::CatchUp,
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
            ChromeIntent::ToggleWorkspaceMenu => {
                self.close_bar_popovers_except(BarPopover::Workspace);
                let model = self.workspace_menu_model();
                self.workspace_menu
                    .toggle(&model, self.bar.workspace_menu_rect(), self.rect);
            }
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
            ChromeIntent::SwitchStrip(strip) => {
                if let Some(workspace) = self.active_workspace() {
                    self.dispatch(&Command::Strip {
                        generation: self.generation.clone(),
                        workspace,
                        strip: strip.0.to_string(),
                    });
                }
            }
            ChromeIntent::AddStrip { name } => self.add_strip(name),
            ChromeIntent::RenameWorkspace { name } => self.rename_workspace(name),
            ChromeIntent::ToggleTiloMenu => {
                // One popover from the bar at a time.
                self.close_bar_popovers_except(BarPopover::Tilo);
                self.tilo_menu.toggle(self.bar.tilo_mark_rect(), self.rect);
            }
            _ => {}
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
    }

    /// What the workspace menu offers now: this page's workspaces and the
    /// person's others the page knows, where it is hosted, the page's own
    /// workspace commands, and what this person may change.
    fn workspace_menu_model(&self) -> WorkspaceMenuModel {
        let actions = self.handle.host_actions();
        let user = ui::collaboration::local_user_id().0;
        let (mut workspaces, can_rename, has_workspace) = self.handle.read(|tree| {
            let workspaces: Vec<WorkspaceChoice> = tree
                .workspaces
                .iter()
                .map(|workspace| WorkspaceChoice {
                    key: format!("{WORKSPACE_KEY}{}", workspace.id.0),
                    name: workspace.name.clone(),
                    current: workspace.id == tree.active,
                })
                .collect();
            let can_rename = tree
                .active_workspace()
                .is_some_and(|workspace| may_rename(workspace, &user));
            (workspaces, can_rename, tree.active_workspace().is_some())
        });
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
            can_rename,
            can_add_desk: has_workspace
                && self
                    .chrome
                    .snapshot()
                    .strip_places
                    .is_some_and(|places| places.can_add),
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
            WorkspaceMenuChoice::NewDesk => {
                self.bar.begin_new_desk();
            }
            WorkspaceMenuChoice::Rename => {
                self.bar.begin_workspace_rename();
            }
        }
    }

    /// Who the profile card is about, and what the page offers about them.
    fn profile_model(&self) -> ProfileModel {
        let actions = self
            .handle
            .host_actions()
            .into_iter()
            .filter(|action| action.id != RENAME_ACTION)
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
            .collect();
        let rgb = Some(self.chrome.own_chip().rgb);
        // The name in the field is the person's own, without the number
        // others are shown beside it.
        let name = self
            .chrome
            .here
            .borrow()
            .own_label()
            .map_or_else(tilo_desktop::here::guest_label, str::to_owned);
        ProfileModel {
            identity: ui::collaboration::local_user_id().0,
            name,
            colour: rgb.map(|(r, g, b)| ui::vello::peniko::Color::from_rgb8(r, g, b)),
            can_rename: self.handle.offers_host_action(RENAME_ACTION),
            actions,
            linked: self.chrome.shared.linked.accounts(),
        }
    }

    fn apply_profile_choice(&self, choice: ProfileChoice) {
        let request = match choice {
            ProfileChoice::Rename(name) => HostRequest::Named(name),
            ProfileChoice::Action(id) => HostRequest::Action(id),
            ProfileChoice::Link(app) => return self.link_account(&app, true),
            ProfileChoice::Unlink(app) => return self.link_account(&app, false),
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

    /// Add a strip named `name` to the active workspace, kept as this
    /// person's, and go there.
    fn add_strip(&self, name: String) {
        let mut tree = self.handle.read(Clone::clone);
        let active = tree.active;
        let Some(workspace) = tree.workspaces.iter_mut().find(|w| w.id == active) else {
            return;
        };
        let strip = Strip::new(StripId(self.handle.mint_id()), name);
        let id = strip.id;
        workspace.strips.push(strip);
        tree.active_strip.insert(active, id);
        self.chrome
            .room
            .authors
            .borrow_mut()
            .insert(id.0, self.chrome.room.author.clone());
        self.handle.replace_tree(tree);
    }

    /// Give the active workspace `name`.
    fn rename_workspace(&self, name: String) {
        let mut tree = self.handle.read(Clone::clone);
        let active = tree.active;
        let Some(workspace) = tree.workspaces.iter_mut().find(|w| w.id == active) else {
            return;
        };
        if workspace.name == name {
            return;
        }
        workspace.name = name;
        self.handle.replace_tree(tree);
    }

    fn toggle_overview(&mut self) {
        let open = !self.overview.is_open();
        let cards = if open { self.cards() } else { Vec::new() };
        self.overview_revision = self.handle.revision();
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

    fn step_strip(&self, step: isize) {
        let next = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let current = tree
                .active_strip
                .get(&workspace.id)
                .and_then(|id| workspace.strips.iter().position(|s| s.id == *id))
                .unwrap_or(0);
            let strip = workspace.strips.get(current.checked_add_signed(step)?)?;
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

    /// A desktop chord (see the module docs), applied; false when `event`
    /// is not one.
    fn chord(&mut self, event: &ui::winit::event::KeyEvent, modifiers: &Modifiers) -> bool {
        use ui::winit::keyboard::KeyCode;
        if event.state != ElementState::Pressed {
            return false;
        }
        let mods = modifiers.state();
        let native = mods.super_key() && !mods.alt_key() && !mods.control_key();
        let stand_in = !mods.super_key()
            && mods.alt_key()
            && if self.offers.mac {
                mods.control_key() && !mods.shift_key()
            } else {
                mods.shift_key() && !mods.control_key()
            };
        if !native && !stand_in {
            return false;
        }
        let digit = |key: &Key| match key {
            Key::Character(c) => c.parse::<usize>().ok().filter(|n| (1..=9).contains(n)),
            _ => None,
        };
        // A Mac's Option and Shift change the character a key types; the
        // key's place does not.
        let physical = match event.physical_key {
            ui::winit::keyboard::PhysicalKey::Code(code) => Some(code),
            ui::winit::keyboard::PhysicalKey::Unidentified(_) => None,
        };
        let nth = digit(&event.logical_key).or_else(|| match physical? {
            KeyCode::Digit1 => Some(1),
            KeyCode::Digit2 => Some(2),
            KeyCode::Digit3 => Some(3),
            KeyCode::Digit4 => Some(4),
            KeyCode::Digit5 => Some(5),
            KeyCode::Digit6 => Some(6),
            KeyCode::Digit7 => Some(7),
            KeyCode::Digit8 => Some(8),
            KeyCode::Digit9 => Some(9),
            _ => None,
        });
        match (&event.logical_key, physical) {
            (Key::Named(NamedKey::Tab), _) if native => self.toggle_overview(),
            (_, Some(KeyCode::KeyO)) => self.toggle_overview(),
            (Key::Named(NamedKey::ArrowLeft), _) | (_, Some(KeyCode::ArrowLeft)) => {
                if let Some(tab) = self.window_tab(Some(-1), None) {
                    self.handle.focus_tab(tab);
                }
            }
            (Key::Named(NamedKey::ArrowRight), _) | (_, Some(KeyCode::ArrowRight)) => {
                if let Some(tab) = self.window_tab(Some(1), None) {
                    self.handle.focus_tab(tab);
                }
            }
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some(KeyCode::ArrowUp)) => {
                self.step_strip(-1);
            }
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some(KeyCode::ArrowDown)) => {
                self.step_strip(1);
            }
            (Key::Named(NamedKey::PageUp), _) | (_, Some(KeyCode::PageUp)) => {
                self.apply_intent(ChromeIntent::SwitchWorkspace(-1));
            }
            (Key::Named(NamedKey::PageDown), _) | (_, Some(KeyCode::PageDown)) => {
                self.apply_intent(ChromeIntent::SwitchWorkspace(1));
            }
            _ => {
                let Some(index) = nth.and_then(|nth| nth.checked_sub(1)) else {
                    return false;
                };
                if let Some(tab) = self.window_tab(None, Some(index)) {
                    self.handle.focus_tab(tab);
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
/// and no workspace settings, so shares are named by their scope.
struct PageOverview {
    user: String,
}

impl WindowSource for PageOverview {
    /// The page's apps are documents and tools; only the few the toolkit
    /// has a mark for get their own.
    fn app_kind(&self, tile: &tilo_desktop_model::TileContent) -> ui::widget::AppKind {
        use ui::widget::AppKind;
        match tile {
            tilo_desktop_model::TileContent::LocalApp { app_id, .. } => match app_id.as_str() {
                "so.tilo.files" => AppKind::Files,
                "so.tilo.settings" | "so.tilo.themes" => AppKind::Settings,
                "so.tilo.terminal" => AppKind::Terminal,
                _ => AppKind::Editor,
            },
            other => tilo_desktop::resolver::tile_app_kind(other),
        }
    }
}

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

fn workspace_is_empty(tree: &DesktopTree) -> bool {
    tree.active_workspace()
        .is_some_and(|w| w.strips.iter().all(|s| s.window_groups.is_empty()))
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
        let empty = self.handle.read(workspace_is_empty);
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
        let menu_open = self.tilo_menu.is_menu_open();
        if self.menu_shown.replace(menu_open) != menu_open {
            self.bar.set_tilo_menu_open(menu_open);
            changed = true;
        }
        changed |= self.bar.update(state, path);
        changed |= self.area.update(state, path);
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
            self.handle.open_search(None);
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
        self.drawn_minute.set(minute());
        self.backdrop.draw(scene, transform, text, theme, presence);
        ui::widget::draw_child(&mut self.area, scene, transform, text, theme, presence);
        self.overview.draw(scene, transform, text, theme, presence);
        // Last, so what the bar opens below its band stays above windows.
        self.bar.draw(scene, transform, text, theme, presence);
        if self.people_open {
            self.draw_people(scene, transform, text, theme);
        } else {
            self.people.clear();
        }
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
    /// never reaches the empty-workspace launchpad beneath.
    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        !self.people_open
            && !self.people.wants_pointer_capture()
            && !self.tilo_menu.is_open()
            && !self.workspace_menu.is_open()
            && !self.profile.is_open()
            && !self.overview.is_open()
            && self
                .cursor
                .is_none_or(|point| point.y >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX))
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
            || (self.people_open && self.people.needs_animation_frame())
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

    /// The next minute, for the bar's clock, unless something wakes sooner.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let seconds = ui::time::unix_now_i64().rem_euclid(60);
        let clock = ui::time::Instant::now().checked_add(ui::time::Duration::from_secs(
            60_u64.saturating_sub(seconds.unsigned_abs()),
        ));
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
        .chain(clock)
        .min()
    }

    fn name(&self) -> &'static str {
        "BrowserShell"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
