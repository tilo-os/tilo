//! The desktop's chrome in the page, from `tilo-desktop`'s widgets: the
//! native shell's backdrop, its top bar over the strip, the launchpad on a
//! workspace with no windows yet, and the overview panel.
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

use tilo_desktop::chrome::{
    ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource, TopBarWidget,
    TopBarWidgets,
};
use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, RosterRow, RosterStatus,
};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction, LaunchpadCopy};
use tilo_desktop::overview::{OverviewHost, room_cards};
use tilo_desktop::overview_panel::{OverviewPanel, OverviewPick};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop::window_switcher::WindowSource;
use tilo_desktop_model::{DesktopTree, Strip, StripId, TabId, Workspace, WorkspaceId};
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
}

/// Requests the desktop queued since the page last read its status.
pub(crate) type HostRequests = Rc<RefCell<Vec<HostRequest>>>;

/// What the embedding page offers the desktop.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HostOffers {
    /// The page takes [`HostRequest::Invite`].
    pub invite: bool,
    /// The page runs on macOS: its command key is Cmd.
    pub mac: bool,
}

/// The bar's view of the page: the surface's tree, the wall clock, and the
/// intents it raised since the last update pass.
struct PageChrome {
    handle: DesktopHandle,
    shared: crate::desktop::Shared,
    here: tilo_desktop::here::Here,
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
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            clock,
            // The page reads no system telemetry, so the monitor would only
            // ever show dashes.
            topbar_widgets: Some(TopBarWidgets {
                widgets: vec![TopBarWidget::Clock, TopBarWidget::Calendar],
            }),
            watching: self.here.borrow().watching(),
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
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default()
}

/// One person as the People popover lists them.
fn roster_row(person: &tilo_desktop::here::Person, theme: &Theme) -> RosterRow {
    let status = |line: &tilo_desktop::here::Line| RosterStatus {
        text: line.text.clone(),
        automatic: line.automatic,
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
    chrono::Utc::now().timestamp().div_euclid(60)
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
    /// The tree revision the open overview shows.
    overview_revision: u64,
    /// The minute the bar's clock last showed.
    drawn_minute: Cell<i64>,
    rect: Rect,
    chrome: Rc<PageChrome>,
    handle: DesktopHandle,
    generation: String,
    empty: Rc<Cell<bool>>,
    offers: HostOffers,
    requests: HostRequests,
    /// The People popover the bar's avatars open.
    people: CollaboratorsOverlay,
    people_open: bool,
    /// The person whose recent summary the popover shows.
    expanded: Option<String>,
    /// The pointer, in logical pixels, for the popover's targets.
    cursor: Option<Point>,
}

impl BrowserShell {
    pub fn new(
        surface: DesktopSurface,
        handle: DesktopHandle,
        generation: String,
        offers: HostOffers,
        requests: HostRequests,
        here: tilo_desktop::here::Here,
        shared: crate::desktop::Shared,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let chrome = Rc::new(PageChrome {
            handle: handle.clone(),
            shared,
            here,
            intents: RefCell::default(),
            seen: Cell::new((u64::MAX, 0, 0)),
            revision: Cell::new(0),
            wake,
        });
        let tree = handle.read(Clone::clone);
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        let empty = Rc::new(Cell::new(workspace_is_empty(&tree)));
        let launchpad = CollaborationLaunchpad::new(|_| 0.0).with_copy(welcome_copy(offers.mac));
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
            overview_revision: 0,
            drawn_minute: Cell::new(i64::MIN),
            rect: Rect::ZERO,
            chrome,
            handle,
            generation,
            empty,
            offers,
            requests,
            people: CollaboratorsOverlay::default(),
            people_open: false,
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
        let own_label = here.own_label().unwrap_or("You").to_owned();
        let own_rgb = here
            .own_rgb()
            .unwrap_or_else(|| ui::presence::user_color(&own_label));
        let own_status = here.own_status();
        let watching = usize::try_from(here.watching()).unwrap_or(usize::MAX);
        let sharing = here.sharing();
        drop(here);
        let own_initial = own_label
            .chars()
            .next()
            .map_or('\u{2022}', |c| c.to_uppercase().next().unwrap_or(c));
        self.people.draw(
            scene,
            transform,
            text,
            theme,
            &CollaboratorsModel {
                host: self.rect,
                rows: &rows,
                own_initial,
                own_color: theme.tokens.colors.presence.of_rgb(own_rgb),
                unavailable: None,
                may_invite: self.offers.invite,
                followed_id: None,
                following_pairs: &[],
                own_status: None,
                own_activity: own_status.as_ref().map(|line| line.text.as_str()),
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
            CollaboratorsHit::SetStatusSharing(mode) => {
                self.chrome.here.borrow_mut().set_sharing(mode);
            }
            CollaboratorsHit::ToggleDetails(index) => {
                let id = self.people.details_target(index).map(str::to_owned);
                self.expanded = if self.expanded == id { None } else { id };
            }
            CollaboratorsHit::PreviousPeople | CollaboratorsHit::NextPeople => {
                let tree = self.handle.read(Clone::clone);
                let count = self.chrome.people(&tree).len();
                self.people
                    .change_people_page(hit == CollaboratorsHit::NextPeople, count);
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
                self.people_open = !self.people_open;
                if !self.people_open {
                    self.people.clear();
                }
            }
            ChromeIntent::ToggleOverview => self.toggle_overview(),
            _ => {}
        }
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
        let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or_default();
        self.handle.read(|tree| room_cards(tree, &host, now))
    }

    fn apply_pick(&mut self, pick: OverviewPick) {
        self.overview.set_open(false, Vec::new());
        match pick {
            OverviewPick::Window(tab) => {
                self.handle.focus_tab(tab);
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
        use ui::winit::keyboard::KeyCode;
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
                let Some(nth) = nth else { return false };
                if let Some(tab) = self.window_tab(None, Some(nth - 1)) {
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
            self.apply_pick(pick);
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
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[backdrop, bar, area, overview], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let nodes = taffy.children(node).unwrap_or_default();
        if let [backdrop, bar, area, overview] = nodes[..] {
            self.backdrop.apply_layout(taffy, backdrop, inner);
            self.bar.apply_layout(taffy, bar, inner);
            self.area.apply_layout(taffy, area, inner);
            self.overview.apply_layout(taffy, overview, inner);
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
        let [_, bar, area, overview] = nodes[..] else {
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

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.backdrop, &self.bar, &self.area, &self.overview]
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
            ]
            .iter()
            .any(|child| child.needs_redraw())
    }

    /// The next minute, for the bar's clock, unless something wakes sooner.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let seconds = chrono::Utc::now().timestamp().rem_euclid(60);
        let clock = ui::time::Instant::now()
            + ui::time::Duration::from_secs(u64::try_from(60 - seconds).unwrap_or(60));
        [
            &self.backdrop as &dyn Widget,
            &self.bar,
            &self.area,
            &self.overview,
        ]
        .iter()
        .filter_map(|child| child.next_wake_deadline())
        .chain(
            self.people_open
                .then(|| self.people.next_wake_deadline())
                .flatten(),
        )
        .chain(std::iter::once(clock))
        .min()
    }

    fn name(&self) -> &'static str {
        "BrowserShell"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
