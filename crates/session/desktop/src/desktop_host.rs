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
use tilo_desktop_model::{DesktopTree, Strip, StripId, Workspace, WorkspaceId};
use tilo_desktop_surface::navigation::{ActivityView, Command};
use tilo_desktop_surface::{DesktopHandle, HostActionSource};
use ui::presence::PresenceCtx;
use ui::shortcut_table::{ShortcutAction, ShortcutConfig};
use ui::widget::{AppModel, EventCtx, ModelPath, Stack, TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, WindowEvent};

use crate::about::TiloMenu;
use crate::chrome::ChromeIntent;
use crate::desktop_actions::{
    DesktopAction, OPERATION, chrome_resource, desk_resource, launchpad_resource,
};
use crate::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use crate::overview::{OverviewHost, room_cards};
use crate::overview_panel::{OverviewPanel, OverviewPick};
use crate::shortcuts::{page_chord, page_shortcut_action};
use crate::strip_navigator::{StripFrame, StripNavigation, StripNavigator, Viewer};
use crate::strip_switcher::Command as StripCommand;
use crate::topbar::{TOPBAR_HEIGHT_PX, TopBarChrome};
use crate::window_switcher::WindowSource;
use crate::workspace_menu::{
    WorkspaceAction, WorkspaceChoice, WorkspaceMenu, WorkspaceMenuChoice, WorkspaceMenuModel,
};

/// Workspace menu keys: a workspace of this desktop, by id, and one of the
/// person's others the host opens, by its host action.
const WORKSPACE_KEY: &str = "workspace:";
const HOST_KEY: &str = "host:";

/// What the host answers about the person at this desktop and the room it
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

    /// Where the room shown is hosted (`example.org`), named beside the
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
    /// The person asked to report a problem.
    ReportProblem,
    /// The person chose a card of the empty desk's launchpad.
    Launchpad(LaunchpadAction),
    /// A desktop shortcut the layers have no part in (search, quick
    /// settings, locking, the magnifier, …): the host's to carry out.
    Shortcut(ShortcutAction),
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

/// What lies under the windows: the backdrop, and below the bar's band the
/// strips of the desktop surface over the empty desk's launchpad.
pub struct DeskLayer {
    backdrop: crate::backdrop::DesktopBackdrop,
    area: Stack,
    handle: DesktopHandle,
    empty: Rc<Cell<bool>>,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    rect: Rect,
}

impl DeskLayer {
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
            area,
            handle,
            empty,
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
}

impl DeskLayer {
    /// Carry out a desk action; false for one of the chrome's.
    fn perform(&self, action: &DesktopAction) -> bool {
        match action {
            DesktopAction::FocusTab { tab } => {
                self.handle.focus_tab(*tab);
            }
            DesktopAction::CloseTab { tab } => {
                let mut tree = self.handle.read(Clone::clone);
                if tree.close_tab(*tab).is_some() {
                    self.handle.replace_tree(tree);
                }
            }
            DesktopAction::OpenSearch { query } => self.handle.open_search(query.clone()),
            DesktopAction::SearchFor { query } => self.handle.set_search_query(query.clone()),
            DesktopAction::ChooseSearchRow { row, argument } => self.handle.choose_search_row(
                usize::try_from(*row).unwrap_or(usize::MAX),
                argument.clone(),
            ),
            DesktopAction::OpenSearch => self.handle.open_search(None),
            DesktopAction::Launchpad { ref card } => {
                if card == "apps" {
                    self.handle.open_applications();
                } else if let Some(card) = LaunchpadAction::ALL
                    .into_iter()
                    .find(|action| action.slug() == card)
                {
                    self.asks.borrow_mut().push(DesktopAsk::Launchpad(card));
                } else {
                    return false;
                }
            }
            DesktopAction::StepFocus { steps } => {
                self.handle
                    .step_focus(i32::try_from(steps).unwrap_or_default());
            }
            DesktopAction::FocusDisplay { display } => {
                self.handle.focus_display(display);
            }
            DesktopAction::MoveToDisplay { steps } => {
                self.handle
                    .move_focused_to_display(i32::try_from(steps).unwrap_or_default());
            }
            _ => return false,
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
                .map(|tab| (tab.id, tab.title.clone()))
                .collect()
        });
        let (query, rows) = self.handle.search_rows();
        let search = self
            .handle
            .search_open()
            .then_some((query.as_str(), rows.as_slice()));
        vec![desk_resource(&tabs, search)]
        let mut resources = vec![desk_resource(&tabs)];
        let mut resources = vec![desk_resource(&tabs, &self.handle.displays())];
        if self.handle.read(desk_is_empty) {
            resources.push(launchpad_resource());
        }
        resources
    }
}

impl ui::host_operation::ResourceOperations for DeskLayer {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        if let Some(request) = op.unscoped()
            && let Some(action) = DesktopAction::decode(request.type_id, request.payload)
            && self.perform(&action)
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
            self.handle.open_applications();
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
        use taffy::{Dimension, Size};
        let backdrop = self.backdrop.build_layout(taffy, theme, map);
        fill(taffy, backdrop);
        let band = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(TOPBAR_HEIGHT_PX),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap_or(backdrop);
        let area = self.area.build_layout(taffy, theme, map);
        grow(taffy, area);
        let node = taffy.widget_node(column(), &[backdrop, band, area], map, self);
        fill(taffy, node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [backdrop, _, area] = taffy.children(node).unwrap_or_default()[..] {
            self.backdrop.apply_layout(taffy, backdrop, inner);
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
        vec![&self.backdrop, &self.area]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.backdrop, &mut self.area]
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
        self.backdrop.needs_redraw() || self.area.needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.backdrop.needs_redraw() {
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
    overview: OverviewPanel,
    tilo_menu: TiloMenu,
    workspace_menu: WorkspaceMenu,
    /// Where the person is in the workspace (the strip ribbon grown into
    /// the strip switcher, and the window rail).
    navigator: StripNavigator,
    /// Where the navigator's ribbon and rail stand and what they report.
    navigator_frame: StripFrame,
    /// What the navigator last read: the navigation and who-is-here
    /// revisions, whether the overview covered the desktop, and the content
    /// area.
    navigator_seen: Option<(u64, u64, bool, Rect)>,
    /// The tree revision the open overview shows.
    overview_revision: u64,
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
    /// Whether the chrome takes the keyboard, as of its last update or
    /// event.
    keyboard: Rc<Cell<bool>>,
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
        Self {
            bar,
            overview: OverviewPanel::new(),
            tilo_menu,
            workspace_menu: WorkspaceMenu::new(),
            navigator: StripNavigator::default(),
            navigator_frame: StripFrame::empty(Rect::ZERO),
            navigator_seen: None,
            overview_revision: 0,
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
            keyboard: Rc::default(),
        }
    }

    /// Set while keys should come to the chrome rather than the window in
    /// front ([`Self::wants_keyboard`]), for a host routing keys.
    #[must_use]
    pub fn keyboard(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.keyboard)
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

    /// Whether keys should come here rather than to the window in front:
    /// a menu, the overview or the strip switcher is up.
    #[must_use]
    pub fn wants_keyboard(&self) -> bool {
        self.menu_open() || self.overview.is_open() || self.navigator.takes_keys()
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
    #[must_use]
    pub fn with_asks(mut self, asks: Rc<RefCell<Vec<DesktopAsk>>>) -> Self {
        self.asks = asks;
        self
    }

    /// Take the intents a host's bar source raises from `intents`,
    /// shared with that source.
    #[must_use]
    pub fn with_intents(mut self, intents: Rc<RefCell<Vec<ChromeIntent>>>) -> Self {
        self.intents = intents;
        self
    }

    /// Close the menus the chrome opened: the host opens a popover of its
    /// own, one at a time.
    pub fn close_menus(&mut self) {
        if self.tilo_menu.is_menu_open() {
            self.tilo_menu.toggle(Rect::ZERO, self.rect);
        }
        self.workspace_menu.close();
    }

    fn dispatch(&self, command: &Command) {
        if let Err(error) = self.handle.dispatch(command, &self.generation) {
            tracing::warn!(%error, "desktop chrome");
        }
    }

    /// History of the active workspace, showing only what `author` did
    /// when one is named.
    pub fn activity_by(&self, author: Option<String>) {
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|w| w.id.0.to_string()));
        if let Some(workspace) = workspace {
            self.dispatch(&Command::WorkspaceActivity {
                generation: self.generation.clone(),
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
                    key: format!("{WORKSPACE_KEY}{}", workspace.id.0),
                    name: workspace.name.clone(),
                    current: workspace.id == tree.active,
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
                    self.dispatch(&Command::Workspace {
                        generation: self.generation.clone(),
                        workspace: workspace.to_owned(),
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

    fn cards(&self) -> Vec<crate::overview::RoomCard> {
        let host = PlainOverview {
            user: ui::collaboration::local_user_id().0,
        };
        let now = ui::time::unix_now();
        self.handle.read(|tree| room_cards(tree, &host, now))
    }

    fn apply_pick(&mut self, pick: &OverviewPick) {
        if pick.closes() {
            self.overview.set_open(false, Vec::new());
        }
        match pick {
            OverviewPick::Window(tab) => {
                self.handle.focus_tab(*tab);
            }
            OverviewPick::Workspace(id) => self.dispatch(&Command::Workspace {
                generation: self.generation.clone(),
                workspace: id.0.to_string(),
            }),
            OverviewPick::NewWorkspace => self.new_workspace(),
            // The overview here goes somewhere; it has no directory to
            // join from and no window or strip edits.
            OverviewPick::JoinWorkspace
            | OverviewPick::LeaveWorkspace(_)
            | OverviewPick::CloseWindow(_)
            | OverviewPick::MoveWindow { .. }
            | OverviewPick::MoveWindowToStrip { .. }
            | OverviewPick::RenameStrip { .. }
            | OverviewPick::MoveStrip { .. }
            | OverviewPick::Dismiss => {}
        }
    }

    /// Add an empty workspace after the others and go there; its launchpad
    /// offers what to open.
    fn new_workspace(&self) {
        if let Err(refusal) = self.rules.may_create_workspace() {
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
        self.navigator.needs_animation_frame(ui::time::now())
            || [
                &self.bar as &dyn Widget,
                &self.overview,
                &self.tilo_menu,
                &self.workspace_menu,
            ]
            .iter()
            .any(|child| child.needs_redraw())
    }
}

impl DesktopChrome {
    /// Carry out a chrome action; false for one of the desk's.
    fn perform(&mut self, action: &DesktopAction) -> bool {
        let intent = match action {
            DesktopAction::ToggleOverview => ChromeIntent::ToggleOverview,
            DesktopAction::ToggleQuickSettings => ChromeIntent::ToggleQuickSettings,
            DesktopAction::ToggleTiloMenu => ChromeIntent::ToggleTiloMenu,
            DesktopAction::ToggleWorkspaceMenu => ChromeIntent::ToggleWorkspaceMenu,
            DesktopAction::SwitchWorkspace { delta } => {
                ChromeIntent::SwitchWorkspace(i32::try_from(*delta).unwrap_or_default())
            }
            DesktopAction::ShowPerson { person } => ChromeIntent::ShowPerson(person.clone()),
            DesktopAction::StepStrip { steps } => {
                let steps = isize::try_from(*steps).unwrap_or_default();
                self.go_to_strip(|current| current.checked_add_signed(steps));
                return true;
            }
            DesktopAction::NewWorkspace => {
                self.new_workspace();
                return true;
            }
            DesktopAction::Overview { open } => {
                if self.overview_open() != open {
                    self.toggle_overview();
                }
                return true;
            }
            DesktopAction::Shortcut { ref action } => {
                let Ok(action) = serde_json::from_value::<ShortcutAction>(
                    serde_json::Value::String(action.clone()),
                ) else {
                    return false;
                };
                if !self.shortcut(action) {
                    self.asks.borrow_mut().push(DesktopAsk::Shortcut(action));
                }
                return true;
            }
            DesktopAction::FocusTab { .. }
            | DesktopAction::CloseTab { .. }
            | DesktopAction::OpenSearch { .. }
            | DesktopAction::SearchFor { .. }
            | DesktopAction::ChooseSearchRow { .. } => return false,
            | DesktopAction::OpenSearch
            | DesktopAction::Launchpad { .. }
            | DesktopAction::StepFocus { .. }
            | DesktopAction::FocusDisplay { .. }
            | DesktopAction::MoveToDisplay { .. } => return false,
        };
        self.apply_intent(intent);
        true
    }
}

impl ui::resources::PublishesResources for DesktopChrome {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![chrome_resource()]
    }
}

impl ui::host_operation::ResourceOperations for DesktopChrome {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        if let Some(request) = op.unscoped()
            && let Some(action) = DesktopAction::decode(request.type_id, request.payload)
            && self.perform(&action)
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
        self.observe_navigation();
        self.keyboard.set(self.wants_keyboard());
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
        let node = taffy.widget_node(
            column(),
            &[bar, content, overview, tilo_menu, workspace_menu],
            map,
            self,
        );
        fill(taffy, node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [bar, content, overview, tilo_menu, workspace_menu] =
            taffy.children(node).unwrap_or_default()[..]
        {
            self.bar.apply_layout(taffy, bar, inner);
            self.content = ui::widget::layout_rect(taffy, content, inner);
            self.overview.apply_layout(taffy, overview, inner);
            self.tilo_menu.apply_layout(taffy, tilo_menu, inner);
            self.workspace_menu
                .apply_layout(taffy, workspace_menu, inner);
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
        // Last, so what the bar opens below its band stays above the rest.
        self.bar.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let [bar, _, overview, tilo_menu, workspace_menu] =
            ctx.taffy.children(ctx.node_id).unwrap_or_default()[..]
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
        // The menus take what they cover, the keyboard and the press that
        // dismisses them.
        if self.workspace_menu.is_open() {
            self.workspace_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, workspace_menu));
            if *ctx.handled {
                return;
            }
        }
        if self.tilo_menu.is_open() {
            self.tilo_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, tilo_menu));
            if *ctx.handled {
                return;
            }
        }
        // The strip switcher's name field and keyboard choosing come before
        // the desktop's chords, as any field's typing does.
        if self.navigation_event(event, ctx.scale_factor, *ctx.modifiers) {
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
        self.keyboard.set(self.wants_keyboard());
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![
            &self.bar,
            &self.overview,
            &self.tilo_menu,
            &self.workspace_menu,
        ]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![
            &mut self.bar,
            &mut self.overview,
            &mut self.tilo_menu,
            &mut self.workspace_menu,
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
        self.navigator.needs_animation_frame(ui::time::now())
            || [
                &self.bar as &dyn Widget,
                &self.overview,
                &self.tilo_menu,
                &self.workspace_menu,
            ]
            .iter()
            .any(|child| child.needs_animation_frame())
    }

    /// The bar's own deadline turns its clock over on the minute.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [&self.bar as &dyn Widget, &self.overview, &self.tilo_menu]
            .iter()
            .filter_map(|child| child.next_wake_deadline())
            .chain(
                self.navigator
                    .next_wake_deadline(&self.navigator_frame, ui::time::now()),
            )
            .min()
    }

    fn name(&self) -> &'static str {
        "DesktopChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
