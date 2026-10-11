//! A keyed list of rows that follows a collection in the resource, with each
//! row's item actions in its context menu.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use ui_core::widget::{AppModel, ModelPath};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use tilo_resource::{Id, Resource};
use ui_core::a11y::{AccessNode, Role};
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::widget::{ContextMenu, Element, MenuItem, TaffyTreeExt, Widget};
use ui_core::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use ui_core::winit::keyboard::{Key, NamedKey};
use ui_core::{Theme, taffy};

use tilo_resource::Membership;

use super::dispatch::Dispatcher;
use super::live::Live;
use super::view::Updaters;

/// One entry of a row's context menu: an action published on the row's item.
struct MenuEntry {
    address: tilo_actions::ActionAddress,
    /// Whether the action still takes arguments once the item is bound; those
    /// open in the action palette, which asks for them.
    prompts: bool,
}

/// The context menu of a row: the actions offered on its item.
#[derive(Default)]
struct RowMenu {
    menu: ContextMenu,
    entries: Vec<MenuEntry>,
    cursor: Point,
    /// An entry that needs arguments, for the host to open in the palette.
    palette: Option<tilo_actions::ActionAddress>,
}

/// One item of the list: its id and where it sits, and its row once built.
/// Most items of a long list are never shown, and never build a row.
struct Row<R: Resource> {
    id: Id,
    /// Height measured the last time the row was laid out at the list's
    /// current width. Rows not measured yet take the measured average.
    height: Option<f64>,
    /// The row's view, from the first time it was mounted for as long as
    /// its item exists.
    view: Option<Box<RowView<R>>>,
}

/// A built row: its widgets and what keeps them in step with its item.
struct RowView<R: Resource> {
    /// Dropping it retires every binding the row registered.
    _alive: Rc<()>,
    /// What keeps the row's bound controls in step with its item. They run
    /// while the row is mounted, and catch up when it mounts again.
    followers: Updaters<R>,
    /// The live revision the followers last ran at.
    followed: u64,
    widget: Element,
    /// Layout node while the row is mounted.
    node: Option<NodeId>,
}

/// Shows `visit` the ids of the items, in order, without copying them, and
/// the collection's [`Membership`]: the same one means the same ids.
pub(crate) type Ids<R> = Box<
    dyn Fn(
        &<R as Resource>::State,
        &mut dyn FnMut(Option<Membership>, &mut dyn ExactSizeIterator<Item = &Id>),
    ),
>;
type BuildRow<R> = Box<dyn Fn(&Id, &Rc<()>, &Updaters<R>) -> Element>;

impl<R: Resource> Row<R> {
    fn node(&self) -> Option<NodeId> {
        self.view.as_ref().and_then(|view| view.node)
    }

    fn widget(&self) -> Option<&dyn Widget> {
        self.view.as_ref().map(|view| view.widget.as_ref())
    }
}

impl<R: Resource> RowView<R> {
    /// Bring the row's bound controls up to `state` at `revision`, when
    /// they are behind it.
    fn follow(&mut self, state: &R::State, revision: u64) {
        if self.followed != revision {
            self.followed = revision;
            self.followers.run(state);
        }
    }
}

/// Which rows a list builds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Builds {
    /// No row was built yet, so it is not known whether rows read their
    /// local state from outside.
    Unknown,
    /// The rows it mounts, each the first time.
    OnMount,
    /// Every row: one reads its local state from outside the row, so all
    /// of them may.
    Every,
}

/// What the tree pushed down to the list last, for rows built later.
#[derive(Default)]
struct Pushed {
    conventions: Option<ui_core::platform::PlatformConventions>,
    environment: Option<ui_core::interaction::InteractionEnvironment>,
    motion_reduced: Option<bool>,
}

impl Pushed {
    fn apply(&self, widget: &mut dyn Widget) {
        if let Some(conventions) = self.conventions {
            widget.set_platform_conventions(conventions);
        }
        if let Some(environment) = self.environment {
            widget.set_interaction_environment(environment);
        }
        if let Some(reduced) = self.motion_reduced {
            widget.set_motion_reduced(reduced);
        }
    }
}

/// Height assumed for a row before any row was measured.
const ESTIMATED_ROW_HEIGHT: f64 = 32.0;
/// Rows mounted before the first layout says what the list shows.
const INITIAL_ROWS: usize = 64;
/// Least distance beyond the shown part of the list that stays mounted.
const MIN_OVERSCAN: f64 = 160.0;
/// Remounts the layouts of one frame (from one `update` to the next draw)
/// may ask for, so rows whose measured heights keep moving the window cannot
/// keep a frame from settling.
const MAX_REMOUNTS_PER_FRAME: u8 = 3;

/// Rows for the items of a collection, in collection order, built by
/// [`ViewCx::each`](super::ViewCx::each).
///
/// A row is built the first time it is mounted, and then stays built — its
/// bindings, followers and local state live as long as its item — so
/// opening a list of any length builds the rows it shows, and only removed
/// items drop rows. Inside a scroll view only the rows near what the view
/// shows are *mounted*: laid out, synced, drawn and given events. The list
/// places its rows at measured offsets (estimated for rows never
/// mounted), so it keeps its whole height and a scroll position keeps its
/// meaning. Adding or removing an item away from the shown rows changes the
/// list's geometry, not the window's widget tree. The row holding focus
/// stays mounted with its neighbours wherever the view is scrolled, so Tab
/// and the arrow keys always find the next row, as does a row holding the
/// pointer.
///
/// A change to the resource brings the bound controls of the mounted rows
/// up to date; a row out of view catches up as it mounts, so it shows its
/// item as it is then. A change costs the rows shown, not the rows held.
/// A row whose local state something else reads — an edit it binds into
/// the published tree, an effect it answers — keeps following while out of
/// view. Such a row is published whether or not anyone looked at it, so a
/// list whose rows do that builds every row, as soon as one of them shows
/// it does (the first row is built with the list to find out).
///
/// Agents and assistive technology reach every item regardless: the items
/// are published from the resource, not from the rows, and the list's
/// semantics give each mounted row its position in the whole collection.
pub struct Each<R: Resource> {
    live: Live<R>,
    dispatch: Dispatcher<R>,
    menu: RowMenu,
    ids: Ids<R>,
    build: BuildRow<R>,
    rows: Vec<Row<R>>,
    built: u64,
    /// The membership of the collection `rows` follows, when it has one.
    membership: Option<Membership>,
    builds: Builds,
    pushed: Pushed,
    /// Indices into `rows` of the mounted rows, ascending.
    mounted: Vec<usize>,
    /// Top of each row in list coordinates, and the list's whole height.
    tops: Vec<f64>,
    total: f64,
    /// The sum and count of the measured heights, and the estimate the
    /// tops were laid out with.
    measured: (f64, u32),
    estimated: f64,
    gap: f64,
    /// Width the measured heights hold for.
    width: f64,
    node: Option<NodeId>,
    rect: Rect,
    /// The span of the list a scroll view shows, in list coordinates, as of
    /// the last layout; `None` when nothing clips the list.
    visible: Option<(f64, f64)>,
    laid_out: bool,
    remounts: u8,
    structure_dirty: bool,
    geometry_dirty: bool,
}

impl<R: Resource> Each<R> {
    pub(crate) fn new(
        live: Live<R>,
        dispatch: Dispatcher<R>,
        ids: Ids<R>,
        build: BuildRow<R>,
    ) -> Self {
        let mut each = Self {
            built: live.revision(),
            live,
            dispatch,
            menu: RowMenu::default(),
            ids,
            build,
            rows: Vec::new(),
            membership: None,
            builds: Builds::Unknown,
            pushed: Pushed::default(),
            mounted: Vec::new(),
            tops: Vec::new(),
            total: 0.0,
            measured: (0.0, 0),
            estimated: ESTIMATED_ROW_HEIGHT,
            gap: 0.0,
            width: 0.0,
            node: None,
            rect: Rect::ZERO,
            visible: None,
            laid_out: false,
            remounts: 0,
            structure_dirty: false,
            geometry_dirty: false,
        };
        each.reconcile();
        each
    }

    fn reconcile(&mut self) {
        let rows = &self.rows;
        let known = self.membership;
        let (membership, ids) = self.live.with(|state| {
            // Most changes leave the items as they were. The same
            // membership says so outright; else compare in place, and copy
            // the ids only when they differ.
            let mut membership = None;
            let mut unchanged = false;
            (self.ids)(state, &mut |current_membership, current| {
                membership = current_membership;
                unchanged = (membership.is_some() && membership == known)
                    || (current.len() == rows.len()
                        && current.zip(rows).all(|(id, row)| *id == row.id));
            });
            if unchanged {
                return (membership, None);
            }
            let mut ids = Vec::new();
            (self.ids)(state, &mut |_, current| ids = current.cloned().collect());
            (membership, Some(ids))
        });
        self.membership = membership;
        let Some(ids) = ids else {
            return;
        };
        let was_mounted: Vec<Id> = self
            .mounted
            .iter()
            .filter_map(|&index| self.rows.get(index))
            .map(|row| row.id.clone())
            .collect();
        let mut old: HashMap<Id, Row<R>> = self
            .rows
            .drain(..)
            .map(|row| (row.id.clone(), row))
            .collect();
        // An added item's row is built when it is first mounted.
        self.rows = ids
            .into_iter()
            .map(|id| {
                old.remove(&id).unwrap_or(Row {
                    id,
                    height: None,
                    view: None,
                })
            })
            .collect();
        // A mounted row that left (its widget is gone from under the node
        // map) or moved among the mounted ones changes the window's tree.
        // Anything else only moves rows: the next layout mounts whatever
        // came into view.
        self.mounted = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.node().is_some())
            .map(|(index, _)| index)
            .collect();
        let still_mounted = self.mounted().map(|(id, _)| id);
        if !still_mounted.eq(was_mounted.iter()) {
            self.structure_dirty = true;
        }
        if self.builds == Builds::Every {
            self.build_all();
        } else if self.builds == Builds::Unknown && !self.rows.is_empty() {
            // Whether rows read their local state from outside, which
            // makes every row's view part of the published tree.
            self.ensure_built(0);
        }
        self.recount();
        self.relayout_tops(0);
        self.geometry_dirty = true;
    }

    /// Build the row at `index` if it is not built yet, from the state as
    /// it is now. A row that reads its local state from outside makes the
    /// list build every row.
    fn ensure_built(&mut self, index: usize) {
        let revision = self.live.revision();
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if row.view.is_some() {
            return;
        }
        let alive = Rc::new(());
        let followers = Updaters::new();
        let mut widget = (self.build)(&row.id, &alive, &followers);
        self.pushed.apply(widget.as_mut());
        let reads_unmounted = followers.read_unmounted();
        row.view = Some(Box::new(RowView {
            _alive: alive,
            followers,
            followed: revision,
            widget,
            node: None,
        }));
        if reads_unmounted && self.builds != Builds::Every {
            self.builds = Builds::Every;
            self.build_all();
        } else if self.builds == Builds::Unknown {
            self.builds = Builds::OnMount;
        }
    }

    fn build_all(&mut self) {
        for index in 0..self.rows.len() {
            self.ensure_built(index);
        }
    }

    /// Bring the bound controls of the mounted rows up to the live state,
    /// and of every row whose local state something outside it reads. The
    /// others catch up when they mount ([`Widget::build_layout`]), so a
    /// change costs the rows shown, not the rows held.
    fn follow_rows(&mut self) {
        let revision = self.live.revision();
        let state = self.live.get();
        if self.builds == Builds::Every {
            for view in self.rows.iter_mut().filter_map(|row| row.view.as_mut()) {
                if view.node.is_some() || view.followers.read_unmounted() {
                    view.follow(&state, revision);
                }
            }
            return;
        }
        // Only a list that builds every row holds rows that follow while
        // unmounted.
        for &index in &self.mounted {
            if let Some(view) = self.rows.get_mut(index).and_then(|row| row.view.as_mut()) {
                view.follow(&state, revision);
            }
        }
    }

    /// Height assumed for rows not measured yet: the measured average.
    fn estimate(&self) -> f64 {
        let (sum, count) = self.measured;
        if count == 0 {
            ESTIMATED_ROW_HEIGHT
        } else {
            sum / f64::from(count)
        }
    }

    /// Sum the measured heights again, after rows came or went.
    fn recount(&mut self) {
        self.measured = self
            .rows
            .iter()
            .filter_map(|row| row.height)
            .fold((0.0, 0_u32), |(sum, count), height| {
                (sum + height, count.saturating_add(1))
            });
    }

    /// Place the rows from `from` on; all of them when the estimate the
    /// others were placed with moved.
    fn relayout_tops(&mut self, from: usize) {
        let estimate = self.estimate();
        let from = if (estimate - self.estimated).abs() > f64::EPSILON
            || self.tops.len() != self.rows.len()
        {
            0
        } else {
            from.min(self.rows.len())
        };
        self.estimated = estimate;
        self.tops.truncate(from);
        let mut y = from
            .checked_sub(1)
            .and_then(|last| {
                let top = self.tops.get(last)?;
                let row = self.rows.get(last)?;
                Some(top + row.height.unwrap_or(estimate))
            })
            .unwrap_or(0.0);
        for (index, row) in self.rows.iter().enumerate().skip(from) {
            if index > 0 {
                y += self.gap;
            }
            self.tops.push(y);
            y += row.height.unwrap_or(estimate);
        }
        self.total = y;
    }

    /// The rows reaching into `top..bottom` of the list, by index. A row
    /// reaches down to the next row's top.
    fn rows_between(&self, top: f64, bottom: f64) -> Range<usize> {
        let first = self
            .tops
            .partition_point(|&row_top| row_top <= top)
            .saturating_sub(1);
        let end = self.tops.partition_point(|&row_top| row_top < bottom);
        first..end.max(first)
    }

    /// Distance beyond what the view shows that the window keeps mounted.
    fn overscan(&self) -> f64 {
        self.visible.map_or(0.0, |(top, bottom)| {
            ((bottom - top) * 0.5).max(MIN_OVERSCAN)
        })
    }

    /// The mounted row holding keyboard focus, by index.
    fn focused_index(&self) -> Option<usize> {
        self.mounted.iter().copied().find(|&index| {
            self.rows
                .get(index)
                .and_then(Row::widget)
                .is_some_and(holds_focus)
        })
    }

    /// The focused row and the rows beside it: where Tab and the arrow keys
    /// go next, whether or not the view follows focus.
    fn around_focus(&self) -> Range<usize> {
        self.focused_index().map_or(0..0, |index| {
            index.saturating_sub(1)..index.saturating_add(2).min(self.rows.len())
        })
    }

    /// The rows to mount: all of them when nothing clips the list; else the
    /// shown ones with an overscan on both sides, the focused row and its
    /// neighbours, and every mounted row holding the pointer.
    fn window(&self) -> Vec<usize> {
        let count = self.rows.len();
        if !self.laid_out {
            return (0..count.min(INITIAL_ROWS)).collect();
        }
        let Some((top, bottom)) = self.visible else {
            return (0..count).collect();
        };
        let overscan = self.overscan();
        let mut window: Vec<usize> = self
            .rows_between(top - overscan, bottom + overscan)
            .collect();
        window.extend(self.around_focus());
        window.extend(self.mounted.iter().copied().filter(|&index| {
            self.rows
                .get(index)
                .and_then(Row::widget)
                .is_some_and(|widget| {
                    widget.wants_pointer_capture() || widget.modal_capture().is_some()
                })
        }));
        window.sort_unstable();
        window.dedup();
        window
    }

    /// Whether the mounted rows no longer suit what the view shows: a row
    /// near the shown ones or beside the focused one is missing, or far more
    /// are mounted than needed.
    fn window_stale(&self) -> bool {
        let Some((top, bottom)) = self.visible else {
            return self.mounted.len() != self.rows.len();
        };
        let mounts = |rows: Range<usize>| {
            rows.is_empty()
                || self.mounted.binary_search(&rows.start).is_ok_and(|at| {
                    let last = at.saturating_add(rows.len()).saturating_sub(1);
                    self.mounted.get(last) == Some(&rows.end.saturating_sub(1))
                })
        };
        let overscan = self.overscan();
        let needed = self.rows_between(top - overscan / 4.0, bottom + overscan / 4.0);
        let wanted = self.rows_between(top - overscan, bottom + overscan).len();
        !mounts(needed)
            || !mounts(self.around_focus())
            || self.mounted.len() > wanted.saturating_mul(2).saturating_add(8)
    }

    /// Ids of the items, in order.
    pub fn row_ids(&self) -> impl Iterator<Item = &Id> {
        self.rows.iter().map(|row| &row.id)
    }

    /// How many rows are mounted: laid out, synced, drawn and given events.
    #[must_use]
    pub fn mounted_rows(&self) -> usize {
        self.mounted.len()
    }

    /// The mounted rows, each with its item's id. A mounted row is built.
    fn mounted(&self) -> impl Iterator<Item = (&Id, &RowView<R>)> {
        self.mounted.iter().filter_map(|&index| {
            let row = self.rows.get(index)?;
            Some((&row.id, row.view.as_deref()?))
        })
    }

    fn for_each_mounted(&mut self, mut visit: impl FnMut(&mut RowView<R>)) {
        for &index in &self.mounted {
            if let Some(view) = self.rows.get_mut(index).and_then(|row| row.view.as_mut()) {
                visit(view);
            }
        }
    }

    /// Bring the list's own height and the mounted rows' offsets in `taffy`
    /// up to date. Returns whether a style changed.
    fn push_geometry(&mut self, taffy: &mut TaffyTree<()>) -> bool {
        let mut changed = false;
        if let Some(node) = self.node
            && let Ok(style) = taffy.style(node)
            && style.size.height != length(px(self.total))
        {
            let mut style = style.clone();
            style.size.height = length(px(self.total));
            changed |= taffy.set_style(node, style).is_ok();
        }
        for &index in &self.mounted {
            let Some(child) = self.rows.get(index).and_then(Row::node) else {
                continue;
            };
            let top = length(px(self.tops.get(index).copied().unwrap_or_default()));
            if let Ok(style) = taffy.style(child)
                && style.inset.top != top
            {
                let mut style = style.clone();
                style.inset.top = top;
                changed |= taffy.set_style(child, style).is_ok();
            }
        }
        self.geometry_dirty = false;
        changed
    }

    /// Open the context menu of the row for `item` at `at`: every action the
    /// resource offers on that item. Returns whether there was any.
    fn open_menu(&mut self, item: &Id, at: Point) -> bool {
        let children = self.live.with(|state| R::children(state));
        let Some(resource) = children.iter().find(|child| {
            tilo_resource::items::item_of_resource::<R>(child.id.as_str())
                .is_some_and(|(_, id)| &id == item)
        }) else {
            return false;
        };
        let read_only = self.dispatch.read_only();
        let mut menu = ContextMenu::new();
        let mut entries = Vec::new();
        for spec in &resource.actions {
            let entry = if spec.contract.risk == tilo_actions::RiskLevel::Destructive {
                MenuItem::destructive_action(spec.label.clone(), || {})
            } else {
                MenuItem::action(spec.label.clone(), || {})
            };
            menu.push(entry.with_enabled(!read_only));
            entries.push(MenuEntry {
                address: tilo_actions::ActionAddress::new(resource.id.as_str(), spec.id.as_str()),
                prompts: !spec.params().is_empty(),
            });
        }
        if entries.is_empty() {
            return false;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a menu anchor is a logical pixel position on screen"
        )]
        menu.open_at([at.x as f32, at.y as f32]);
        self.menu.menu = menu;
        self.menu.entries = entries;
        true
    }

    /// Act on the menu entry the user chose: invoke it, or hand it to the
    /// palette when it still needs arguments.
    fn choose(&mut self, index: usize) {
        let Some(entry) = self.menu.entries.get(index) else {
            return;
        };
        if entry.prompts {
            self.menu.palette = Some(entry.address.clone());
        } else {
            self.dispatch
                .invoke_at(entry.address.clone(), serde_json::Map::new());
        }
    }

    /// The row holding keyboard focus.
    fn focused_row(&self) -> Option<(&Id, &dyn Widget)> {
        let row = self.rows.get(self.focused_index()?)?;
        Some((&row.id, row.widget()?))
    }

    /// Route an event to the row menu. Returns whether it was consumed.
    fn menu_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.menu.cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if self.menu.menu.is_open() {
            self.menu.menu.handle_event(event, ctx);
            if let Some(index) = self.menu.menu.take_selection() {
                self.choose(index);
            }
            return *ctx.handled;
        }
        let target = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self
                .mounted()
                .find(|(_, view)| view.widget.get_rect().contains(self.menu.cursor))
                .map(|(id, _)| (id.clone(), self.menu.cursor)),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                || (matches!(logical_key, Key::Named(NamedKey::F10))
                    && ctx.modifiers.state().shift_key()) =>
            {
                self.focused_row().map(|(id, widget)| {
                    let rect = widget.get_rect();
                    (id.clone(), Point::new(rect.x0, rect.y1))
                })
            }
            _ => None,
        };
        let Some((item, at)) = target else {
            return false;
        };
        if self.open_menu(&item, at) {
            *ctx.handled = true;
            return true;
        }
        false
    }
}

/// Whether `widget` or a descendant holds keyboard focus.
fn holds_focus(widget: &dyn Widget) -> bool {
    let mut found = widget.is_focused();
    widget.for_each_child(&mut |child| found |= holds_focus(child));
    found
}

fn px(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "list offsets are logical pixels, far below f32::MAX"
    )]
    let value = value as f32;
    value
}

impl<R: Resource> Widget for Each<R> {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.live.revision() != self.built {
            self.built = self.live.revision();
            self.reconcile();
            self.follow_rows();
        }
        // A row synced again when it is next mounted: that rebuild syncs
        // the whole tree.
        self.for_each_mounted(|view| view.widget.sync(state, path));
    }

    // The per-frame update pass drives retained models; every mounted row
    // sees it. It also opens the frame's remount budget.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.remounts = 0;
        let mut changed = false;
        self.for_each_mounted(|view| changed |= view.widget.update(state, path));
        changed
    }

    // Inputs pushed down the tree reach every built row, mounted or not,
    // and rows built later, so a row mounted later shows the same as one
    // that always was.
    fn set_platform_conventions(&mut self, conventions: ui_core::platform::PlatformConventions) {
        self.pushed.conventions = Some(conventions);
        for view in self.rows.iter_mut().filter_map(|row| row.view.as_mut()) {
            view.widget.set_platform_conventions(conventions);
        }
    }

    fn set_interaction_environment(
        &mut self,
        environment: ui_core::interaction::InteractionEnvironment,
    ) {
        self.pushed.environment = Some(environment);
        for view in self.rows.iter_mut().filter_map(|row| row.view.as_mut()) {
            view.widget.set_interaction_environment(environment);
        }
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.pushed.motion_reduced = Some(reduced);
        for view in self.rows.iter_mut().filter_map(|row| row.view.as_mut()) {
            view.widget.set_motion_reduced(reduced);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let gap = f64::from(theme.gap_standard);
        if (gap - self.gap).abs() > f64::EPSILON {
            self.gap = gap;
            self.relayout_tops(0);
        }
        let window = self.window();
        // Only mounted rows hold a node.
        self.for_each_mounted(|view| view.node = None);
        // A row mounted for the first time is built now.
        for &index in &window {
            self.ensure_built(index);
        }
        let revision = self.live.revision();
        let state = self.live.get();
        let mut nodes = Vec::with_capacity(window.len());
        for &index in &window {
            let top = px(self.tops.get(index).copied().unwrap_or_default());
            let Some(row) = self.rows.get_mut(index).and_then(|row| row.view.as_mut()) else {
                continue;
            };
            // A row coming into view shows its item as it is now.
            row.follow(&state, revision);
            // A row out of the tree kept no structure of its own to report:
            // it is built fresh here.
            let _ = row.widget.take_structure_dirty();
            let child = row.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            style.inset.left = length(0.0_f32);
            style.inset.right = length(0.0_f32);
            style.inset.top = length(top);
            style.size.width = taffy::Dimension::auto();
            let _ = taffy.set_style(child, style);
            row.node = Some(child);
            nodes.push(child);
        }
        self.mounted = window;
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(px(self.total)),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let node = taffy.parent_node(style, &nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        self.structure_dirty = false;
        self.geometry_dirty = false;
        node
    }

    // Measured rows move the rows after them: read what Taffy measured and
    // place the mounted rows (and the list's height) to match.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        if self.node != Some(node) {
            return false;
        }
        let width = taffy
            .layout(node)
            .map_or(0.0, |layout| f64::from(layout.size.width));
        let mut measured = false;
        if (width - self.width).abs() > 0.5 {
            // Heights measured at another width no longer hold.
            self.width = width;
            for row in &mut self.rows {
                if row.node().is_none() {
                    row.height = None;
                }
            }
            self.recount();
            measured = true;
        }
        // The first row whose height moved: the rows before it stay put.
        let mut moved = None;
        for &index in &self.mounted {
            let Some(row) = self.rows.get_mut(index) else {
                continue;
            };
            let Some(height) = row
                .node()
                .and_then(|child| taffy.layout(child).ok())
                .map(|layout| f64::from(layout.size.height))
            else {
                continue;
            };
            if row.height.is_none_or(|known| (known - height).abs() > 0.01) {
                let (sum, count) = self.measured;
                self.measured = match row.height.replace(height) {
                    Some(known) => (sum - known + height, count),
                    None => (sum + height, count.saturating_add(1)),
                };
                moved = Some(moved.map_or(index, |first: usize| first.min(index)));
            }
        }
        if measured {
            self.relayout_tops(0);
        } else if let Some(first) = moved {
            measured = true;
            self.relayout_tops(first);
        }
        if measured || self.geometry_dirty {
            self.push_geometry(taffy)
        } else {
            false
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui_core::widget::layout_rect(taffy, node, origin);
        let inner = self.rect.origin();
        self.for_each_mounted(|view| {
            if let Some(child) = view.node {
                view.widget.apply_layout(taffy, child, inner);
            }
        });
        self.visible = ui_core::widget::layout_clip()
            .map(|clip| (clip.y0 - self.rect.y0, clip.y1 - self.rect.y0));
        self.laid_out = true;
        if self.window_stale() && self.remounts < MAX_REMOUNTS_PER_FRAME {
            self.remounts = self.remounts.saturating_add(1);
            self.structure_dirty = true;
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
        self.remounts = 0;
        self.for_each_mounted(|view| {
            if !presence.should_cull(view.widget.get_rect()) {
                view.widget.draw(scene, transform, text, theme, presence);
            }
        });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) {
        if self.menu_event(event, ctx) {
            return;
        }
        let capturing = self.mounted.iter().copied().find(|&index| {
            self.rows
                .get(index)
                .and_then(Row::widget)
                .is_some_and(Widget::wants_pointer_capture)
        });
        if let Some(index) = capturing {
            if let Some(view) = self.rows.get_mut(index).and_then(|row| row.view.as_mut())
                && let Some(child) = view.node
            {
                view.widget
                    .handle_event(event, &mut ctx.reborrow(ctx.path, child));
            }
            return;
        }
        for &index in &self.mounted {
            let Some(view) = self.rows.get_mut(index).and_then(|row| row.view.as_mut()) else {
                continue;
            };
            let Some(child) = view.node else {
                continue;
            };
            view.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, child));
            if *ctx.handled {
                break;
            }
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.menu.is_open()
            || self
                .mounted()
                .any(|(_, view)| view.widget.wants_pointer_capture())
    }

    fn modal_capture(&self) -> Option<Rect> {
        let mut capture = self.menu.menu.modal_capture();
        for (_, view) in self.mounted() {
            if capture.is_none() {
                capture = view.widget.modal_capture();
            }
        }
        capture
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.for_each_mounted(|view| {
            view.widget
                .draw_overlay(scene, transform, text, theme, presence);
        });
        if self.menu.menu.is_open() {
            self.menu
                .menu
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        if let Some(address) = self.menu.palette.take() {
            return Some(address);
        }
        let mut request = None;
        self.for_each_mounted(|view| {
            if request.is_none() {
                request = view.widget.take_action_palette_request();
            }
        });
        request
    }

    fn take_search_request(&mut self) -> bool {
        let mut requested = false;
        self.for_each_mounted(|view| requested |= view.widget.take_search_request());
        requested
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        self.for_each_mounted(|view| dirty |= view.widget.take_structure_dirty());
        dirty
    }

    fn is_layout_dirty(&self) -> bool {
        self.geometry_dirty
            || self
                .mounted()
                .any(|(_, view)| view.widget.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.geometry_dirty
            || self
                .mounted()
                .any(|(_, view)| view.widget.layout_needs_measure())
    }

    // The mounted rows, and an open row menu: it is part of the tree, so
    // assistive technology and inspection see it; it lays itself out where
    // it opened.
    fn children(&self) -> Vec<&dyn Widget> {
        let open = self
            .menu
            .menu
            .is_open()
            .then_some(&self.menu.menu as &dyn Widget);
        self.mounted()
            .map(|(_, view)| view.widget.as_ref())
            .chain(open)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let open = self.menu.menu.is_open();
        let mut children: Vec<&mut (dyn Widget + '_)> =
            Vec::with_capacity(self.mounted.len().saturating_add(1));
        // `mounted` is ascending: split each mounted row off the rest.
        let mut rest: &mut [Row<R>] = &mut self.rows;
        let mut start = 0_usize;
        for &index in &self.mounted {
            let Some((row, tail)) = index.checked_sub(start).and_then(|offset| {
                std::mem::take(&mut rest)
                    .get_mut(offset..)
                    .and_then(<[Row<R>]>::split_first_mut)
            }) else {
                break;
            };
            if let Some(view) = row.view.as_mut() {
                children.push(view.widget.as_mut());
            }
            rest = tail;
            start = index.saturating_add(1);
        }
        if open {
            children.push(&mut self.menu.menu);
        }
        children
    }

    // A list of every item: each mounted row sits in an item that says
    // where it stands in the whole collection, so a screen reader announces
    // "4 of 1,000" whichever rows are mounted.
    fn accessibility(&self) -> Option<AccessNode> {
        let count = self.rows.len();
        let items = self
            .mounted
            .iter()
            .filter_map(|&index| {
                let widget = self.rows.get(index)?.widget()?;
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_position(index.saturating_add(1), count)
                        .with_bounds(widget.get_rect()),
                )
            })
            .collect();
        Some(AccessNode::new(Role::List).with_children(items))
    }

    fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
        (child_index < self.mounted.len()).then_some(child_index)
    }

    fn name(&self) -> &'static str {
        "Each"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
