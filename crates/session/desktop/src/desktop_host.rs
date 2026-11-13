//! The desktop every host shows: the native session and the web page compose the same two layers
//! around the [`DesktopSurface`].
//! [`DeskLayer`] is what lies under the windows: the backdrop, the launchpad on a desk with no
//! windows yet, and the surface's strips and window bars. [`DesktopChrome`] is what lies above
//! them: the top bar, the strip ribbon and rail ([`StripNavigator`]), the overview panel, the Tilo
//! menu and the workspace menu, and the desktop's chords. A page draws the chrome over the desk in
//! one widget tree; the native session hosts the desk under its client windows and the chrome above
//! them.
//! Neither layer knows its host. What a person may change is asked of [`DesktopRules`]; what only
//! the host can do (inviting someone, a host action, the bar's people and profile popovers, quick
//! settings) comes back as [`DesktopAsk`]s.
//!
//! [`DesktopSurface`]: tilo_desktop_surface::DesktopSurface

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::layout::TileContent;
use tilo_desktop_model::{DesktopTree, TabId};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::DeskEffect;
use tilo_resource::Effect;
use ui::presence::PresenceCtx;
use ui::search::Scope;
use ui::widget::DrawChild;
use ui::widget::{AppModel, EventCtx, ModelPath, Stack, TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

mod cards;
mod start_cards;
mod testing;

use start_cards::{DisplayPad, EmptyWorkspace};

use crate::chrome::{ChromeIntent, TopBarPresence};
use crate::desk_verbs::{self, OpenSearch, desk_resource, launchpad_resource};
use crate::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use crate::overview::OverviewHost;
use crate::topbar::{BarSpace, Reveals};
use crate::window_switcher::WindowSource;

/// The next deadline a child widget reports, for the earliest of them.
fn wake_deadline(child: &dyn Widget) -> Option<ui::time::Instant> {
    child.next_wake_deadline()
}

pub use crate::host_contract::{
    BRANCH_WORKSPACE, DELETE_WORKSPACE, DesktopAsk, DesktopRules, JOIN_WORKSPACE, LEAVE_WORKSPACE,
    NEW_WORKSPACE, REPORT_PROBLEM, SESSION_LOCK, SESSION_LOG_OUT, SESSION_RESTART,
    SESSION_SHUT_DOWN, WORKSPACE_SETTINGS,
};

pub use testing::{NoticeTiles, PermissiveRules};

/// Whether the desk shown has no window. Every desk, not only a workspace's first: a new desk
/// beside others with windows is as blank as a new workspace, and the launchpad is the way in from
/// either.
#[must_use]
pub fn desk_is_empty(tree: &DesktopTree) -> bool {
    tree.active_workspace().is_some_and(|workspace| {
        tilo_desktop_surface::navigation::active_strip(tree, workspace)
            .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id))
            .is_none_or(|strip| strip.window_groups.is_empty())
    })
}

/// Queue `ask` for the host, and ask it for the pass that answers it. What the person chose on the
/// desk is carried out by the host on its next pass: a desk that drew the press and then went quiet
/// would answer it only whenever something else woke the host, so the window it opens comes long
/// after the press, and a person who presses again meanwhile gets a second one.
pub(crate) fn ask(handle: &DesktopHandle, asks: &Rc<RefCell<Vec<DesktopAsk>>>, asked: DesktopAsk) {
    asks.borrow_mut().push(asked);
    handle.wake();
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

/// What lies under the windows: the backdrop, and below the bar's room the strips of the desktop
/// surface over the empty desk's launchpad.
pub struct DeskLayer {
    backdrop: crate::backdrop::DesktopBackdrop,
    /// The room the bar takes above the strips: none under a fullscreen window, else the bar's
    /// height.
    band: BarSpace,
    area: Stack,
    handle: DesktopHandle,
    /// The workspace whose picture the backdrop shows.
    workspace: Cell<Option<u64>>,
    /// Where the pointer is, in the desk's own coordinates: a press carries no place, and the menu
    /// a secondary press opens stands at the pointer.
    cursor: Option<Point>,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// The verbs the empty desk offers now: the cards whose apps this host has, read every pass so
    /// an app installed or removed while the desk stands changes them.
    start_cards: Rc<dyn Fn() -> Vec<LaunchpadAction>>,
    /// The cards the windows' apps present over the desk, for a host with no
    /// compositor to give each one a window of its own.
    cards: cards::Cards,
    /// Whether this desk hosts those cards.
    hosts_cards: bool,
    rect: Rect,
}

/// A place on the screen as a chrome intent carries it: the logical pixel the person pressed,
/// rounded to the pixel they saw.
pub(crate) fn place(at: Point) -> (i32, i32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a place on the screen, in the logical pixels it was pressed in"
    )]
    let place = (at.x.round() as i32, at.y.round() as i32);
    place
}

/// Close the tab `tab` on this device's desk, wherever it stands.
pub(crate) fn close_tab(handle: &DesktopHandle, tab: TabId) {
    let mut tree = handle.read(Clone::clone);
    if tree.close_tab(tab).is_some() {
        handle.replace_tree(tree);
    }
}

impl DeskLayer {
    /// Take when the bar shows over the window covering its display from `reveals` , which the
    /// chrome reads too: the strip above the area takes the bar's room only while it is docked in
    /// the column.
    #[must_use]
    pub fn with_reveals(mut self, reveals: Rc<RefCell<Reveals>>) -> Self {
        self.band = BarSpace::new(Rc::new(move || reveals.borrow().presence(None, false)));
        self
    }

    /// The desk over `surface` (whose host side is `handle` ), with `launchpad` on a desk with no
    /// windows and `beneath` (a host's own layers) under both, putting what it asks into `asks` .
    /// `start_cards` names the verbs that desk offers, read each pass: the cards of the apps this
    /// host has ([`LaunchpadAction::ALL`] for a host that can open them all).
    #[must_use]
    pub fn new(
        surface: Box<dyn Widget>,
        handle: DesktopHandle,
        launchpad: CollaborationLaunchpad,
        start_cards: Rc<dyn Fn() -> Vec<LaunchpadAction>>,
        asks: Rc<RefCell<Vec<DesktopAsk>>>,
        beneath: Vec<Box<dyn Widget>>,
    ) -> Self {
        let workspace =
            handle.read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0));
        // The room the bar takes above the strips, at the same height it takes itself. The bar
        // docks in this desktop's column, so its room is there until a window covers its display; a
        // desk that follows the reveal (see [`Self::with_reveals`]) replaces this with whether the
        // bar shows, hidden or docked.
        let desk = handle.clone();
        let band = BarSpace::new(Rc::new(move || {
            if desk.read(|tree| tree.fullscreen_group().is_some()) {
                TopBarPresence::Hidden
            } else {
                TopBarPresence::Docked
            }
        }));
        let pads = handle.read(EmptyWorkspace::wants);
        let mut area = Stack::new();
        for layer in beneath {
            area = area.push(layer);
        }
        // The surface stays the area's top layer.
        let area = area
            .push(Box::new(EmptyWorkspace {
                pads: pads
                    .into_iter()
                    .map(|want| DisplayPad {
                        display: want.display,
                        place: want.place,
                        shown: want.shown,
                        launchpad: launchpad.for_another_display(),
                    })
                    .collect(),
                template: launchpad,
                cards: Rc::clone(&start_cards),
                rebuilt: false,
                handle: handle.clone(),
                asks: Rc::clone(&asks),
            }))
            .push(surface);
        Self {
            backdrop: crate::backdrop::DesktopBackdrop::new(),
            band,
            area,
            handle,
            workspace: Cell::new(workspace),
            cursor: None,
            asks,
            start_cards,
            cards: cards::Cards::default(),
            hosts_cards: false,
            rect: Rect::ZERO,
        }
    }

    /// Host the cards the windows' apps present over the desk: the desk draws
    /// each at the place its floating column gives it, and hands the app that
    /// draws it the input and the reading that follow from there. A host that
    /// gives a card a window of its own leaves this off.
    #[must_use]
    pub fn with_cards(mut self) -> Self {
        self.hosts_cards = true;
        self
    }

    /// Where a secondary press landed, when `event` is one and the pointer stands on the desk: the
    /// menu a press asks for stands there.
    fn secondary_press(&self, event: &WindowEvent) -> Option<(i32, i32)> {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
        else {
            return None;
        };
        self.cursor.filter(|at| self.rect.contains(*at)).map(place)
    }

    /// The area under the bar: the surface and what lies beneath it.
    #[must_use]
    pub const fn area(&self) -> &Stack {
        &self.area
    }

    /// Whether the surface takes `event` as a view-only scroll (panning its strip).
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

    /// Take the pictures the desk shows, looked up by the workspace in front.
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
            // With nothing typed the effect is the search key itself: it opens search, and a press
            // over the open search closes it, as the shell's own search key does. A query is typed
            // into the search already open instead.
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
                    ask(
                        &self.handle,
                        &self.asks,
                        DesktopAsk::HostAction(card.host_id()),
                    );
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
            DeskEffect::WaitWindow { tab } => {
                let Ok(tab) = tab.parse() else {
                    return false;
                };
                ask(&self.handle, &self.asks, DesktopAsk::WaitWindow(tab));
            }
            DeskEffect::ForceQuitApp { tab } => {
                let Ok(tab) = tab.parse() else {
                    return false;
                };
                ask(&self.handle, &self.asks, DesktopAsk::ForceQuitApp(tab));
            }
        }
        true
    }
}

impl ui::resources::PublishesResources for DeskLayer {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let windows: Vec<desk_verbs::Shown> = self.handle.read(|tree| {
            tree.tabs()
                .map(|tab| {
                    let title = tilo_desktop_model::apps::canonical_app_id(&tab.tile).map_or_else(
                        || tab.title.clone(),
                        |app| tilo_desktop_search::names::window_name(&app, &tab.title),
                    );
                    // A window the session does not run is read of through the desk, unless its app
                    // keeps what it shows to itself: the address is not offered, so no row names a
                    // picture nobody may make.
                    let foreign = match &tab.tile {
                        TileContent::Foreign { app_id, .. } if !self.handle.withholds(app_id) => {
                            Some(app_id.clone())
                        }
                        _ => None,
                    };
                    desk_verbs::Shown {
                        tab: tab.id,
                        title,
                        foreign,
                    }
                })
                .collect()
        });
        let (query, rows) = self.handle.search_rows();
        let search = self.handle.search_open().then_some(OpenSearch {
            query: query.as_str(),
            rows: rows.as_slice(),
            searching: self.handle.search_searching(),
        });
        let hung = self.handle.unresponsive_tabs();
        let mut resources = vec![desk_resource(
            &windows,
            &hung,
            &self.handle.displays(),
            search,
        )];
        if self.handle.read(desk_is_empty) {
            resources.push(launchpad_resource(&(self.start_cards)()));
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
        let changed = self.area.update(state, path);
        if !self.hosts_cards {
            return changed;
        }
        self.follow_cards() || changed
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
        self.backdrop
            .draw_child(scene, transform, text, theme, presence);
        ui::widget::draw_child(&mut self.area, scene, transform, text, theme, presence);
        if self.hosts_cards {
            self.draw_cards(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        } else if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.cursor = None;
        }
        // A card the desk stands over its windows takes the pointer where it
        // is drawn: the app that paints it is the one the press is for.
        if self.hosts_cards
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::Touch(_)
            )
            && let Some(at) = self.cursor
            && let Some(id) = self.card_at(at)
            && self.card_event(&id, event, ctx)
        {
            return;
        }
        if let [_, _, area] = ctx.taffy.children(ctx.node_id).unwrap_or_default()[..] {
            self.area
                .handle_event(event, &mut ctx.reborrow(ctx.path, area));
        }
        // A secondary press the desk takes — no window stands under the pointer, and nothing in the
        // desk beneath it claimed the press — is the menu of the place it pressed: the chrome's
        // own, offered where the person is looking.
        if !*ctx.handled
            && let Some(at) = self.secondary_press(event)
        {
            ask(
                &self.handle,
                &self.asks,
                DesktopAsk::Intent(ChromeIntent::ToggleWorkspaceMenu { at: Some(at) }),
            );
            *ctx.handled = true;
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
        if self.backdrop.needs_redraw() || self.backdrop.needs_animation_frame() {
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let cards = self.cards_accessibility();
        if cards.is_empty() {
            return None;
        }
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_hashed_key("desk-cards")
                .with_children(cards),
        )
    }

    /// A control a card over the desk publishes is the app's: the press goes
    /// to the app that draws the card.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        if !self.hosts_cards {
            return false;
        }
        let Some(surface) = self.area.children_mut().pop() else {
            return false;
        };
        let (state, path) = ui::doc::document(state, path);
        self.cards.press(surface, target, action, state, path)
    }

    fn name(&self) -> &'static str {
        "DeskLayer"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The overview's view of a host: no app artwork and no workspace settings, so shares are named by
/// their scope, and each window wears its app's mark as on every host.
pub(crate) struct PlainOverview {
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

mod chrome_layer;
pub use chrome_layer::DesktopChrome;
