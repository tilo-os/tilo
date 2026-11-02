//! The desktop every host shows: the native session and the web page
//! compose the same two layers around the [`DesktopSurface`].
//!
//! [`DeskLayer`] is what lies under the windows: the backdrop, the
//! launchpad on a desk with no windows yet, and the surface's strips and
//! window bars. [`DesktopChrome`] is what lies above them: the top bar, the
//! strip ribbon and rail ([`StripNavigator`]), the overview panel, the Tilo
//! menu and the workspace menu, and the desktop's chords. A page draws the
//! chrome over the desk in one widget tree; the native session hosts the
//! desk under its client windows and the chrome above them.
//!
//! Neither layer knows its host. What a person may change is asked of
//! [`DesktopRules`]; what only the host can do (inviting someone, a host
//! action, the bar's people and profile popovers, quick settings) comes
//! back as [`DesktopAsk`]s.
//!
//! [`DesktopSurface`]: tilo_desktop_surface::DesktopSurface

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::commands::{DesktopCommand, apply_to_active_workspace};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, TabId, Workspace, WorkspaceId, WorkspaceSlot,
};
use tilo_desktop_surface::navigation::{ActivityView, ChromeEffect, DeskEffect, Destination};
use tilo_desktop_surface::{DesktopHandle, HostActionSource};
use tilo_resource::Effect;
use ui::presence::PresenceCtx;
use ui::search::Scope;
use ui::shortcut_table::{ShortcutAction, ShortcutConfig};
use ui::widget::{AppModel, EventCtx, ModelPath, Stack, TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, WindowEvent};

use crate::about::TiloMenu;
use crate::chrome::{ChromeIntent, TopBarPresence};
use crate::desk_verbs::{OpenSearch, chrome_resource, desk_resource, launchpad_resource};
use crate::desk_widgets::music::MusicAction;
use crate::display_identify::DisplayIdentify;
use crate::emoji_picker::{EmojiLink, EmojiPicker};
use crate::fullscreen_hint::FullscreenHint;
use crate::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use crate::overlays::{Overlays, Slot};
use crate::overview::{OverviewHost, workspace_cards};
use crate::overview_panel::{OverviewPanel, OverviewPick};
use crate::search_overlay::SearchOverlay;
use crate::shortcuts::{page_chord, page_shortcut_action};
use crate::strip_navigator::{StripFrame, StripNavigation, StripNavigator, Viewer};
use crate::strip_switcher::Command as StripCommand;
use crate::topbar::{BarSpace, Placement, Reveals, TOPBAR_HEIGHT_PX, TopBarChrome};
use crate::window_flight::WindowFlight;
use crate::window_switcher::WindowSource;
use crate::workspace_menu::{
    WorkspaceAction, WorkspaceChoice, WorkspaceMenu, WorkspaceMenuChoice, WorkspaceMenuModel,
};

/// The next deadline a child widget reports, for the earliest of them.
fn wake_deadline(child: &dyn Widget) -> Option<ui::time::Instant> {
    child.next_wake_deadline()
}

/// Workspace menu keys: a workspace of this desktop, by id, and one of the
/// person's others the host opens, by its host action.
const WORKSPACE_KEY: &str = "workspace:";
/// The host action that lists the workspaces this computer may join.
pub const JOIN_WORKSPACE: &str = "join-workspace";
/// The host action that leaves the workspace the person is in.
pub const LEAVE_WORKSPACE: &str = "leave-workspace";
/// The host action that makes a new workspace.
pub const NEW_WORKSPACE: &str = "new-workspace";
/// The host action that opens the workspace's settings.
pub const WORKSPACE_SETTINGS: &str = "workspace-settings";
const HOST_KEY: &str = "host:";

/// What the host answers about the person at this desktop and the workspace it
/// shows.
pub trait DesktopRules {
    /// The tree as the chrome shows it: the surface's, with who is here as
    /// the active workspace's collaborators.
    fn tree(&self) -> DesktopTree;

    /// Changes whenever who is here does, beyond the tree.
    fn people_revision(&self) -> u64 {
        0
    }

    /// The person this desktop acts for.
    fn author(&self) -> String;

    /// Whether the person may run `command` on the active workspace.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may(&self, command: &DesktopCommand) -> Result<(), String>;

    /// Whether the person may add a workspace.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_create_workspace(&self) -> Result<(), String>;

    /// The person made `strip`: it is theirs from now on.
    fn made_strip(&self, _strip: StripId) {}

    /// Where the workspace shown is hosted (`example.org`), named beside the
    /// workspace; `None` on the person's own desktop.
    fn workspace_host(&self) -> Option<String> {
        None
    }
}

/// What the desktop asks of its host.
#[derive(Clone, Debug, PartialEq)]
pub enum DesktopAsk {
    /// A request of the bar the layers do not answer themselves: the
    /// people and profile popovers, inviting, quick settings, the clock.
    Intent(ChromeIntent),
    /// The person chose a host action (`id`) in the workspace menu.
    HostAction(String),
    /// The person left the workspace `name` here; the host says so.
    LeftWorkspace(String),
    /// The person asked to report a problem.
    ReportProblem,
    /// The person chose a card of the empty desk's launchpad.
    Launchpad(LaunchpadAction),
    /// A desktop shortcut the layers have no part in (search, quick
    /// settings, locking, the magnifier, …): the host's to carry out.
    Shortcut(ShortcutAction),
    /// The person pressed one of the music widget's controls: the host
    /// steers the player it shows.
    Music(MusicAction),
}

/// Whether the desk shown has no window. Every desk, not only a workspace's
/// first: a new desk beside others with windows is as blank as a new
/// workspace, and the launchpad is the way in from either.
#[must_use]
pub fn desk_is_empty(tree: &DesktopTree) -> bool {
    tree.active_workspace().is_some_and(|workspace| {
        tilo_desktop_surface::navigation::active_strip(tree, workspace)
            .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id))
            .is_none_or(|strip| strip.window_groups.is_empty())
    })
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
        // The launchpad keeps the node it is drawn in, so an action addressed
        // to a control of it reaches the launchpad; this workspace is the
        // node around it rather than a second claimant of the same one.
        let launchpad = self.launchpad.build_layout(taffy, theme, map);
        let style = Style {
            display: taffy::Display::Flex,
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &[launchpad], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(launchpad) = taffy.children(node).unwrap_or_default().first() {
            self.launchpad.apply_layout(taffy, *launchpad, origin);
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
        self.shown.get()
            && (self.launchpad.needs_redraw() || self.launchpad.needs_animation_frame())
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

/// A node filling its parent, laid over its siblings.
fn fill(taffy: &mut TaffyTree<()>, node: NodeId) {
    use taffy::{LengthPercentageAuto, Position, Size};
    let mut style = taffy.style(node).cloned().unwrap_or_default();
    style.position = Position::Absolute;
    style.inset = taffy::Rect {
        left: LengthPercentageAuto::length(0.0),
        right: LengthPercentageAuto::length(0.0),
        top: LengthPercentageAuto::length(0.0),
        bottom: LengthPercentageAuto::length(0.0),
    };
    style.size = Size::from_percent(1.0, 1.0);
    let _ = taffy.set_style(node, style);
}

/// A column filling its parent: the bar's band, then the area under it.
fn column() -> Style {
    use taffy::{Display, FlexDirection, Size};
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        size: Size::from_percent(1.0, 1.0),
        ..Default::default()
    }
}

/// The area under the bar: what is left of the column.
fn grow(taffy: &mut TaffyTree<()>, node: NodeId) {
    use taffy::Dimension;
    let mut style = taffy.style(node).cloned().unwrap_or_default();
    style.flex_grow = 1.0;
    style.flex_basis = Dimension::length(0.0);
    style.size.height = Dimension::auto();
    style.min_size.height = Dimension::length(0.0);
    let _ = taffy.set_style(node, style);
}

/// What lies under the windows: the backdrop, and below the bar's room the
/// strips of the desktop surface over the empty desk's launchpad.
pub struct DeskLayer {
    backdrop: crate::backdrop::DesktopBackdrop,
    /// The room the bar takes above the strips: none under a fullscreen
    /// window, else the bar's height.
    band: BarSpace,
    area: Stack,
    handle: DesktopHandle,
    empty: Rc<Cell<bool>>,
    /// The workspace whose picture the backdrop shows.
    workspace: Cell<Option<u64>>,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    rect: Rect,
}

/// Close the tab `tab` on this device's desk, wherever it stands.
fn close_tab(handle: &DesktopHandle, tab: TabId) {
    let mut tree = handle.read(Clone::clone);
    if tree.close_tab(tab).is_some() {
        handle.replace_tree(tree);
    }
}

impl DeskLayer {
    /// Take when the bar shows over the window covering its display from
    /// `reveals`, which the chrome reads too: the strip above the area takes
    /// the bar's room only while it is docked in the column.
    #[must_use]
    pub fn with_reveals(mut self, reveals: Rc<RefCell<Reveals>>) -> Self {
        self.band = BarSpace::new(Rc::new(move || reveals.borrow().presence(None, false)));
        self
    }

    /// The desk over `surface` (whose host side is `handle`), with
    /// `launchpad` on a desk with no windows and `beneath` (a host's own
    /// layers) under both.
    #[must_use]
    pub fn new(
        surface: Box<dyn Widget>,
        handle: DesktopHandle,
        launchpad: CollaborationLaunchpad,
        beneath: Vec<Box<dyn Widget>>,
    ) -> Self {
        let empty = Rc::new(Cell::new(handle.read(desk_is_empty)));
        let workspace =
            handle.read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0));
        // The room the bar takes above the strips, at the same height it
        // takes itself. The bar docks in this desktop's column, so its room
        // is there until a window covers its display; a desk that follows the
        // reveal (see [`Self::with_reveals`]) replaces this with whether the
        // bar shows, hidden or docked.
        let desk = handle.clone();
        let band = BarSpace::new(Rc::new(move || {
            if desk.read(|tree| tree.fullscreen_group().is_some()) {
                TopBarPresence::Hidden
            } else {
                TopBarPresence::Docked
            }
        }));
        let mut area = Stack::new();
        for layer in beneath {
            area = area.push(layer);
        }
        // The surface stays the area's top layer.
        let area = area
            .push(Box::new(EmptyWorkspace {
                launchpad,
                shown: Rc::clone(&empty),
            }))
            .push(surface);
        Self {
            backdrop: crate::backdrop::DesktopBackdrop::new(),
            band,
            area,
            handle,
            empty,
            workspace: Cell::new(workspace),
            asks: Rc::default(),
            rect: Rect::ZERO,
        }
    }

    /// The area under the bar: the surface and what lies beneath it.
    #[must_use]
    pub const fn area(&self) -> &Stack {
        &self.area
    }

    /// Whether the surface takes `event` as a view-only scroll (panning
    /// its strip).
    #[must_use]
    pub fn surface_view_only_scroll(&self, event: &WindowEvent) -> bool {
        self.area
            .children()
            .last()
            .is_some_and(|surface| surface.view_only_scroll(event))
    }

    /// What the person asked of the host since the last call.
    pub fn take_asks(&mut self) -> Vec<DesktopAsk> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// Put the desk's asks in `asks`, shared with a host that reads them
    /// outside the widget tree the desk is drawn in.
    #[must_use]
    pub fn with_asks(mut self, asks: Rc<RefCell<Vec<DesktopAsk>>>) -> Self {
        self.asks = asks;
        self
    }

    /// Take the pictures the desk shows, looked up by the workspace in
    /// front.
    #[must_use]
    pub fn with_wallpaper(
        mut self,
        provider: impl Fn(&str) -> Option<ui::peniko::ImageData> + 'static,
    ) -> Self {
        self.backdrop.set_wallpaper_provider(provider);
        self
    }
}

impl DeskLayer {
    /// Carry out a verb of the desk's.
    fn perform(&self, verb: &DeskEffect) -> bool {
        match verb {
            DeskEffect::Focus { tab } => {
                return tab.parse().is_ok_and(|tab| self.handle.focus_tab(tab));
            }
            DeskEffect::CloseTab { tab } => {
                let Ok(tab) = tab.parse::<u64>() else {
                    return false;
                };
                close_tab(&self.handle, tab);
            }
            // With nothing typed the effect is the search key itself: it
            // opens search, and a press over the open search closes it, as
            // the shell's own search key does. A query is typed into the
            // search already open instead.
            DeskEffect::Search { query } => match query {
                None => self.handle.toggle_search(),
                Some(query) => self
                    .handle
                    .open_search(Scope::Workspace, Some(query.clone())),
            },
            DeskEffect::ChooseSearchRow { row, argument } => self.handle.choose_search_row(
                usize::try_from(*row).unwrap_or(usize::MAX),
                argument.clone(),
            ),
            DeskEffect::Launchpad { card } => {
                if card == "apps" {
                    self.handle.open_search(Scope::WindowGroup(None), None);
                } else if let Some(card) = LaunchpadAction::ALL
                    .into_iter()
                    .find(|action| action.slug() == card)
                {
                    self.asks.borrow_mut().push(DesktopAsk::Launchpad(card));
                } else {
                    return false;
                }
            }
            DeskEffect::StepFocus { steps } => {
                self.handle
                    .step_focus(i32::try_from(*steps).unwrap_or_default());
            }
            DeskEffect::FocusDisplay { display } => {
                let Ok(display) = display.parse::<u32>() else {
                    return false;
                };
                self.handle.focus_display(display);
            }
            DeskEffect::MoveToDisplay { steps } => {
                self.handle
                    .move_focused_to_display(i32::try_from(*steps).unwrap_or_default());
            }
        }
        true
    }
}

impl ui::resources::PublishesResources for DeskLayer {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let tabs: Vec<(u64, String)> = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| {
                    let title = tilo_desktop_model::apps::canonical_app_id(&tab.tile).map_or_else(
                        || tab.title.clone(),
                        |app| tilo_desktop_search::names::window_name(&app, &tab.title),
                    );
                    (tab.id, title)
                })
                .collect()
        });
        let (query, rows) = self.handle.search_rows();
        let search = self.handle.search_open().then_some(OpenSearch {
            query: query.as_str(),
            rows: rows.as_slice(),
            searching: self.handle.search_searching(),
        });
        let mut resources = vec![desk_resource(&tabs, &self.handle.displays(), search)];
        if self.handle.read(desk_is_empty) {
            resources.push(launchpad_resource());
        }
        resources
    }
}

impl ui::host_operation::ResourceOperations for DeskLayer {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[DeskEffect::OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        if let Some(request) = op.unscoped()
            && request.type_id == DeskEffect::OPERATION
            && let Ok(verb) = DeskEffect::from_payload(request.payload)
            && self.perform(&verb)
        {
            return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        ui::host_operation::start_in_children(self, op, doc, path)
    }
}

impl Widget for DeskLayer {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::host_operation::ResourceOperations
    );

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let empty = self.handle.read(desk_is_empty);
        let mut changed = self.empty.replace(empty) != empty;
        changed |= self.area.update(state, path);
        changed
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ui::automerge::ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        if crate::launchpad::take_open_app(edits, path) {
            self.handle.open_search(Scope::WindowGroup(None), None);
        }
        if let Some(action) = LaunchpadAction::take(edits, path) {
            self.asks.borrow_mut().push(DesktopAsk::Launchpad(action));
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let backdrop = self.backdrop.build_layout(taffy, theme, map);
        fill(taffy, backdrop);
        let band = self.band.build_layout(taffy, theme, map);
        let area = self.area.build_layout(taffy, theme, map);
        grow(taffy, area);
        let node = taffy.widget_node(column(), &[backdrop, band, area], map, self);
        fill(taffy, node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [backdrop, band, area] = taffy.children(node).unwrap_or_default()[..] {
            self.backdrop.apply_layout(taffy, backdrop, inner);
            self.band.apply_layout(taffy, band, inner);
            self.area.apply_layout(taffy, area, inner);
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
        // The picture is the one of the workspace in front.
        let front = self
            .handle
            .read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0));
        if self.workspace.replace(front) != front {
            self.backdrop
                .set_workspace(front.map_or_else(String::new, |id| id.to_string()));
        }
        self.backdrop.draw(scene, transform, text, theme, presence);
        ui::widget::draw_child(&mut self.area, scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let [_, _, area] = ctx.taffy.children(ctx.node_id).unwrap_or_default()[..] {
            self.area
                .handle_event(event, &mut ctx.reborrow(ctx.path, area));
        }
    }

    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        self.surface_view_only_scroll(event)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.backdrop, &self.band, &self.area]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.backdrop, &mut self.band, &mut self.area]
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

    fn set_interaction_environment(&mut self, environment: ui::InteractionEnvironment) {
        for child in self.children_mut() {
            child.set_interaction_environment(environment);
        }
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        ui::widget::StructureChange::of_children(self.children_mut())
    }

    fn needs_redraw(&self) -> bool {
        self.backdrop.needs_redraw()
            || self.backdrop.needs_animation_frame()
            || (self.area.needs_redraw() || self.area.needs_animation_frame())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if (self.backdrop.needs_redraw() || self.backdrop.needs_animation_frame()) {
            regions.push(self.rect);
            return;
        }
        self.area.collect_damage(regions);
    }

    fn needs_animation_frame(&self) -> bool {
        self.backdrop.needs_animation_frame() || self.area.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [&self.backdrop as &dyn Widget, &self.area]
            .iter()
            .filter_map(|child| child.next_wake_deadline())
            .min()
    }

    fn name(&self) -> &'static str {
        "DeskLayer"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The overview's view of a host: no app artwork and no workspace
/// settings, so shares are named by their scope, and each window wears its
/// app's mark as on every host.
struct PlainOverview {
    user: String,
}

impl WindowSource for PlainOverview {}

impl OverviewHost for PlainOverview {
    fn local_user(&self) -> &str {
        &self.user
    }

    fn scope_label(&self, scope: &collab_types::caps::Scope) -> String {
        crate::workspace_settings::scope_label(scope, &crate::workspace_settings::NoAppDirectory)
    }

    fn node_label(&self, node: &tilo_action_policy::TreePath) -> String {
        crate::workspace_settings::node_phrase(node, &crate::workspace_settings::NoAppDirectory)
    }
}

/// What lies above the windows: the top bar, the strip ribbon and rail,
/// the overview panel and the menus the bar opens; and the desktop's
/// chords.
pub struct DesktopChrome {
    bar: TopBarChrome,
    /// The bars of the other displays, each on its own: a display besides
    /// the one in use carries the compact bar.
    compact: Vec<TopBarChrome>,
    /// Where the bars stood at the bar's source last reading, and the
    /// snapshot revision it was read at.
    placement: Placement,
    bars_revision: u64,
    overview: OverviewPanel,
    tilo_menu: TiloMenu,
    workspace_menu: WorkspaceMenu,
    /// What the person can do to the window in front, at its window.
    /// Where the person is in the workspace (the strip ribbon grown into
    /// the strip switcher, and the window rail).
    navigator: StripNavigator,
    /// The emoji picker: Super+. opens its card at the caret of the field the
    /// person types into, and every pick is written into that field by the
    /// host.
    emoji: EmojiPicker,
    /// Where the picker's next opening goes, and what came of the last pick.
    emoji_link: EmojiLink,
    /// The display the open card stands on.
    emoji_viewport: Rect,
    /// Where the navigator's ribbon and rail stand and what they report.
    navigator_frame: StripFrame,
    /// What the navigator last read: the navigation and who-is-here
    /// revisions, whether the overview covered the desktop, and the content
    /// area.
    navigator_seen: Option<(u64, u64, bool, Rect)>,
    /// The tree revision the open overview shows.
    overview_revision: u64,
    /// Whether the overview is open, as the chrome last drew it, for the
    /// host whose clock flies its windows in and out. The panel speaks only
    /// when it moves: where the shell carries the overview, the panel stays
    /// shut and the shell's own surface is the one that has the flag.
    overview_open: Option<Rc<Cell<bool>>>,
    /// What the overview's panel last said of itself.
    overview_said: bool,
    /// Whether the bar last drew its Tilo mark pressed (its menu open).
    menu_shown: Cell<bool>,
    handle: DesktopHandle,
    generation: String,
    rules: Rc<dyn DesktopRules>,
    /// The desktop's shortcut table.
    shortcuts: ShortcutConfig,
    /// Chords read the host's stand-in for Super as a Mac's.
    mac: bool,
    /// The pointer, in logical pixels.
    cursor: Option<Point>,
    /// The area under the bar, as last laid out.
    content: Rect,
    rect: Rect,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// What a host's bar source raised, carried out on the next update.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// The surfaces the chrome draws, as the session's overlay stack holds
    /// them: which are up, what kind each is, and which holds the keyboard.
    overlays: Overlays,
    /// Whether the reader's focus sits on the chrome, as the focus framework
    /// put it there; cleared as soon as no surface the chrome draws holds the
    /// keyboard, which the stack says.
    focused: bool,
    /// When each display's bar shows over the window covering it: shared
    /// with the desk's band, so the bar's room and the bars it is in agree.
    reveals: Rc<RefCell<Reveals>>,
    /// The way out of the window that filled a display, said once over it.
    fullscreen_hint: FullscreenHint,
    /// Which screen is which, said on each display for a moment after
    /// Settings asks.
    identify: DisplayIdentify,
    /// Where the displays are the identify cards stand on, each by its
    /// monitor, as last read from the desk.
    identify_displays: Vec<(String, Rect)>,
    /// The Identify request answered, so one press shows the cards once.
    answered: Option<u64>,
    /// The desk's search palette, over the windows the desk draws: it is the
    /// one surface here that must cover a window in front, so the chrome
    /// draws it rather than the desk under them.
    search: SearchOverlay,
}

/// The overlays the chrome paints itself, as opposed to the retained widgets
/// it hands to layout: what a screen reader only reaches if the chrome puts
/// them in its own tree.
#[derive(Clone, Copy)]
enum PaintedOverlay {
    /// The strip ribbon's labels, open for the keyboard or under the pointer.
    StripLabels,
    /// The emoji card.
    Emoji,
    /// The way out of the window that filled a display.
    Hint,
    /// Which screen is which, after Settings asked.
    Identify,
    /// The search palette.
    Search,
}

impl PaintedOverlay {
    /// Every painted overlay, bottom to top.
    const ALL: [Self; 5] = [
        Self::StripLabels,
        Self::Emoji,
        Self::Hint,
        Self::Identify,
        Self::Search,
    ];

    /// The key its node keeps in the chrome's tree, so the id survives the
    /// other overlays opening and closing.
    fn key(self) -> u64 {
        ui::a11y::hash_key(&match self {
            Self::StripLabels => "chrome-strip-labels",
            Self::Emoji => "chrome-emoji-card",
            Self::Hint => "chrome-fullscreen-way-out",
            Self::Identify => "chrome-display-identify",
            Self::Search => "chrome-search-palette",
        })
    }

    /// Whether `chrome` has it on screen.
    fn is_open(self, chrome: &DesktopChrome) -> bool {
        match self {
            Self::StripLabels => chrome.navigator.is_open(),
            Self::Emoji => chrome.emoji.is_open(),
            Self::Hint => chrome.fullscreen_hint.is_up(ui::time::now()),
            Self::Identify => chrome.identify.is_up(ui::time::now()),
            Self::Search => chrome.search.is_open(),
        }
    }
}

impl DesktopChrome {
    /// The chrome over the desktop `handle` drives: `bar` (its source
    /// answering the clock, the person and their notifications) and
    /// `tilo_menu`, under `rules`. Commands it sends carry `generation`;
    /// `mac` reads chords with a Mac's keys.
    #[must_use]
    pub fn new(
        handle: DesktopHandle,
        generation: String,
        rules: Rc<dyn DesktopRules>,
        bar: TopBarChrome,
        tilo_menu: TiloMenu,
        mac: bool,
    ) -> Self {
        let search = SearchOverlay::new(handle.clone());
        Self {
            bar,
            compact: Vec::new(),
            placement: Placement::default(),
            bars_revision: u64::MAX,
            overview: OverviewPanel::new(),
            tilo_menu,
            workspace_menu: WorkspaceMenu::new(),
            navigator: StripNavigator::default(),
            emoji: EmojiPicker::new(),
            emoji_link: EmojiLink::default(),
            emoji_viewport: Rect::ZERO,
            navigator_frame: StripFrame::empty(Rect::ZERO),
            navigator_seen: None,
            overview_revision: 0,
            overview_open: None,
            overview_said: false,
            menu_shown: Cell::new(false),
            handle,
            generation,
            rules,
            shortcuts: ShortcutConfig::default(),
            mac,
            cursor: None,
            content: Rect::ZERO,
            rect: Rect::ZERO,
            asks: Rc::default(),
            intents: Rc::default(),
            overlays: Overlays::new(),
            focused: false,
            reveals: Rc::default(),
            fullscreen_hint: FullscreenHint::default(),
            identify: DisplayIdentify::default(),
            identify_displays: Vec::new(),
            answered: None,
            search,
        }
    }

    /// Follow the bars' reveal with `reveals`, which the desk's band reads
    /// too: when each bar shows over the window covering its display.
    pub fn set_reveals(&mut self, reveals: Rc<RefCell<Reveals>>) {
        self.reveals = reveals;
    }

    /// The surfaces the chrome draws, as the session's stack holds them.
    #[must_use]
    pub const fn overlays(&self) -> &Overlays {
        &self.overlays
    }

    /// Publish the chrome's surfaces to `overlays`: the session's stack, which
    /// says which of them holds the keyboard and what a press outside them
    /// closes.
    pub fn set_overlays(&mut self, overlays: Overlays) {
        self.overlays = overlays;
        self.publish_overlays();
    }

    /// Keep the emoji picker's openings, picks and recents through `link`: the
    /// host says where the focused field's caret is, writes each pick into
    /// that field and keeps the recents where the link's store does.
    pub fn set_emoji(&mut self, link: EmojiLink) {
        self.emoji_link = link;
    }

    /// The emoji picker, as the host sees it: open or not, the card's place,
    /// and what the person picked.
    #[must_use]
    pub const fn emoji(&self) -> &EmojiPicker {
        &self.emoji
    }

    /// Use `shortcuts` for the desktop's chords (the person's bindings).
    pub fn set_shortcuts(&mut self, shortcuts: ShortcutConfig) {
        self.shortcuts = shortcuts;
    }

    /// The top bar.
    #[must_use]
    pub const fn bar(&self) -> &TopBarChrome {
        &self.bar
    }

    /// The top bar, to open what it anchors.
    pub fn bar_mut(&mut self) -> &mut TopBarChrome {
        &mut self.bar
    }

    /// Where every display's bar stands, as the bar's source last said.
    #[must_use]
    pub const fn placement(&self) -> &Placement {
        &self.placement
    }

    /// Every bar the chrome shows: the full one on the display in use,
    /// then the compact one of each other display in the placement's
    /// order, with the display each stands on (`None` for the full bar).
    #[must_use]
    pub fn bars(&self) -> Vec<(Option<u32>, &TopBarChrome)> {
        std::iter::once((self.bar.display(), &self.bar))
            .chain(self.compact.iter().map(|bar| (bar.display(), bar)))
            .collect()
    }

    /// The Tilo menu behind the bar's mark.
    pub fn tilo_menu_mut(&mut self) -> &mut TiloMenu {
        &mut self.tilo_menu
    }

    /// Whether the overview is open over the desk.
    #[must_use]
    pub fn overview_open(&self) -> bool {
        self.overview.is_open()
    }

    /// Whether a menu the chrome opened is up.
    #[must_use]
    pub fn menu_open(&self) -> bool {
        self.tilo_menu.is_open() || self.workspace_menu.is_open()
    }

    /// Say what the chrome's own surfaces are to the stack: which are up and
    /// which are gone. Reached from everywhere they open, close or take a
    /// key, so the stack's answer — which of them holds the keyboard, what a
    /// press outside one closes — is what the chrome shows.
    fn publish_overlays(&mut self) {
        for (slot, open) in [
            (Slot::Emoji, self.emoji.is_open()),
            (Slot::WorkspaceMenu, self.workspace_menu.is_open()),
            (Slot::TiloMenu, self.tilo_menu.is_open()),
            (Slot::Overview, self.overview.is_open()),
            (Slot::Strips, self.navigator.takes_keys()),
            (Slot::Search, self.search.is_open()),
        ] {
            if open == self.overlays.is_open(slot) {
                continue;
            }
            if open {
                self.overlays.open(slot, None);
            } else {
                self.overlays.close(slot);
            }
        }
        if !self.is_focusable() {
            self.focused = false;
        }
    }

    /// The chrome's own accessibility node: the group over the desk that
    /// carries what the chrome paints itself. The retained parts (the bars,
    /// the overview, the menus) are children widgets, which the host's walk
    /// exports on its own.
    fn accessibility_group(&self) -> ui::a11y::AccessNode {
        let holds = self.is_focusable();
        ui::a11y::AccessNode::new(ui::a11y::Role::Group).with_state(
            ui::a11y::AccessState::new()
                .focusable(holds)
                .focused(self.focused),
        )
    }

    /// The painted overlays on screen now, bottom to top — the order a
    /// reader meets them in.
    fn painted_overlays(&self) -> Vec<PaintedOverlay> {
        PaintedOverlay::ALL
            .into_iter()
            .filter(|overlay| overlay.is_open(self))
            .collect()
    }

    /// What one painted overlay says to assistive technology, while it is up.
    fn overlay_accessibility(&self, overlay: PaintedOverlay) -> Option<ui::a11y::AccessNode> {
        match overlay {
            PaintedOverlay::StripLabels => self.navigator.accessibility(),
            PaintedOverlay::Emoji => self.emoji.accessibility(),
            PaintedOverlay::Hint => self.fullscreen_hint.accessibility(ui::time::now()),
            PaintedOverlay::Identify => self.identify.accessibility(ui::time::now()),
            PaintedOverlay::Search => self.search.accessibility(),
        }
    }

    /// Open the emoji picker at the caret of the field the person types into,
    /// centred on the display they are working on when that field reported
    /// none. What is picked goes to the host, which writes it into that
    /// field.
    pub fn open_emoji(&mut self) {
        let place = self.emoji_link.place().unwrap_or_default();
        self.emoji_viewport = place
            .viewport
            .or_else(|| self.working_display())
            .unwrap_or(self.rect);
        self.emoji.open_at(
            self.emoji_viewport,
            place.caret,
            &ui::i18n::I18nManager::current_locale(),
        );
        self.publish_overlays();
    }

    /// The display the person is working on, while the desk spans several:
    /// where a card without a caret is centred.
    fn working_display(&self) -> Option<Rect> {
        let tree = self.rules.tree();
        let focused = tree.outputs.focused_display()?;
        tree.outputs
            .displays
            .iter()
            .find(|display| display.id == focused)
            .map(|display| {
                Rect::new(
                    display.x,
                    display.y,
                    display.x + display.width,
                    display.y + display.height,
                )
            })
    }

    /// Close the emoji picker without inserting anything.
    pub fn close_emoji(&mut self) {
        self.emoji.close();
        self.publish_overlays();
    }

    /// Super+. opens the picker, and closes it again while it is up.
    pub fn toggle_emoji(&mut self) {
        if self.emoji.is_open() {
            self.close_emoji();
        } else {
            self.open_emoji();
        }
    }

    /// Hand the host the emoji the person picked since the card last ran, for
    /// it to write into the field they were typing into.
    fn note_emoji_picks(&mut self) {
        for text in self.emoji.take_commits() {
            self.emoji_link.note_picked(text);
        }
    }

    /// Whether a wheel at `point` steps strips (over the ribbon or its
    /// labels) rather than panning the desk.
    #[must_use]
    pub fn takes_wheel_at(&self, point: Point) -> bool {
        self.navigator.takes_wheel_at(&self.navigator_frame, point)
    }

    /// What the person asked of the host since the last call.
    pub fn take_asks(&mut self) -> Vec<DesktopAsk> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// Put the chrome's asks in `asks`, shared with a host that reads them
    /// outside the widget tree the chrome is drawn in.
    pub fn set_asks(&mut self, asks: Rc<RefCell<Vec<DesktopAsk>>>) {
        self.asks = asks;
    }

    /// Take the intents a host's bar source raises from `intents`,
    /// shared with that source.
    pub fn set_intents(&mut self, intents: Rc<RefCell<Vec<ChromeIntent>>>) {
        self.intents = intents;
    }

    /// Have the overview's windows drawn by the host through `flight`, their
    /// pictures resting where the panel draws them — flying between the desk
    /// and the overview on the clock the host keeps there; `open` is set as
    /// the chrome draws, for the host that clock belongs to.
    pub fn set_overview(&mut self, flight: WindowFlight, open: Rc<Cell<bool>>) {
        self.overview.set_flight(flight);
        self.overview_open = Some(open);
    }

    /// Close the menus the chrome opened: the host opens a popover of its
    /// own, one at a time.
    pub fn close_menus(&mut self) {
        if self.tilo_menu.is_menu_open() {
            self.tilo_menu.toggle(Rect::ZERO, self.rect);
        }
        self.workspace_menu.close();
    }

    /// Say what became of an action the person asked for, and say nothing
    /// when it went through.
    fn report(&self, done: Result<(), String>) {
        match done {
            Ok(()) => {}
            Err(refusal) => self.handle.acknowledge(refusal),
        }
    }

    fn dispatch(&self, verb: impl Into<tilo_desktop_surface::navigation::Verb>) {
        if let Err(error) = self.handle.dispatch(verb) {
            tracing::warn!(%error, "desktop chrome");
        }
    }

    /// History of the active workspace, showing only what `author` did
    /// when one is named.
    pub fn activity_by(&self, author: Option<String>) {
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|w| w.slot.0.to_string()));
        if let Some(workspace) = workspace {
            self.dispatch(ChromeEffect::Activity {
                workspace,
                view: ActivityView::CatchUp,
                author,
            });
        }
    }

    /// Carry out what the bar asked for; what only the host answers is
    /// asked of it ([`DesktopAsk::Intent`]).
    pub fn apply_intent(&mut self, intent: ChromeIntent) {
        match intent {
            ChromeIntent::SwitchWorkspace(delta) => {
                let next = self.handle.read(|tree| {
                    let index = tree.workspaces.iter().position(|w| w.slot == tree.active)?;
                    let next = index.checked_add_signed(isize::try_from(delta).ok()?)?;
                    tree.workspaces.get(next).map(|w| w.slot.0.to_string())
                });
                match next {
                    Some(workspace) => self.dispatch(ChromeEffect::SwitchWorkspace {
                        workspace: Some(workspace),
                        delta: None,
                    }),
                    // Stepping past the end says which end it was.
                    None if delta > 0 => self
                        .handle
                        .acknowledge(crate::i18n::t("desk-last-workspace")),
                    None if delta < 0 => {
                        self.handle
                            .acknowledge(crate::i18n::t("desk-first-workspace"));
                    }
                    None => {}
                }
            }
            ChromeIntent::ShowActivity(_) => self.activity_by(None),
            ChromeIntent::ToggleWorkspaceMenu => {
                if self.tilo_menu.is_menu_open() {
                    self.tilo_menu.toggle(Rect::ZERO, self.rect);
                }
                let model = self.workspace_menu_model();
                self.workspace_menu
                    .toggle(&model, self.bar.workspace_menu_rect(), self.rect);
            }
            ChromeIntent::ToggleOverview => self.toggle_overview(),
            ChromeIntent::RenameWorkspace { name } => self.rename_workspace(name),
            ChromeIntent::ToggleTiloMenu => {
                self.workspace_menu.close();
                self.tilo_menu.toggle(self.bar.tilo_mark_rect(), self.rect);
            }
            // The strip's name in the bar opens the strip switcher, as a
            // menu on the shown strip.
            ChromeIntent::ShowStrips => self
                .navigator
                .open_for_choosing(&self.navigator_frame, ui::time::now()),
            ChromeIntent::OpenAvatarAsk => {
                self.handle.set_personal_ask_open(true);
            }
            ChromeIntent::CollapseAvatarAsk => {
                self.handle.set_personal_ask_open(false);
            }
            ChromeIntent::AnswerAvatarAsk => {
                self.handle.answer_personal_ask();
            }
            ChromeIntent::PutOffAvatarAsk => {
                self.handle.put_off_personal_ask();
            }
            other => self.asks.borrow_mut().push(DesktopAsk::Intent(other)),
        }
    }

    /// What the workspace menu offers now: this desktop's workspaces and
    /// the person's others the host knows, where it is hosted, the host's
    /// own workspace commands, and what this person may change.
    fn workspace_menu_model(&self) -> WorkspaceMenuModel {
        let actions = self.handle.host_actions();
        let mut workspaces = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| WorkspaceChoice {
                    key: format!("{WORKSPACE_KEY}{}", workspace.slot.0),
                    name: workspace.name.clone(),
                    current: workspace.slot == tree.active,
                })
                .collect::<Vec<WorkspaceChoice>>()
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
            host: self.rules.workspace_host(),
            actions: actions
                .iter()
                .filter(|action| action.source == Some(HostActionSource::Workspace))
                .map(|action| WorkspaceAction {
                    id: action.id.clone(),
                    title: action.label.clone().unwrap_or_else(|| action.title.clone()),
                    destructive: action.id.starts_with("leave"),
                })
                .collect(),
            can_rename: self.may_add_or_rename(false),
            can_add_strip: self.may_add_or_rename(true),
        }
    }

    /// Whether the person may add a desk (`add`) or rename the workspace.
    fn may_add_or_rename(&self, add: bool) -> bool {
        let command = if add {
            DesktopCommand::CreateStrip {
                name: String::new(),
            }
        } else {
            DesktopCommand::RenameWorkspace {
                name: String::new(),
            }
        };
        self.rules.may(&command).is_ok()
    }

    fn apply_workspace_choice(&mut self, choice: WorkspaceMenuChoice) {
        match choice {
            WorkspaceMenuChoice::Switch(key) => {
                if let Some(workspace) = key.strip_prefix(WORKSPACE_KEY) {
                    self.dispatch(ChromeEffect::SwitchWorkspace {
                        workspace: Some(workspace.to_owned()),
                        delta: None,
                    });
                } else if let Some(id) = key.strip_prefix(HOST_KEY) {
                    self.asks
                        .borrow_mut()
                        .push(DesktopAsk::HostAction(id.to_owned()));
                }
            }
            WorkspaceMenuChoice::Action(id) => {
                self.asks.borrow_mut().push(DesktopAsk::HostAction(id));
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
            WorkspaceMenuChoice::Rename => self.bar.begin_workspace_rename(),
        }
    }

    /// Run a workspace or strip `command` on the active workspace as the
    /// model's command, once the rules allow it; a refusal is said at once.
    /// The strip it made, if any.
    fn command(&self, command: &DesktopCommand) -> Option<StripId> {
        let applied = self
            .rules
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
    /// person's, and go there. Whether it was made.
    fn add_strip(&self, name: String) -> bool {
        let Some(strip) = self.command(&DesktopCommand::CreateStrip { name }) else {
            return false;
        };
        self.rules.made_strip(strip);
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
    /// path and replicate the result; the strip it made, if any.
    fn edit_workspace(&self, command: &DesktopCommand) -> Result<Option<StripId>, String> {
        let mut tree = self.handle.read(Clone::clone);
        let made =
            apply_to_active_workspace(&mut tree, command, &mut || Ok(self.handle.mint_id()))?;
        self.handle.replace_tree(tree);
        Ok(made)
    }

    /// Open or close the overview.
    pub fn toggle_overview(&mut self) {
        let open = !self.overview.is_open();
        let cards = if open { self.cards() } else { Vec::new() };
        self.overview_revision = self.handle.revision();
        self.overview
            .set_new_workspace(self.rules.may_create_workspace().is_ok());
        self.overview.set_open(open, cards);
    }

    fn cards(&self) -> Vec<crate::overview::WorkspaceCard> {
        let host = PlainOverview {
            user: ui::collaboration::local_user_id().0,
        };
        let now = ui::time::unix_now();
        self.handle.read(|tree| workspace_cards(tree, &host, now))
    }

    fn apply_pick(&mut self, pick: &OverviewPick) {
        if pick.closes() {
            self.overview.set_open(false, Vec::new());
        }
        // The ones with a published verb are that verb; the rest are the
        // desk's own.
        let verb = match pick {
            OverviewPick::Window(tab) => {
                self.handle.focus_tab(*tab);
                None
            }
            OverviewPick::Workspace(id) => Some(ChromeEffect::SwitchWorkspace {
                workspace: Some(id.0.to_string()),
                delta: None,
            }),
            OverviewPick::NewWorkspace => {
                self.new_workspace(None, None);
                None
            }
            OverviewPick::JoinWorkspace => Some(ChromeEffect::JoinWorkspace),
            OverviewPick::LeaveWorkspace(id) => Some(ChromeEffect::LeaveWorkspace {
                workspace: Some(id.0.to_string()),
            }),
            OverviewPick::CloseWindow(tab) => {
                self.close_window(*tab);
                None
            }
            OverviewPick::MoveWindow { tab, to } => Some(ChromeEffect::MoveTab {
                tab: tab.to_string(),
                to: Destination {
                    workspace: Some(to.0.to_string()),
                    ..Destination::default()
                },
            }),
            OverviewPick::MoveWindowToStrip { tab, strip: to } => Some(ChromeEffect::MoveTab {
                tab: tab.to_string(),
                to: Destination {
                    strip: Some(to.0.to_string()),
                    ..Destination::default()
                },
            }),
            OverviewPick::RenameStrip { strip: id, name } => Some(ChromeEffect::Rename {
                strip: Some(id.0.to_string()),
                name: name.clone(),
                group: None,
                workspace: None,
            }),
            OverviewPick::MoveStrip { strip: id, before } => Some(ChromeEffect::MoveStrip {
                strip: id.0.to_string(),
                before: before.map(|strip| strip.0.to_string()),
            }),
            OverviewPick::Dismiss => None,
        };
        if let Some(verb) = verb {
            let _ = self.perform(&verb);
        }
    }

    /// Close the window `tab` here, as the close on its own bar does.
    fn close_window(&self, tab: TabId) {
        close_tab(&self.handle, tab);
    }

    /// The workspace `key` names: its id, or its name.
    fn workspace_named(&self, key: &str) -> Result<WorkspaceSlot, String> {
        let key = key.trim();
        self.handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.name.eq_ignore_ascii_case(key))
                    .or_else(|| {
                        key.parse::<u64>().ok().and_then(|id| {
                            tree.workspaces
                                .iter()
                                .find(|workspace| workspace.slot.0 == id)
                        })
                    })
                    .map(|workspace| workspace.slot)
            })
            .ok_or_else(|| crate::i18n::t_args("desktop-no-workspace", &[("name", key.into())]))
    }

    /// The strip `key` names in the workspace the person is in: its id, or
    /// its name.
    fn strip_named(&self, key: &str) -> Result<StripId, String> {
        let key = key.trim();
        let missing = || crate::i18n::t_args("desktop-no-strip", &[("name", key.into())]);
        self.handle
            .read(|tree| {
                let workspace = tree.active_workspace()?;
                workspace
                    .strips
                    .iter()
                    .find(|strip| strip.name.eq_ignore_ascii_case(key))
                    .or_else(|| {
                        key.parse::<u64>()
                            .ok()
                            .and_then(|id| workspace.strips.iter().find(|strip| strip.id.0 == id))
                    })
                    .map(|strip| strip.id)
            })
            .ok_or_else(missing)
    }

    /// Leave the workspace `id` (the one the person is in when absent):
    /// struck from this device's desktop, the person going to the next one
    /// when it was the one they were in.
    fn leave_workspace(&self, id: Option<WorkspaceSlot>) -> Result<(), String> {
        let mut tree = self.handle.read(Clone::clone);
        let id = id.or_else(|| tree.active_workspace().map(|workspace| workspace.slot));
        let id = id.ok_or_else(|| crate::i18n::t("desktop-no-workspace-to-leave"))?;
        if tree.workspaces.len() <= 1 {
            return Err(crate::i18n::t("desktop-only-workspace"));
        }
        let at = tree
            .workspaces
            .iter()
            .position(|workspace| workspace.slot == id)
            .ok_or_else(|| crate::i18n::t("desktop-workspace-gone"))?;
        let left = tree.workspaces.remove(at);
        if left.slot == tree.active {
            // The next one along, or the one before it when this was the
            // last: what stood where the person was.
            let next_at = at.min(tree.workspaces.len().saturating_sub(1));
            if let Some(next) = tree.workspaces.get(next_at) {
                tree.active = next.slot;
            }
        }
        tree.active_strip.remove(&id);
        let strips: Vec<StripId> = tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .map(|strip| strip.id)
            .collect();
        tree.active_group.retain(|strip, _| strips.contains(strip));
        self.handle.replace_tree(tree);
        self.asks
            .borrow_mut()
            .push(DesktopAsk::LeftWorkspace(left.name));
        Ok(())
    }

    /// Make a workspace after the others and go there; with `join`, the
    /// workspace holding that id, joining it when this computer has none (a
    /// workspace that exists is a workspace that exists, so joining one is holding
    /// it, which needs no workspace of the person's own to make).
    fn new_workspace(&self, join: Option<&str>, name: Option<&str>) {
        let mut tree = self.handle.read(Clone::clone);
        let name = Self::name_of(name, tree.workspaces.len());
        let mut workspace = if let Some(id) = join {
            let Some(joined) = WorkspaceId::parse(id) else {
                self.handle.acknowledge(tilo_desktop_surface::i18n::t(
                    "desktop-joined-workspace-unknown",
                ));
                return;
            };
            if let Some(held) = tree.workspaces.iter().find(|held| held.id() == joined) {
                // Already held, here or from another device: go there rather
                // than make a second workspace of one workspace.
                let held = held.slot;
                tree.active = held;
                self.handle.replace_tree(tree);
                return;
            }
            Workspace::in_workspace(WorkspaceSlot::of_workspace(joined), joined, name)
        } else {
            if let Err(refusal) = self.rules.may_create_workspace() {
                self.handle.acknowledge(refusal);
                return;
            }
            Workspace::new(WorkspaceSlot(self.handle.mint_id()), name)
        };
        let strip = Strip::new(StripId(self.handle.mint_id()), "Main");
        let strip_id = strip.id;
        workspace.strips.push(strip);
        let id = workspace.slot;
        tree.workspaces.push(workspace);
        tree.active_strip.insert(id, strip_id);
        tree.active = id;
        self.handle.replace_tree(tree);
    }

    /// What a workspace made with `name` is called: that name, or one
    /// numbered by how many workspaces `held` there are here.
    fn name_of(name: Option<&str>, held: usize) -> String {
        name.map_or_else(
            || {
                tilo_desktop_surface::i18n::t_args(
                    "browser-new-workspace-name",
                    &[("number", held.saturating_add(1).into())],
                )
            },
            str::to_owned,
        )
    }

    /// The window `step` places from the focused one in the shown strip, or
    /// the strip's `nth` window.
    fn window_tab(&self, step: Option<isize>, nth: Option<usize>) -> Option<u64> {
        self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip_id = tree.active_strip.get(&workspace.slot).copied()?;
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
    /// active workspace; whether there was one.
    fn go_to_strip(&self, pick: impl FnOnce(usize) -> Option<usize>) -> bool {
        let next = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let current = tree
                .active_strip
                .get(&workspace.slot)
                .and_then(|id| workspace.strips.iter().position(|s| s.id == *id))
                .unwrap_or(0);
            let strip = workspace.strips.get(pick(current)?)?;
            Some((workspace.slot.0.to_string(), strip.id.0.to_string()))
        });
        if let Some((workspace, strip)) = next {
            self.dispatch(ChromeEffect::ShowStrip { workspace, strip });
            true
        } else {
            false
        }
    }

    /// Step strips by `steps`, saying so when the person steps past an end.
    fn step_strip(&self, steps: isize) {
        if !self.go_to_strip(|current| current.checked_add_signed(steps)) {
            if steps > 0 {
                self.handle.acknowledge(crate::i18n::t("desk-last-strip"));
            } else if steps < 0 {
                self.handle.acknowledge(crate::i18n::t("desk-first-strip"));
            }
        }
    }

    /// Maximize the focused window (`Some(true)`), restore it
    /// (`Some(false)`), or toggle it (`None`).
    fn maximize_focused(&self, want: Option<bool>) {
        let maximized = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip = *tree.active_strip.get(&workspace.slot)?;
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
        self.dispatch(ChromeEffect::Maximize {
            group: route.group,
            on: target,
        });
    }

    /// Cover the display with the focused window, or give it back.
    fn toggle_fullscreen(&self) {
        let covering = self.handle.read(|tree| {
            let focused = tree
                .active_strip_id()
                .and_then(|strip| tree.active_group_for(strip));
            focused.is_some() && focused == tree.fullscreen_group()
        });
        let Some(route) = self.handle.snapshot(&self.generation, Vec::new()).active else {
            return;
        };
        self.dispatch(ChromeEffect::Fullscreen {
            group: route.group,
            on: !covering,
        });
    }

    /// A desktop chord, applied; false when `event` is not one. The chord
    /// is looked up in the desktop's one shortcut table, with a host's
    /// stand-in for Super counting as Super ([`page_shortcut_action`]).
    pub fn chord(&mut self, event: &winit::event::KeyEvent, modifiers: &Modifiers) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        let action = page_shortcut_action(
            &self.shortcuts,
            &event.logical_key,
            event.physical_key,
            *modifiers,
            self.mac,
        );
        if let Some(action) = action {
            return self.shortcut(action);
        }
        // Super+S opens the strip switcher for the keyboard, fixed rather
        // than rebindable.
        let strips = page_chord(&event.logical_key, event.physical_key, *modifiers, self.mac)
            .is_some_and(|(key, held)| {
                held.state() == winit::keyboard::ModifiersState::SUPER
                    && key == winit::keyboard::Key::Character("s".into())
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
        let covered = self.overview.is_open();
        let seen = (
            self.handle.navigation_revision(),
            self.rules.people_revision(),
            covered,
            self.content,
        );
        if self.navigator_seen.replace(seen) == Some(seen) {
            return;
        }
        let tree = self.rules.tree();
        let author = self.rules.author();
        // Who names a strip is the desktop's one policy: whoever governs the
        // workspace, and anyone the strips they made.
        let may_rename = |strip: StripId| {
            self.rules
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
                // Offered as the workspace menu's is: to whom the rules let
                // add one.
                may_add: self.may_add_or_rename(true),
            },
        ));
        self.navigator_frame = StripFrame::of_tree(&tree, self.content, (&shown, scroll), covered);
        self.navigator
            .observe(&self.navigator_frame, ui::time::now());
    }

    /// Carry out what the strip ribbon, its labels or the rail asked for.
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
                self.step_strip(isize::try_from(steps).unwrap_or_default());
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
                .acknowledge(crate::i18n::t("strip-switcher-refused"));
        }
        // A new strip is listed at once, so its name goes in place.
        self.navigator_seen = None;
        self.observe_navigation();
    }

    /// Whether the strip ribbon, its labels or the rail took `event`.
    fn navigation_event(&mut self, event: &WindowEvent, scale: f64, modifiers: Modifiers) -> bool {
        use winit::event::{Ime, MouseButton};
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

    /// Perform a desktop shortcut: false for one the desktop has no part
    /// in (locking the device, the magnifier, dictation, …), which then
    /// reaches the window as an ordinary key.
    pub fn shortcut(&mut self, action: ShortcutAction) -> bool {
        match action {
            ShortcutAction::WorkspaceOverview => self.toggle_overview(),
            ShortcutAction::EmojiPicker => self.toggle_emoji(),
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
            ShortcutAction::PreviousStrip => self.step_strip(-1),
            ShortcutAction::NextStrip => self.step_strip(1),
            ShortcutAction::PreviousWorkspace => {
                self.apply_intent(ChromeIntent::SwitchWorkspace(-1));
            }
            ShortcutAction::NextWorkspace => self.apply_intent(ChromeIntent::SwitchWorkspace(1)),
            ShortcutAction::MaximizeWindow => self.maximize_focused(Some(true)),
            ShortcutAction::RestoreWindow => self.maximize_focused(Some(false)),
            // The window the person closed last in the shown workspace comes
            // back where it was. Nothing closed is no reason to leave the
            // chord to whatever holds the keyboard: the desktop answers it
            // either way, as the shell it replaces did.
            ShortcutAction::ReopenClosedTab => {
                self.dispatch(ChromeEffect::ReopenClosed);
            }
            // A window covers the display it stands on by covering the whole
            // strip, bar's room included: the chord and the window menu's
            // row are the same request.
            ShortcutAction::ToggleMaximize => self.maximize_focused(None),
            ShortcutAction::ToggleFullscreen => self.toggle_fullscreen(),
            ShortcutAction::ReportProblem => self.asks.borrow_mut().push(DesktopAsk::ReportProblem),
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

    /// Whether anything of the chrome is due a redraw.
    fn chrome_needs_redraw(&self) -> bool {
        // An intent waiting to be carried out asks for the frame that
        // carries it.
        !self.intents.borrow().is_empty()
            || (self.emoji.needs_redraw() || self.emoji.needs_animation_frame())
            || self.emoji_link.has_delivered()
            || self.navigator.needs_animation_frame(ui::time::now())
            || self.fullscreen_hint.needs_redraw(ui::time::now())
            || self.identify.needs_redraw(ui::time::now())
            || self
                .children()
                .iter()
                .any(|child| child.needs_redraw() || child.needs_animation_frame())
    }
}

impl DesktopChrome {
    /// Put every display's bar where the desk's source says, when what it
    /// says moved: a display arriving or going, the person moving to
    /// another display, or a window going fullscreen there. Returns
    /// whether anything moved.
    ///
    /// A desk of one display carries no places: the full bar then stands
    /// where layout puts it, across the whole desk.
    fn follow_bars(&mut self) -> bool {
        let Some(chrome) = self.bar.chrome().cloned() else {
            return false;
        };
        let revision = chrome.snapshot().revision;
        if revision == self.bars_revision {
            return false;
        }
        self.bars_revision = revision;
        let placement = chrome.bars();
        if placement == self.placement {
            return false;
        }
        // The full bar stands on the display in use, where the person is
        // working; each of the others carries the compact bar on its own
        // display, so each follows its own display's rectangle.
        let full = placement.bars.iter().find(|place| place.full).copied();
        self.bar.set_placement(full.map(|place| place.rect));
        let tree = placement
            .bars
            .iter()
            .any(|place| !place.full)
            .then(|| chrome.tree());
        let mut kept = std::mem::take(&mut self.compact);
        let mut bars = Vec::with_capacity(placement.bars.len().saturating_sub(1));
        for place in placement.bars.iter().filter(|place| !place.full) {
            let reused = kept
                .iter()
                .position(|bar| bar.display() == Some(place.display))
                .map(|at| kept.remove(at));
            let mut bar = if let Some(bar) = reused {
                bar
            } else {
                let Some(tree) = tree.as_ref() else {
                    continue;
                };
                TopBarChrome::from_tree(tree)
                    .compact()
                    .on_display(place.display)
                    .with_chrome(Rc::clone(&chrome))
            };
            bar.set_placement(Some(place.rect));
            bars.push(bar);
        }
        self.compact = bars;
        self.placement = placement;
        true
    }

    /// Bring the bars' reveal up to date at `now`: where each display's bar
    /// stands, which displays a window covers, whether the person hides the
    /// bar (`hide`), and what of the chrome holds it open (`held_extra`).
    /// Returns whether any bar's presence may have changed.
    ///
    /// A bar shows over the window covering its display for the idle time
    /// after a window enters full screen there, and while the person reaches
    /// for it, uses it or keeps one of its menus open; else it is hidden.
    /// The person's own "hide the top bar" is the same machinery without a
    /// fullscreen window.
    pub fn follow_reveals(&mut self, hide: bool, held_extra: bool, now: ui::time::Instant) -> bool {
        let mut changed = self.follow_bars();
        let height = self.bar.bar_rect().height();
        let height = if height > 0.0 {
            height
        } else {
            f64::from(TOPBAR_HEIGHT_PX)
        };
        let full = self.placement.bars.iter().find(|place| place.full);
        // The display in use is the one the full bar stands on; a desk that
        // needs no placing is one display, which the window covers or not.
        let in_use_fullscreen = full.map_or(!self.placement.fullscreen.is_empty(), |place| {
            self.placement.fullscreen.contains(&Some(place.display))
        });
        let mut places = Vec::with_capacity(self.compact.len().saturating_add(1));
        places.push((
            None,
            full.map_or(self.rect, |place| place.rect),
            in_use_fullscreen,
        ));
        for place in self.placement.bars.iter().filter(|place| !place.full) {
            places.push((
                Some(place.display),
                place.rect,
                self.placement.fullscreen.contains(&Some(place.display)),
            ));
        }
        let held = held_extra || self.holds_bar_open();
        let mut reveals = self.reveals.borrow_mut();
        reveals.set_height(height);
        reveals.set_hide(hide);
        reveals.follow(&places, now);
        changed |= reveals.settle(now, |_| held);
        let full_presence = reveals.presence(None, held);
        let compact: Vec<(Option<u32>, TopBarPresence)> = self
            .compact
            .iter()
            .map(|bar| (bar.display(), reveals.presence(bar.display(), held)))
            .collect();
        drop(reveals);
        if self.bar.presence() != full_presence {
            self.bar.set_presence(full_presence);
            changed = true;
        }
        for (bar, (_, presence)) in self.compact.iter_mut().zip(compact) {
            if bar.presence() != presence {
                bar.set_presence(presence);
                changed = true;
            }
        }
        // A window that just filled a display says how to leave it, once;
        // leaving full screen takes the words away with it.
        self.fullscreen_hint.follow(self.reveals.borrow().entered());
        // Settings asked which screen is which: each display says its
        // number and name for a moment.
        changed |= self.follow_identify(now);
        changed
    }

    /// Answer Settings' Identify request, once per press: each display says
    /// the number its tile on the canvas carries and the name Settings gives
    /// it. The displays are read while the cards stand, so a card follows the
    /// display it names when the person moves one mid-answer.
    fn follow_identify(&mut self, now: ui::time::Instant) -> bool {
        let Some(chrome) = self.bar.chrome().cloned() else {
            return false;
        };
        let ask = chrome.snapshot().identify;
        let asked = ask.as_ref().map(|ask| ask.id);
        let standing = self.identify.is_up(now);
        self.identify.follow(ask, now);
        if self.identify.is_up(now) {
            let tree = chrome.tree();
            self.identify_displays = tree
                .outputs
                .displays
                .iter()
                .map(|display| {
                    (
                        display.monitor.clone(),
                        Rect::new(
                            display.x,
                            display.y,
                            display.x + display.width,
                            display.y + display.height,
                        ),
                    )
                })
                .collect();
        }
        let up = self.identify.is_up(now);
        let changed = asked != self.answered || standing != up;
        self.answered = asked;
        changed
    }

    /// Whether something of the chrome holds the bars shown: one of the
    /// bar's menus or cards, an overlay it opened, or the keyboard in it.
    fn holds_bar_open(&self) -> bool {
        self.tilo_menu.is_menu_open()
            || self.workspace_menu.is_open()
            || self.navigator.is_open()
            || self.emoji.is_open()
            || self.bar.card_open()
            || self.bar.holds_itself_shown()
            || self.compact.iter().any(TopBarChrome::holds_itself_shown)
    }

    /// Carry out a verb of the chrome's: whether it did, or why it could
    /// not, which the door that asked answers as a refusal.
    fn perform(&mut self, verb: &ChromeEffect) -> Result<bool, String> {
        if let Some(done) = self.perform_workspace_action(verb) {
            return Ok(done);
        }
        if let Some(done) = self.perform_surface_verb(verb) {
            self.report(done.clone());
            return done.map(|()| true);
        }
        let intent = match verb {
            ChromeEffect::RevealTopBar => {
                self.reveals.borrow_mut().reveal_by_key(ui::time::now());
                return Ok(true);
            }
            ChromeEffect::Overview { open } => {
                let open = open.unwrap_or(!self.overview_open());
                if self.overview_open() != open {
                    self.toggle_overview();
                }
                return Ok(true);
            }
            ChromeEffect::TiloMenu { open } => {
                let open = open.unwrap_or(!self.tilo_menu.is_menu_open());
                if self.tilo_menu.is_menu_open() == open {
                    return Ok(true);
                }
                ChromeIntent::ToggleTiloMenu
            }
            ChromeEffect::QuickSettings => ChromeIntent::ToggleQuickSettings,
            ChromeEffect::WorkspaceMenu => ChromeIntent::ToggleWorkspaceMenu,
            ChromeEffect::SwitchWorkspace {
                delta: Some(delta), ..
            } => ChromeIntent::SwitchWorkspace(i32::try_from(*delta).unwrap_or_default()),
            ChromeEffect::ShowPerson { person } => ChromeIntent::ShowPerson(person.clone()),
            ChromeEffect::ToggleCollaborators => ChromeIntent::ToggleCollaborators,
            ChromeEffect::ShowStrips => ChromeIntent::ShowStrips,
            ChromeEffect::StepStrip { steps } => {
                self.step_strip(isize::try_from(*steps).unwrap_or_default());
                return Ok(true);
            }
            ChromeEffect::NewWorkspace { workspace, name } => {
                self.new_workspace(workspace.as_deref(), name.as_deref());
                return Ok(true);
            }
            ChromeEffect::Shortcut { action } => {
                let Ok(action) = serde_json::from_value::<ShortcutAction>(
                    serde_json::Value::String(action.clone()),
                ) else {
                    return Ok(false);
                };
                if !self.shortcut(action) {
                    self.asks.borrow_mut().push(DesktopAsk::Shortcut(action));
                }
                return Ok(true);
            }
            _ => return Ok(false),
        };
        self.apply_intent(intent);
        Ok(true)
    }

    /// Carry out a verb of the surface's, the ones a page's commands carry
    /// too: the desk admits each against the tree it changes and applies it
    /// on its next pass, so nothing here decides twice.
    fn perform_surface_verb(&self, verb: &ChromeEffect) -> Option<Result<(), String>> {
        let surface = matches!(
            verb,
            ChromeEffect::OpenApp { .. }
                | ChromeEffect::ShowApp { .. }
                | ChromeEffect::MoveTab { .. }
                | ChromeEffect::CloseWindow { .. }
                | ChromeEffect::SplitRight { .. }
                | ChromeEffect::NewGroup { .. }
                | ChromeEffect::SetWidth { .. }
                | ChromeEffect::Equalize { .. }
                | ChromeEffect::Maximize { .. }
                | ChromeEffect::Fullscreen { .. }
                | ChromeEffect::Rename { .. }
                | ChromeEffect::NewStrip { .. }
                | ChromeEffect::ReopenClosed
                | ChromeEffect::ShareWindow { .. }
                | ChromeEffect::StopDesktopControl { .. }
                | ChromeEffect::SwitchWorkspace {
                    workspace: Some(_),
                    ..
                }
                | ChromeEffect::ShowStrip { .. }
                | ChromeEffect::Pan { .. }
                | ChromeEffect::Scroll { .. }
                | ChromeEffect::History { .. }
                | ChromeEffect::Activity { .. }
                | ChromeEffect::CloseActivity { .. }
                | ChromeEffect::Notifications { .. }
        );
        surface.then(|| self.handle.dispatch(verb.clone()))
    }

    /// What the desk itself does to offers of workspaces and strips, by
    /// address: `None` for an action of another kind.
    fn perform_workspace_action(&self, verb: &ChromeEffect) -> Option<bool> {
        match verb {
            ChromeEffect::Rename {
                strip: Some(strip),
                name,
                ..
            } => {
                let renamed = self.strip_named(strip).and_then(|strip| {
                    self.edit_workspace(&DesktopCommand::RenameStrip {
                        strip: strip.0,
                        name: name.clone(),
                    })
                    .map(drop)
                });
                self.report(renamed);
                Some(true)
            }
            ChromeEffect::Rename {
                workspace: Some(workspace),
                name,
                ..
            } => {
                let renamed = self.workspace_named(workspace).and_then(|workspace| {
                    let active = self.handle.read(|tree| tree.active);
                    (active == workspace)
                        .then_some(())
                        .ok_or_else(|| "Workspace is no longer active".to_owned())?;
                    self.edit_workspace(&DesktopCommand::RenameWorkspace { name: name.clone() })
                        .map(drop)
                });
                self.report(renamed);
                Some(true)
            }
            ChromeEffect::MoveStrip { strip, before } => {
                let moved = self.strip_named(strip).and_then(|strip| {
                    let before = before
                        .as_deref()
                        .map(|before| self.strip_named(before))
                        .transpose()?;
                    self.edit_workspace(&DesktopCommand::MoveStrip {
                        strip: strip.0,
                        before: before.map(|before| before.0),
                    })
                    .map(drop)
                });
                self.report(moved);
                Some(true)
            }
            ChromeEffect::JoinWorkspace => {
                self.asks
                    .borrow_mut()
                    .push(DesktopAsk::HostAction(JOIN_WORKSPACE.to_owned()));
                Some(true)
            }
            ChromeEffect::LeaveWorkspace { workspace } => {
                let left = workspace
                    .as_deref()
                    .map_or(Ok(None), |id| self.workspace_named(id).map(Some))
                    .and_then(|id| self.leave_workspace(id));
                self.report(left);
                Some(true)
            }
            ChromeEffect::WorkspaceSettings => {
                self.asks
                    .borrow_mut()
                    .push(DesktopAsk::HostAction(WORKSPACE_SETTINGS.to_owned()));
                Some(true)
            }
            _ => None,
        }
    }
}

impl ui::resources::PublishesResources for DesktopChrome {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![chrome_resource()]
    }
}

impl ui::host_operation::ResourceOperations for DesktopChrome {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[ChromeEffect::OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        if let Some(request) = op.unscoped()
            && request.type_id == ChromeEffect::OPERATION
            && let Ok(verb) = ChromeEffect::from_payload(request.payload)
            && self.perform(&verb)?
        {
            return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        ui::host_operation::start_in_children(self, op, doc, path)
    }
}

impl Widget for DesktopChrome {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::host_operation::ResourceOperations
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let intents = std::mem::take(&mut *self.intents.borrow_mut());
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
        changed |= self.tilo_menu.settle();
        if let Some(choice) = self.workspace_menu.take_choice() {
            self.apply_workspace_choice(choice);
            changed = true;
        }
        let menu_open = self.tilo_menu.is_menu_open();
        if self.menu_shown.replace(menu_open) != menu_open {
            self.bar.set_tilo_menu_open(menu_open);
            changed = true;
        }
        changed |= self.bar.update(state, path);
        changed |= self.follow_bars();
        // Each display's bar keeps its own copy of the desk's tree, and
        // its own clock under it.
        for bar in &mut self.compact {
            changed |= bar.update(state, path);
        }
        // The host says where the recents are kept, and what of the person's
        // picks reached a field: a secret field's emoji is never remembered.
        if let Some(store) = self.emoji_link.take_store() {
            self.emoji.attach_store(store);
            changed = true;
        }
        for (text, sensitive) in self.emoji_link.take_delivered() {
            self.emoji.note_delivered(&text, sensitive);
            changed = true;
        }
        changed |= self.emoji.needs_redraw() || self.emoji.needs_animation_frame();
        changed |= self.search.update(state, path);
        self.observe_navigation();
        self.publish_overlays();
        if let Some(open) = &self.overview_open {
            let said = self.overview.is_open();
            if said != self.overview_said {
                open.set(said);
                self.overview_said = said;
            }
        }
        changed | !self.asks.borrow().is_empty()
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ui::automerge::ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use taffy::{LengthPercentageAuto, Position, Size};
        let bar = self.bar.build_layout(taffy, theme, map);
        let content = taffy.new_leaf(Style::default()).unwrap_or(bar);
        grow(taffy, content);
        // Over the area under the bar, which stays usable while it is open.
        let overview = self.overview.build_layout(taffy, theme, map);
        let mut style = taffy.style(overview).cloned().unwrap_or_default();
        style.position = Position::Absolute;
        style.inset = taffy::Rect {
            left: LengthPercentageAuto::length(0.0),
            right: LengthPercentageAuto::length(0.0),
            top: LengthPercentageAuto::length(TOPBAR_HEIGHT_PX),
            bottom: LengthPercentageAuto::length(0.0),
        };
        style.size = Size::auto();
        let _ = taffy.set_style(overview, style);
        let tilo_menu = self.tilo_menu.build_layout(taffy, theme, map);
        let workspace_menu = self.workspace_menu.build_layout(taffy, theme, map);
        let search = self.search.build_layout(taffy, theme, map);
        let mut kids = vec![bar, content, overview, tilo_menu, workspace_menu, search];
        // Every other display's bar stands over its own display's top
        // edge, laid out across the desk: it takes no room in the column,
        // its place carrying it where its display is.
        for compact in &mut self.compact {
            let node = compact.build_layout(taffy, theme, map);
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = Position::Absolute;
            style.inset = taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::auto(),
            };
            let _ = taffy.set_style(node, style);
            kids.push(node);
        }
        let node = taffy.widget_node(column(), &kids, map, self);
        fill(taffy, node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let kids = taffy.children(node).unwrap_or_default();
        let [
            bar,
            content,
            overview,
            tilo_menu,
            workspace_menu,
            search,
            compact @ ..,
        ] = &kids[..]
        else {
            return;
        };
        self.bar.apply_layout(taffy, *bar, inner);
        self.content = ui::widget::layout_rect(taffy, *content, inner);
        self.overview.apply_layout(taffy, *overview, inner);
        self.tilo_menu.apply_layout(taffy, *tilo_menu, inner);
        self.workspace_menu
            .apply_layout(taffy, *workspace_menu, inner);
        self.search.apply_layout(taffy, *search, inner);
        for (bar, node) in self.compact.iter_mut().zip(compact) {
            bar.apply_layout(taffy, *node, inner);
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
        self.navigator.draw(
            &self.navigator_frame,
            scene,
            transform,
            text,
            theme,
            ui::time::now(),
        );
        self.overview.draw(scene, transform, text, theme, presence);
        for bar in &mut self.compact {
            if bar.presence() != TopBarPresence::Hidden {
                bar.draw(scene, transform, text, theme, presence);
            }
        }
        // Last, so what the bar opens below its band stays above the rest.
        self.bar.draw(scene, transform, text, theme, presence);
        // Above all of it: the emoji picker is modal, the one thing the person
        // works with until it closes.
        if self.emoji.is_open() {
            self.emoji.advance_scroll();
            self.emoji
                .draw(scene, transform, text, theme, presence, self.emoji_viewport);
            self.emoji.mark_drawn();
        }
        // Above everything: the window that filled the display hides the
        // chrome that would leave it, so the words saying how are the only
        // way out the person can see.
        self.fullscreen_hint.draw(scene, transform, text, theme);
        // Settings' Identify, said on each display so the canvas tile and
        // the physical screen carry the same number.
        self.identify
            .draw(&self.identify_displays, scene, transform, text, theme);
        // Topmost: the search palette dims what it stands over, a window in
        // front included, and the card over it.
        self.search.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let kids = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [
            bar,
            _,
            overview,
            tilo_menu,
            workspace_menu,
            search,
            compact @ ..,
        ] = &kids[..]
        else {
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
        // The search palette stands over everything, a window in front
        // included: while it is up, every key and press is its, and its own
        // chord or a press off the card leaves search. While it is down its
        // chord opens it, before a menu or the bar takes that key.
        self.search
            .handle_event(event, &mut ctx.reborrow(ctx.path, *search));
        if *ctx.handled {
            self.publish_overlays();
            return;
        }
        // The emoji picker is modal: its own chord closes it, and every other
        // key and press is the card's, reaching nothing behind it.
        if self.emoji.is_open() {
            let own = matches!(event, WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && page_shortcut_action(
                        &self.shortcuts,
                        &key.logical_key,
                        key.physical_key,
                        *ctx.modifiers,
                        self.mac,
                    ) == Some(ShortcutAction::EmojiPicker));
            if own {
                self.close_emoji();
            } else {
                self.emoji.handle_event(
                    event,
                    ctx.scale_factor,
                    ctx.modifiers.state(),
                    self.cursor,
                    ui::time::now(),
                );
            }
            self.note_emoji_picks();
            *ctx.handled = true;
            self.publish_overlays();
            return;
        }
        // The bar's menus take what they cover, the keyboard and the press
        // that dismisses them, in the order the stack stands them: the
        // workspace's over the Tilo mark's.
        if self.overlays.is_open(Slot::WorkspaceMenu) {
            self.workspace_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, *workspace_menu));
        }
        if !*ctx.handled && self.overlays.is_open(Slot::TiloMenu) {
            self.tilo_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, *tilo_menu));
        }
        if *ctx.handled {
            self.publish_overlays();
            return;
        }
        // The strip switcher's name field and keyboard choosing come before
        // the desktop's chords, as any field's typing does.
        if self.navigation_event(event, ctx.scale_factor, *ctx.modifiers) {
            *ctx.handled = true;
            self.publish_overlays();
            return;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.chord(key, ctx.modifiers)
        {
            *ctx.handled = true;
            self.publish_overlays();
            return;
        }
        // The open overview takes the keyboard and what it covers; the bar
        // above it stays usable.
        if self.overview.is_open() {
            self.overview
                .handle_event(event, &mut ctx.reborrow(ctx.path, *overview));
        }
        if !*ctx.handled {
            self.bar
                .handle_event(event, &mut ctx.reborrow(ctx.path, *bar));
        }
        // The other displays' bars take what is theirs on their own
        // display; a hidden one takes nothing.
        if !*ctx.handled {
            for (bar, node) in self.compact.iter_mut().zip(compact) {
                if bar.presence() == TopBarPresence::Hidden {
                    continue;
                }
                bar.handle_event(event, &mut ctx.reborrow(ctx.path, *node));
                if *ctx.handled {
                    break;
                }
            }
        }
        self.publish_overlays();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![
            &self.bar,
            &self.overview,
            &self.tilo_menu,
            &self.workspace_menu,
            &self.search,
        ];
        children.extend(self.compact.iter().map(|bar| bar as &dyn Widget));
        children
    }

    /// Focus reaches the chrome while a surface it draws holds the keyboard,
    /// so a reader standing in the emoji card stays there as the chrome
    /// rebuilds under it rather than falling back to the window.
    fn is_focusable(&self) -> bool {
        self.overlays.target().slot().is_some_and(Slot::is_chrome)
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// The desk's overlays a screen reader can reach: the labels and the card
    /// the chrome paints, plus the children widgets the walk adds below them.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let children = self
            .painted_overlays()
            .into_iter()
            .filter_map(|overlay| {
                self.overlay_accessibility(overlay)
                    .map(|node| node.with_key(overlay.key()))
            })
            .collect();
        Some(self.accessibility_group().with_children(children))
    }

    fn accessibility_root(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.accessibility_group())
    }

    /// Where the overlay element at `path` is drawn, so assistive
    /// activation presses it where the person sees it.
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let (&overlay, rest) = path.split_first()?;
        let overlay = *self.painted_overlays().get(overlay)?;
        let mut node = self.overlay_accessibility(overlay)?;
        for &step in rest {
            node = node.children.into_iter().nth(step)?;
        }
        node.bounds
            .map(ui::a11y::AccessBounds::to_rect)
            .filter(|rect| rect.area() > 0.0)
    }

    /// A strip label's semantic target, pressed as a click on the label is.
    /// The card names no targets: its elements are not carried out by a
    /// target but pressed where they are drawn, which the host does through
    /// [`Widget::synthetic_accessibility_rect`].
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !target.starts_with(crate::strip_switcher::TARGET_PREFIX)
            || !matches!(action, ui::a11y::Action::Activate)
        {
            return false;
        }
        let Some(answer) = self
            .navigator
            .activate_target(&self.navigator_frame, target)
        else {
            return false;
        };
        if let Some(navigation) = answer {
            self.apply_navigation(navigation);
        }
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![
            &mut self.bar,
            &mut self.overview,
            &mut self.tilo_menu,
            &mut self.workspace_menu,
            &mut self.search,
        ];
        children.extend(self.compact.iter_mut().map(|bar| bar as &mut dyn Widget));
        children
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

    fn set_interaction_environment(&mut self, environment: ui::InteractionEnvironment) {
        for child in self.children_mut() {
            child.set_interaction_environment(environment);
        }
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        ui::widget::StructureChange::of_children(self.children_mut())
    }

    fn needs_redraw(&self) -> bool {
        self.chrome_needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.chrome_needs_redraw() {
            regions.push(self.rect);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.emoji.needs_animation_frame()
            || self.fullscreen_hint.is_moving(ui::time::now())
            || self.identify.is_moving(ui::time::now())
            || self.navigator.needs_animation_frame(ui::time::now())
            || self
                .children()
                .iter()
                .any(|child| child.needs_animation_frame())
    }

    /// The bar's own deadline turns its clock over on the minute, on every
    /// display that shows one.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [
            &self.bar as &dyn Widget,
            &self.overview,
            &self.tilo_menu,
            &self.search,
        ]
        .into_iter()
        .chain(self.compact.iter().map(|bar| bar as &dyn Widget))
        .filter_map(wake_deadline)
        .chain(
            self.navigator
                .next_wake_deadline(&self.navigator_frame, ui::time::now()),
        )
        .chain(self.reveals.borrow().deadline())
        .chain(self.fullscreen_hint.next_wake_deadline(ui::time::now()))
        .chain(self.identify.next_wake_deadline(ui::time::now()))
        .min()
    }

    fn name(&self) -> &'static str {
        "DesktopChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
