//! The workspace overview as a widget, the one every desktop shows: the
//! native shell hosts it as the overview app, the browser desktop draws it
//! over its desk.
//!
//! A panel names the workspace you are in (who has access, who is here,
//! what is shared out of it) and offers every other workspace, a new one
//! and joining one. Beside it on a wide display, inside it on a narrow
//! one, a picture shows each strip of the workspace you are in under its
//! name, its windows left to right at their share of the display
//! ([`crate::overview_picture`]). Keys walk the rooms and the windows; a
//! window is pressed to go there, middle-clicked to close it, carried onto
//! another workspace or strip to move it there; a strip is renamed in place
//! and moved up or down.
//!
//! The panel decides nothing. Everything chosen is queued for the host as
//! an [`OverviewPick`] ([`OverviewPanel::take_pick`]), and the host carries
//! it out through its own navigation. The windows' pictures are the host's
//! to draw where it holds their buffers: the panel names each pictured
//! window and where it rests through the [`WindowFlight`] the host gives it,
//! and moves its own drawing in step with the flight's clock.

mod input;
mod layout;
mod paint;

use std::collections::{HashMap, HashSet, VecDeque};

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::{GroupId, StripId, TabId, WorkspaceId};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::widget::{TaffyTreeExt as _, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use winit::event::{ElementState, Ime, WindowEvent};

use crate::i18n::{t, t_args};
use crate::overview::{OverviewPress, RoomCard};
use crate::overview_picture::{GridMetrics, StripPlace};
use crate::window_flight::WindowFlight;

use layout::{Metrics, StripControl};
pub use layout::{PANEL_RISE_PX, PANEL_START_SCALE, panel_curve};

/// What the overview shows: every workspace, and what only the host knows
/// about them.
#[derive(Clone, Debug, Default)]
pub struct OverviewDesk {
    /// Every workspace, in order; the one you are in is marked current.
    pub cards: Vec<RoomCard>,
    /// Each window group's size on its display; a group not given one is
    /// drawn at its share of the display.
    pub group_sizes: HashMap<GroupId, Size>,
    /// The strips of the workspace you are in that this machine names and
    /// moves.
    pub renamable: HashSet<StripId>,
    /// Each person's colour, by id: their cursor's.
    pub colors: HashMap<String, (u8, u8, u8)>,
    /// What a workspace held through a site says in place of who has
    /// access: the site, and how many are there.
    pub sites: HashMap<WorkspaceId, String>,
    /// The workspaces that can be left from here.
    pub leavable: HashSet<WorkspaceId>,
    /// The windows the host draws the pictures of itself: the panel leaves
    /// their wells to it.
    pub pictured: HashSet<TabId>,
    /// A new workspace can be made.
    pub can_add: bool,
    /// A workspace can be joined.
    pub can_join: bool,
    /// The picture of the workspace you are in is left out (the host shows
    /// its windows elsewhere for now).
    pub hide_windows: bool,
}

impl OverviewDesk {
    /// `cards`, with nothing else known: a new workspace can be made.
    pub fn of(cards: Vec<RoomCard>) -> Self {
        Self {
            cards,
            can_add: true,
            ..Self::default()
        }
    }
}

/// What the person chose in the overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverviewPick {
    /// Go to this window of the workspace you are in.
    Window(TabId),
    /// Go to another workspace.
    Workspace(WorkspaceId),
    NewWorkspace,
    /// Open the directory a workspace is joined from.
    JoinWorkspace,
    /// Leave this site's workspace.
    LeaveWorkspace(WorkspaceId),
    /// Close this window; the overview stays up.
    CloseWindow(TabId),
    /// Move this window to another workspace; the overview stays up to
    /// show it leave.
    MoveWindow {
        tab: TabId,
        to: WorkspaceId,
    },
    /// Move this window's group to another strip of the workspace.
    MoveWindowToStrip {
        tab: TabId,
        strip: StripId,
    },
    RenameStrip {
        strip: StripId,
        name: String,
    },
    /// Move a strip to stand before `before` (the bottom when `None`).
    MoveStrip {
        strip: StripId,
        before: Option<StripId>,
    },
    /// Leave the overview where it was.
    Dismiss,
}

impl OverviewPick {
    /// Whether the overview goes away once the host carried it out.
    pub fn closes(&self) -> bool {
        matches!(
            self,
            Self::Window(_)
                | Self::Workspace(_)
                | Self::NewWorkspace
                | Self::JoinWorkspace
                | Self::Dismiss
        )
    }
}

/// A strip's name being typed in the picture.
pub(crate) struct Naming {
    strip: StripId,
    text: String,
    selection: Selection,
    input: Option<TextInput>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts each read on its own: whether it is open, the motion setting, an interrupted keyboard selection, whether workspaces may be made, and whether it needs drawing"
)]
pub struct OverviewPanel {
    open: bool,
    /// The area the overview stands in: the display it covers.
    rect: Rect,
    /// The area last laid out for.
    laid_out: Rect,
    desk: OverviewDesk,
    flight: Option<WindowFlight>,
    metrics: Metrics,
    grid: GridMetrics,
    reduce_motion: bool,
    /// The list's scroll.
    scroll_y: f64,
    /// The keyboard's room, by its place among the cards; past them, New
    /// workspace and then Join.
    selection: Option<usize>,
    /// The pictured window the keyboard is on.
    window_selection: Option<TabId>,
    /// The selected window went away under the keyboard: the next Enter
    /// does nothing.
    window_interrupted: bool,
    /// A press on the overview until it is let go.
    press: Option<OverviewPress>,
    naming: Option<Naming>,
    cursor: Option<Point>,
    picks: VecDeque<OverviewPick>,
    /// The host may make workspaces at all.
    offer_add: bool,
    /// The panel as last recorded, and the revision it shows.
    cache: Option<(u64, Scene)>,
    cache_fills: u32,
    revision: u64,
    /// The flight's clock as last drawn.
    drawn_progress: f64,
    dirty: bool,
}

impl Default for OverviewPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl OverviewPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            rect: Rect::ZERO,
            laid_out: Rect::ZERO,
            desk: OverviewDesk::default(),
            flight: None,
            metrics: Metrics::default(),
            grid: GridMetrics::for_theme(&Theme::default()),
            reduce_motion: false,
            scroll_y: 0.0,
            selection: None,
            window_selection: None,
            window_interrupted: false,
            press: None,
            naming: None,
            cursor: None,
            picks: VecDeque::new(),
            offer_add: true,
            cache: None,
            cache_fills: 0,
            revision: 0,
            drawn_progress: 0.0,
            dirty: true,
        }
    }

    /// Offer "New workspace" only when the host can make one.
    #[must_use]
    pub fn with_new_workspace(mut self, offered: bool) -> Self {
        self.offer_add = offered;
        self
    }

    /// Offer a new workspace or not, as the host's policy answers now.
    pub fn set_new_workspace(&mut self, offered: bool) {
        if self.offer_add != offered {
            self.offer_add = offered;
            self.bump();
        }
    }

    /// Pictures of the windows are drawn by the host through `flight`, and
    /// the overview arrives and leaves on its clock.
    pub fn set_flight(&mut self, flight: WindowFlight) {
        self.flight = Some(flight);
        self.bump();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open over `cards` (every workspace, as
    /// [`crate::overview::room_cards`] returns them), or close.
    pub fn set_open(&mut self, open: bool, cards: Vec<RoomCard>) {
        self.open_on(open, OverviewDesk::of(cards));
    }

    /// Open over `desk`, or close. Each opening starts the keyboard on the
    /// room you are in.
    pub fn open_on(&mut self, open: bool, desk: OverviewDesk) {
        if open {
            self.desk = desk;
        }
        if open != self.open {
            self.open = open;
            self.scroll_y = 0.0;
            self.naming = None;
            self.press = None;
            self.window_selection = None;
            self.window_interrupted = false;
            self.selection = if open { self.here_index() } else { None };
        }
        self.bump();
    }

    /// Show `cards` while open, keeping the keyboard where it is.
    pub fn show(&mut self, cards: Vec<RoomCard>) {
        let desk = OverviewDesk {
            cards,
            ..std::mem::take(&mut self.desk)
        };
        self.set_desk(desk);
    }

    /// Show `desk`, keeping the keyboard where it is.
    pub fn set_desk(&mut self, desk: OverviewDesk) {
        self.desk = desk;
        if self
            .selection
            .is_some_and(|index| index > self.max_selection())
        {
            self.selection = self.here_index();
        }
        // A strip renamed or gone elsewhere ends its naming here.
        if let Some(naming) = &self.naming
            && !self
                .here_card()
                .is_some_and(|card| card.strips.iter().any(|strip| strip.id == naming.strip))
        {
            self.naming = None;
        }
        self.reconcile_window_selection();
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y());
        self.bump();
    }

    pub fn desk(&self) -> &OverviewDesk {
        &self.desk
    }

    /// What the person chose since the host last asked, oldest first.
    pub fn take_pick(&mut self) -> Option<OverviewPick> {
        self.picks.pop_front()
    }

    /// The room the keyboard is on, by its place among the cards.
    pub fn selection(&self) -> Option<usize> {
        self.selection
    }

    /// The pictured window the keyboard is on.
    pub fn window_selection(&self) -> Option<TabId> {
        self.window_selection
    }

    /// How many times the panel was recorded, for tests of its cost.
    pub fn panel_recordings(&self) -> u32 {
        self.cache_fills
    }

    /// Whether a strip's name is being typed.
    pub fn is_naming(&self) -> bool {
        self.naming.is_some()
    }

    /// Choose `pick`, as the person did.
    pub fn choose(&mut self, pick: OverviewPick) {
        self.picks.push_back(pick);
        self.bump();
    }

    fn can_add(&self) -> bool {
        self.offer_add && self.desk.can_add
    }

    fn max_selection(&self) -> usize {
        self.desk
            .cards
            .len()
            .saturating_add(usize::from(self.can_add()))
            .saturating_add(usize::from(self.desk.can_join))
            .saturating_sub(1)
    }

    /// Something shown changed: the panel is recorded again and redrawn.
    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.dirty = true;
    }

    /// How far the overview has arrived: its flight's clock, or all the
    /// way while it is open with no flight.
    fn progress(&self) -> f64 {
        match &self.flight {
            Some(flight) => flight.progress(),
            None if self.open => 1.0,
            None => 0.0,
        }
    }

    /// Something of the overview is on screen.
    pub fn is_drawn(&self) -> bool {
        self.open || self.progress() > 0.0
    }

    /// Where room `index`'s row is (the "you are here" block for the room
    /// you are in), for a host's tests and scripts.
    pub fn room_rect(&self, index: usize) -> Option<Rect> {
        (self.open && index < self.desk.cards.len()).then(|| self.row_rect(index))
    }

    /// The pointer moved to `p`; whether what it is over changed.
    fn hover(&mut self, p: Point) {
        let before = self.hover_key();
        self.cursor = Some(p);
        if before != self.hover_key() {
            self.bump();
        }
    }

    /// What the pointer is over, as far as drawing tells.
    fn hover_key(&self) -> (Option<usize>, Option<TabId>, Option<usize>, u8) {
        let Some(p) = self.cursor else {
            return (None, None, None, 0);
        };
        let picture = self.picture();
        let strip = picture
            .places
            .iter()
            .position(|place| place.bounds().contains(p));
        let control = strip.map_or(0, |si| {
            self.strip_controls(&picture, si)
                .iter()
                .position(|(_, rect)| rect.contains(p))
                .map_or(0, |at| {
                    u8::try_from(at.saturating_add(1)).unwrap_or(u8::MAX)
                })
        });
        let rows = u8::from(self.add_hit_rect().is_some_and(|r| r.contains(p)))
            | (u8::from(self.join_hit_rect().is_some_and(|r| r.contains(p))) << 1)
            | (u8::from(
                (0..self.desk.cards.len())
                    .any(|idx| self.leave_rect(idx).is_some_and(|r| r.contains(p))),
            ) << 2)
            | (control << 3);
        (self.hit_room(p), self.window_at(p), strip, rows)
    }

    // ---- What assistive technology reads ----

    fn strips_accessibility(&self, picture: &layout::Picture) -> Option<AccessNode> {
        if picture.strips.is_empty() {
            return None;
        }
        let total = picture.strips.len();
        let items = picture
            .strips
            .iter()
            .zip(&picture.places)
            .enumerate()
            .map(|(index, (strip, place))| {
                let mut node = AccessNode::new(Role::ListItem)
                    .with_action_target(format!("overview-strip:{}", strip.id.raw()))
                    .with_name(strip.name.clone())
                    .with_position(index.saturating_add(1), total)
                    .with_bounds(place.label)
                    .with_state(AccessState::new().selected(strip.focused));
                node.children = self
                    .strip_controls(picture, index)
                    .into_iter()
                    .map(|(control, rect)| {
                        AccessNode::new(Role::Button)
                            .with_action_target(format!(
                                "overview-strip-{}:{}",
                                control.key(),
                                strip.id.raw()
                            ))
                            .with_name(control.name())
                            .with_bounds(rect)
                    })
                    .collect();
                node
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name(t("overview-strips-title"))
                .with_bounds(
                    picture
                        .places
                        .iter()
                        .map(StripPlace::bounds)
                        .reduce(|all, rect| all.union(rect))
                        .unwrap_or(Rect::ZERO),
                )
                .with_children(items),
        )
    }

    /// Act on semantic `target`; whether it named something here.
    fn activate(&mut self, target: &str) -> bool {
        let number = |prefix: &str| {
            target
                .strip_prefix(prefix)
                .and_then(|n| n.parse::<u64>().ok())
        };
        if target == "overview-new-workspace" && self.can_add() {
            self.choose(OverviewPick::NewWorkspace);
        } else if target == "overview-join-workspace" && self.desk.can_join {
            self.choose(OverviewPick::JoinWorkspace);
        } else if let Some(id) = number("overview-room:") {
            self.choose(OverviewPick::Workspace(WorkspaceId(id)));
        } else if let Some(id) = number("overview-leave:") {
            self.choose(OverviewPick::LeaveWorkspace(WorkspaceId(id)));
        } else if let Some(tab) = number("overview-window:") {
            self.choose(OverviewPick::Window(tab));
        } else if let Some(id) = number("overview-strip:") {
            let picture = self.picture();
            let Some(si) = picture.strips.iter().position(|s| s.id.raw() == id) else {
                return false;
            };
            self.begin_naming(&picture, si);
        } else {
            let picture = self.picture();
            let control = [
                StripControl::Rename,
                StripControl::MoveUp,
                StripControl::MoveDown,
            ]
            .into_iter()
            .find_map(|control| {
                number(&format!("overview-strip-{}:", control.key())).map(|id| (control, id))
            });
            let Some((control, id)) = control else {
                return false;
            };
            let Some(si) = picture.strips.iter().position(|s| s.id.raw() == id) else {
                return false;
            };
            self.apply_strip_control(&picture, si, control);
        }
        true
    }

    /// Put the keyboard on semantic `target`; whether it named something
    /// here.
    fn focus(&mut self, target: &str) -> bool {
        let number = |prefix: &str| {
            target
                .strip_prefix(prefix)
                .and_then(|n| n.parse::<u64>().ok())
        };
        let count = self.desk.cards.len();
        if let Some(tab) = number("overview-window:") {
            if !self.row_windows().iter().any(|w| w.address.tab == tab) {
                return false;
            }
            self.select_window(tab);
        } else if let Some(index) =
            number("overview-room:").and_then(|id| self.card_index(WorkspaceId(id)))
        {
            self.focus_row(index);
        } else if target == "overview-new-workspace" && self.can_add() {
            self.focus_row(count);
        } else if target == "overview-join-workspace" && self.desk.can_join {
            self.focus_row(count.saturating_add(usize::from(self.can_add())));
        } else {
            return false;
        }
        true
    }
}

impl Widget for OverviewPanel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolve_layout(theme);
        let style = Style {
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.bump();
        }
    }

    /// The overview fills its node and lays out its rows itself: there is
    /// never a size for the layout solver to measure.
    fn layout_needs_measure(&self) -> bool {
        false
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let progress = self.progress();
        self.drawn_progress = progress;
        if !self.is_drawn() {
            if let Some(flight) = &self.flight {
                flight.clear();
            }
            return;
        }
        self.resolve_layout(theme);
        self.reconcile_window_selection();
        let p = ui::num::f32_of_f64(progress);
        ui::overlay::scrim::whole_view(scene, transform, self.rect, p, theme);
        self.paint_panel_at(scene, transform, text, theme, panel_curve(p));
        let pictures = self.paint_picture(scene, transform, text, theme, progress);
        if let Some(flight) = &self.flight {
            let clip = (!pictures.is_empty()).then(|| self.grid_clip());
            flight.show(pictures, clip);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.open || *ctx.handled {
            return;
        }
        let taken = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.hover(p);
                // Others still see the pointer move, so their hover ends;
                // a press or the name field may still take it.
                self.pointer(event, ctx.scale_factor)
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                self.key(&key.logical_key, ctx.modifiers.state())
            }
            WindowEvent::KeyboardInput { .. } => self.naming.is_some(),
            WindowEvent::Ime(Ime::Commit(text)) => self.naming_commit_text(text),
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_) => self.pointer(event, ctx.scale_factor),
            _ => false,
        };
        if taken {
            *ctx.handled = true;
        }
    }

    /// Changed, or the flight's clock moved since the last frame.
    fn needs_redraw(&self) -> bool {
        self.dirty
            || self
                .flight
                .as_ref()
                .is_some_and(|flight| flight.progress().to_bits() != self.drawn_progress.to_bits())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.open {
            return None;
        }
        let pressable = self.window_at(point).is_some()
            || self.hit_room(point).is_some()
            || self.add_hit_rect().is_some_and(|r| r.contains(point))
            || self.join_hit_rect().is_some_and(|r| r.contains(point));
        pressable.then_some(winit::window::CursorIcon::Pointer)
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.open.then_some(self.rect)
    }

    fn traps_focus(&self) -> bool {
        self.open
    }

    fn wants_pointer_capture(&self) -> bool {
        self.open
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let count = self.desk.cards.len();
        let keyboard_on =
            |index: usize| self.selection == Some(index) && self.window_selection.is_none();
        let mut overview = AccessNode::new(Role::Dialog)
            .with_name(t("overview-title"))
            .with_bounds(self.bounds())
            .with_state(AccessState::new().modal(true));
        overview
            .children
            .extend(self.desk.cards.iter().enumerate().map(|(index, card)| {
                AccessNode::new(Role::Button)
                    .with_action_target(format!("overview-room:{}", card.id.raw()))
                    .with_name(card.name.clone())
                    .with_bounds(self.row_rect(index))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(keyboard_on(index)),
                    )
            }));
        let picture = self.picture();
        let windows: Vec<_> = picture.windows().collect();
        if !windows.is_empty() {
            let total = windows.len();
            let items = windows
                .iter()
                .enumerate()
                .map(|(index, (si, wi, window))| {
                    let selected = self.window_selection == Some(window.address.tab);
                    AccessNode::new(Role::ListItem)
                        .with_action_target(format!("overview-window:{}", window.address.tab))
                        .with_name(window.title.clone())
                        .with_position(index.saturating_add(1), total)
                        .with_bounds(picture.tile(*si, *wi).unwrap_or(Rect::ZERO))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(selected)
                                .selected(selected),
                        )
                })
                .collect();
            overview.children.push(
                AccessNode::new(Role::List)
                    .with_bounds(self.window_list_rect().unwrap_or(Rect::ZERO))
                    .with_name(t("overview-windows-title"))
                    .with_description(t("overview-windows-description"))
                    .with_children(items),
            );
        }
        if let Some(rect) = self.add_rect() {
            overview.children.push(
                AccessNode::new(Role::Button)
                    .with_action_target("overview-new-workspace")
                    .with_name(t("overview-new-workspace"))
                    .with_bounds(rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(keyboard_on(count)),
                    ),
            );
        }
        if let Some(rect) = self.join_rect() {
            let at = count.saturating_add(usize::from(self.can_add()));
            overview.children.push(
                AccessNode::new(Role::Button)
                    .with_action_target("overview-join-workspace")
                    .with_name(t("overview-join-workspace"))
                    .with_bounds(rect)
                    .with_state(AccessState::new().focusable(true).focused(keyboard_on(at))),
            );
        }
        for (idx, card) in self.desk.cards.iter().enumerate() {
            if let Some(leave) = self.leave_rect(idx) {
                overview.children.push(
                    AccessNode::new(Role::Button)
                        .with_action_target(format!("overview-leave:{}", card.id.raw()))
                        .with_name(t_args(
                            "overview-a11y-leave",
                            &[("name", card.name.clone().into())],
                        ))
                        .with_bounds(leave),
                );
            }
        }
        if let Some(strips) = self.strips_accessibility(&picture) {
            overview.children.push(strips);
        }
        Some(overview)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        if !self.open {
            return false;
        }
        match action {
            ui::a11y::Action::Activate => self.activate(target),
            ui::a11y::Action::Focus => self.focus(target),
            _ => false,
        }
    }

    /// Every target a script presses, by name: each room's row in order
    /// (`overview_card[i]`), the rows that make and join a workspace, each
    /// pictured window, each strip's name.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        if !self.open {
            return Vec::new();
        }
        let mut rects = vec![("overview".to_owned(), self.panel_rect())];
        rects.extend(
            (0..self.desk.cards.len()).map(|i| (format!("overview_card[{i}]"), self.row_rect(i))),
        );
        if let Some(add) = self.add_hit_rect() {
            rects.push(("add_workspace".to_owned(), add));
        }
        if let Some(join) = self.join_hit_rect() {
            rects.push(("join_workspace".to_owned(), join));
        }
        for (idx, card) in self.desk.cards.iter().enumerate() {
            if let Some(leave) = self.leave_rect(idx) {
                rects.push((format!("overview-leave:{}", card.id.raw()), leave));
            }
        }
        let picture = self.picture();
        for (si, wi, window) in picture.windows() {
            if let Some(tile) = picture.tile(si, wi) {
                rects.push((format!("overview-window:{}", window.address.tab), tile));
            }
        }
        for (si, (strip, place)) in picture.strips.iter().zip(&picture.places).enumerate() {
            rects.push((format!("overview-strip:{}", strip.id.raw()), place.label));
            for (control, rect) in self.strip_controls(&picture, si) {
                rects.push((
                    format!("overview-strip-{}:{}", control.key(), strip.id.raw()),
                    rect,
                ));
            }
        }
        rects
    }

    fn name(&self) -> &'static str {
        "OverviewPanel"
    }

    fn get_rect(&self) -> Rect {
        if self.open { self.rect } else { Rect::ZERO }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    reason = "test fixtures build small ids and unwrap what they built"
)]
mod tests {
    use super::*;
    use crate::overview::{OverviewGroup, OverviewStrip, OverviewTab, RoomCard, RoomShare};
    use crate::window_switcher::WindowApp;
    use ui::widget::AppKind;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn tab(id: u64, title: &str, active: bool) -> OverviewTab {
        OverviewTab {
            id,
            title: title.into(),
            is_active: active,
            app: WindowApp {
                key: format!("app:{title}"),
                kind: AppKind::Editor,
                icon: None,
                glyph: ui::icons::Icon::AppWindow,
            },
            people: Vec::new(),
        }
    }

    fn card(id: u64, name: &str, current: bool, windows: &[&str]) -> RoomCard {
        RoomCard {
            id: WorkspaceId(id),
            name: name.into(),
            is_current: current,
            people: Vec::new(),
            shares: Vec::new(),
            strips: vec![OverviewStrip {
                id: StripId(id * 10),
                name: "Main".into(),
                is_current: current,
                groups: windows
                    .iter()
                    .enumerate()
                    .map(|(i, title)| OverviewGroup {
                        id: id * 100 + i as u64,
                        width_permille: 500,
                        is_current: i == 0,
                        tabs: vec![tab(id * 1000 + i as u64, title, true)],
                    })
                    .collect(),
            }],
        }
    }

    const WIDE: Rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    const NARROW: Rect = Rect::new(0.0, 0.0, 900.0, 800.0);

    fn opened_in(area: Rect, desk: OverviewDesk) -> OverviewPanel {
        let mut panel = OverviewPanel::new();
        panel.open_on(true, desk);
        panel.rect = area;
        panel.resolve_layout(&Theme::dark());
        panel
    }

    fn desk() -> OverviewDesk {
        let mut desk = OverviewDesk::of(vec![
            card(1, "Acme", true, &["Notes", "Writer"]),
            card(2, "Research", false, &[]),
            card(3, "Design", false, &["Canvas"]),
        ]);
        desk.can_join = true;
        desk
    }

    fn press(panel: &mut OverviewPanel, key: NamedKey) -> bool {
        panel.key(&Key::Named(key), ModifiersState::empty())
    }

    #[test]
    fn the_room_you_are_in_heads_the_panel_and_the_others_follow_in_order() {
        let panel = opened_in(NARROW, desk());
        let here = panel.row_rect(0);
        let research = panel.row_rect(1);
        let design = panel.row_rect(2);
        assert!(here.y1 <= research.y0, "{here:?} {research:?}");
        assert!(research.y1 <= design.y0, "rows advance downward");
        let add = panel.add_hit_rect().unwrap();
        let join = panel.join_hit_rect().unwrap();
        assert!(design.y1 <= add.y0 && add.y1 <= join.y0);
        assert!(panel.panel_rect().contains(add.center()));
        assert_eq!(panel.hit_room(research.center()), Some(1));
        assert_eq!(
            panel.hit_room(Point::new(1.0, 1.0)),
            None,
            "outside the panel"
        );
    }

    #[test]
    fn a_wide_display_puts_the_picture_beside_the_panel_and_a_narrow_one_inside() {
        let wide = opened_in(WIDE, desk());
        assert!(wide.beside_picture());
        let picture = wide.picture();
        let notes = picture.tile(0, 0).unwrap();
        assert!(
            notes.x1 <= wide.panel_rect().x0,
            "the picture stands beside"
        );
        let narrow = opened_in(NARROW, desk());
        assert!(!narrow.beside_picture());
        let notes = narrow.picture().tile(0, 0).unwrap();
        assert!(
            narrow.here_rect().contains(notes.center()),
            "the panel holds it"
        );
    }

    #[test]
    fn a_small_display_shows_the_overview_as_a_page() {
        let panel = opened_in(Rect::new(0.0, 0.0, 480.0, 400.0), desk());
        assert_eq!(panel.panel_rect(), panel.rect);
        assert!(panel.scrolls_whole_panel());
        assert!(panel.add_hit_rect().is_some() || panel.max_scroll_y() > 0.0);
    }

    #[test]
    fn a_long_list_is_capped_and_scrolls_to_the_keyboard() {
        let mut cards = vec![card(1, "Acme", true, &["Notes"])];
        cards.extend((2..30).map(|i| card(i, &format!("Room {i}"), false, &[])));
        let mut panel = opened_in(NARROW, OverviewDesk::of(cards));
        assert!(panel.panel_rect().height() <= NARROW.height());
        assert!(panel.max_scroll_y() > 0.0);
        assert!(press(&mut panel, NamedKey::End));
        assert!(panel.scroll_y > 0.0, "the last row was scrolled to");
        assert!(panel.add_hit_rect().is_some(), "and can be pressed");
    }

    #[test]
    fn keys_walk_the_rooms_and_enter_goes_to_one() {
        let mut panel = opened_in(NARROW, desk());
        assert_eq!(panel.selection(), Some(0), "the keyboard starts here");
        assert!(press(&mut panel, NamedKey::ArrowDown));
        assert!(press(&mut panel, NamedKey::ArrowDown));
        assert!(press(&mut panel, NamedKey::Enter));
        assert_eq!(
            panel.take_pick(),
            Some(OverviewPick::Workspace(WorkspaceId(3)))
        );
        assert!(press(&mut panel, NamedKey::Tab));
        assert!(press(&mut panel, NamedKey::Enter));
        assert_eq!(panel.take_pick(), Some(OverviewPick::NewWorkspace));
        assert!(press(&mut panel, NamedKey::Tab));
        assert!(press(&mut panel, NamedKey::Enter));
        assert_eq!(panel.take_pick(), Some(OverviewPick::JoinWorkspace));
        assert!(press(&mut panel, NamedKey::Escape));
        assert_eq!(panel.take_pick(), Some(OverviewPick::Dismiss));
        assert_eq!(panel.take_pick(), None);
    }

    #[test]
    fn arrows_walk_the_pictured_windows_and_enter_goes_to_one() {
        let mut panel = opened_in(WIDE, desk());
        assert!(press(&mut panel, NamedKey::ArrowRight));
        assert_eq!(panel.window_selection(), Some(1000));
        assert!(press(&mut panel, NamedKey::ArrowRight));
        assert_eq!(panel.window_selection(), Some(1001));
        assert!(press(&mut panel, NamedKey::ArrowRight));
        assert_eq!(panel.window_selection(), Some(1000), "a ring");
        assert!(press(&mut panel, NamedKey::Enter));
        assert_eq!(panel.take_pick(), Some(OverviewPick::Window(1000)));
    }

    #[test]
    fn a_window_selected_then_closed_elsewhere_does_not_take_the_next_enter() {
        let mut panel = opened_in(WIDE, desk());
        assert!(press(&mut panel, NamedKey::ArrowRight));
        let mut gone = desk();
        gone.cards[0].strips[0].groups.remove(0);
        panel.set_desk(gone);
        assert_eq!(panel.window_selection(), None);
        assert!(press(&mut panel, NamedKey::Enter));
        assert_eq!(
            panel.take_pick(),
            None,
            "the Enter meant for it does nothing"
        );
    }

    #[test]
    fn a_strip_this_machine_names_is_renamed_and_moved_in_place() {
        let mut here = card(1, "Acme", true, &["Notes"]);
        let mut second = card(4, "x", false, &["Chat"]).strips.remove(0);
        second.name = "Games".into();
        here.strips.push(second);
        let mut desk = OverviewDesk::of(vec![here]);
        desk.renamable = [StripId(10), StripId(40)].into_iter().collect();
        let mut panel = opened_in(WIDE, desk);
        assert!(press(&mut panel, NamedKey::ArrowRight));
        assert!(press(&mut panel, NamedKey::F2));
        assert!(panel.is_naming());
        for c in ["W", "o", "r", "k"] {
            panel.key(&Key::Character(c.into()), ModifiersState::empty());
        }
        assert!(press(&mut panel, NamedKey::Enter));
        assert_eq!(
            panel.take_pick(),
            Some(OverviewPick::RenameStrip {
                strip: StripId(10),
                name: "Work".into()
            })
        );
        panel.key(&Key::Named(NamedKey::ArrowDown), ModifiersState::ALT);
        assert_eq!(
            panel.take_pick(),
            Some(OverviewPick::MoveStrip {
                strip: StripId(10),
                before: None
            })
        );
        let names: Vec<String> = panel
            .test_rects()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("overview-strip-"))
            .collect();
        assert!(
            names.contains(&"overview-strip-rename:10".to_owned()),
            "{names:?}"
        );
        assert!(
            names.contains(&"overview-strip-move-up:40".to_owned()),
            "{names:?}"
        );
    }

    #[test]
    fn a_strip_named_elsewhere_has_no_controls_and_no_field() {
        let mut panel = opened_in(WIDE, desk());
        assert!(press(&mut panel, NamedKey::ArrowRight));
        assert!(press(&mut panel, NamedKey::F2));
        assert!(!panel.is_naming());
        assert!(panel.strip_controls(&panel.picture(), 0).is_empty());
    }

    #[test]
    fn the_host_draws_the_pictures_it_holds_and_flies_them_on_its_clock() {
        let flight = WindowFlight::new();
        let mut desk = desk();
        desk.pictured = [1000, 1001].into_iter().collect();
        let mut panel = opened_in(WIDE, desk);
        panel.set_flight(flight.clone());
        flight.set_clock(1.0, HashMap::new());
        let mut scene = Scene::new();
        panel.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        let pictures = flight.pictures();
        assert_eq!(
            pictures.iter().map(|p| p.tab).collect::<Vec<_>>(),
            [1000, 1001],
            "every pictured window is named"
        );
        let picture = panel.picture();
        assert_eq!(pictures[0].rect, picture.places[0].wells[0]);
        // A title flies with its picture from where the window stands.
        let source = Rect::new(0.0, 0.0, 800.0, 900.0);
        flight.set_clock(0.25, HashMap::from([(1000, source)]));
        let (rect, alpha) = panel.flown(&pictures[0], 0.25);
        assert_eq!(
            rect,
            ui::motion::Zoom::rect_at(source, pictures[0].rect, 0.25)
        );
        assert!((alpha - 1.0).abs() < f32::EPSILON);
        // One with nowhere to fly from fades in where it rests.
        let (rect, alpha) = panel.flown(&pictures[1], 0.25);
        assert_eq!(rect, pictures[1].rect);
        assert!(alpha < 1.0);
        // Closed and settled back, nothing is pictured.
        panel.open_on(false, OverviewDesk::default());
        flight.set_clock(0.0, HashMap::new());
        panel.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(flight.pictures().is_empty());
    }

    #[test]
    fn the_panel_is_recorded_once_while_it_shows_the_same_thing() {
        let flight = WindowFlight::new();
        let mut panel = opened_in(WIDE, desk());
        panel.set_flight(flight.clone());
        let mut text = TextRenderer::new();
        for step in 1..=4 {
            flight.set_clock(f64::from(step) / 4.0, HashMap::new());
            panel.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                &PresenceCtx::default(),
            );
        }
        assert_eq!(
            panel.panel_recordings(),
            1,
            "the flight replays the recording"
        );
    }

    #[test]
    fn the_share_list_collapses_rather_than_growing() {
        let mut desk = desk();
        desk.cards[0].shares = (0..9)
            .map(|i| RoomShare {
                what: format!("Folder {i}"),
                with: vec!["Ada".into()],
                writable: false,
            })
            .collect();
        let panel = opened_in(NARROW, desk);
        assert_eq!(
            OverviewPanel::share_rows(&panel.desk.cards[0]),
            layout::SHARES_MAX_ROWS + 1
        );
        let none = opened_in(NARROW, super::tests::desk());
        assert_eq!(none.shares_height(), 0.0, "nothing shared, no section");
    }

    #[test]
    fn a_carried_window_moves_to_the_room_it_is_let_go_on() {
        let mut panel = opened_in(NARROW, desk());
        let tile = panel.window_tile_rect(0);
        let research = panel.row_rect(1);
        panel.cursor = Some(tile.center());
        panel.pointer(&ui::synthetic_input::mouse_press(), 1.0);
        panel.press.as_mut().unwrap().moved(research.center());
        panel.cursor = Some(research.center());
        assert_eq!(panel.drop_target(), Some(1));
        panel.pointer(&ui::synthetic_input::mouse_release(), 1.0);
        assert_eq!(
            panel.take_pick(),
            Some(OverviewPick::MoveWindow {
                tab: 1000,
                to: WorkspaceId(2)
            })
        );
    }

    #[test]
    fn semantic_actions_reach_what_the_rows_do() {
        let mut panel = opened_in(WIDE, desk());
        assert!(panel.activate("overview-room:2"));
        assert_eq!(
            panel.take_pick(),
            Some(OverviewPick::Workspace(WorkspaceId(2)))
        );
        assert!(panel.focus("overview-window:1001"));
        assert_eq!(panel.window_selection(), Some(1001));
        assert!(!panel.activate("overview-room:x"));
        let node = panel.accessibility().unwrap();
        assert_eq!(node.name.as_deref(), Some(t("overview-title").as_str()));
        assert!(node.children.iter().any(|child| child.role == Role::List));
    }

    #[test]
    fn a_closed_overview_draws_and_takes_nothing() {
        let mut panel = OverviewPanel::new();
        assert!(panel.accessibility().is_none());
        assert_eq!(panel.get_rect(), Rect::ZERO);
        let mut scene = Scene::new();
        panel.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(scene.encoding().is_empty());
    }

    #[test]
    fn the_panel_curve_is_settled_at_both_ends_and_never_reverses() {
        assert_eq!(panel_curve(0.0), (0.0, PANEL_RISE_PX, PANEL_START_SCALE));
        assert_eq!(panel_curve(1.0), (1.0, 0.0, 1.0));
        let mut last = panel_curve(0.0);
        for step in 1..=20_u8 {
            let now = panel_curve(f32::from(step) / 20.0);
            assert!(now.0 >= last.0 && now.1 <= last.1 && now.2 >= last.2);
            last = now;
        }
    }
}
