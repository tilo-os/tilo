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
    GroupRow, OutputSpan, TabDescriptor, TaffyTreeExt as _, TileContent, TileHost, Widget,
    WindowGroup,
};
use ui::winit::event::{MouseScrollDelta, WindowEvent};
use ui::{TextRenderer, Theme};

use crate::navigation::{
    self, ActivityCloseReason, Admitted, Application, Command, DesktopPan, HistoryDirection,
    HistoryState, Presentation, RouteIds, ScrollAction, ScrollState, Snapshot,
};

mod search;

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

/// Where a host's tab contents come from.
pub trait TileSource {
    /// The content for `tab` in `workspace`, built the first time the tab's
    /// strip is shown.
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost;

    /// A group's new-tab control was pressed: the tab to open, under `id`.
    /// `None` opens nothing.
    fn new_tab(&mut self, _workspace: &Workspace, _id: TabId) -> Option<Tab> {
        None
    }

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
}

struct Shared {
    tree: DesktopTree,
    /// Bumped on every tree change, so a host republishes navigation only
    /// when something moved.
    revision: u64,
    queue: Vec<Admitted>,
    minter: IdMinter,
    /// Logical width the strip lays out against.
    viewport_width: f64,
    /// The tree was replaced from outside; rows must be reconciled.
    replaced: bool,
    /// The content bounds of each window's active tab in the shown strip,
    /// as last laid out.
    shown_tabs: Vec<(TabId, Rect)>,
    /// Carets and selections of the focused window's editors, as last laid
    /// out ([`ui::presence::capability::subtree_presence`]).
    focused_presence: Vec<(PresenceKey, PresencePayload)>,
    /// Tabs whose content the host asked to build again.
    rebuild: Vec<TabId>,
    search_request: Option<search::SearchRequest>,
    search_open: bool,
    /// Pan, scroll, history and activity state, as last presented.
    presentation: Presentation,
    /// Bumped whenever `presentation` changes. Unlike `revision` it never
    /// asks a host to save or share the tree.
    presented: u64,
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
        let admitted = navigation::admit(command, generation, &shared.tree)?;
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
    search: tilo_desktop_search::DesktopSearch,
    /// The pointer in logical coordinates, while search is open.
    search_pointer: Point,
    platform: ui::platform::PlatformConventions,
}

/// Time the update pass may spend building deferred tabs in one frame. At
/// least one tab is built per frame, however long it takes.
const BUILD_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// A pause in wheel input longer than this ends a swipe. A trackpad keeps
/// reporting while the fingers move and through the momentum after they
/// lift, well inside this.
const SWIPE_GAP: Duration = Duration::from_millis(200);

/// One horizontally dominant wheel gesture, from its first event over the
/// step threshold until wheel input pauses for [`SWIPE_GAP`].
#[derive(Clone, Copy)]
struct Swipe {
    direction: i32,
    /// The strip took the gesture (stepped focus or pans), so the rest of
    /// it is the strip's; else it belongs to the content under the pointer.
    stepped: bool,
    /// A trackpad's pixel-delta gesture, which pans the strip under the
    /// fingers rather than stepping a window.
    pan: bool,
    last: Instant,
}

/// A wheel event's sideways direction when horizontal motion dominates
/// (positive is forward, like Super+Right), and whether it is past the step
/// threshold: half a line, a pixel delta counting 40 px to the line. The
/// shell's overlay host classifies wheel input the same way.
fn sideways(delta: MouseScrollDelta) -> Option<(i32, bool)> {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
        MouseScrollDelta::PixelDelta(p) => (p.x / 40.0, p.y / 40.0),
    };
    (dx.abs() > dy.abs()).then_some((if dx > 0.0 { 1 } else { -1 }, dx.abs() > 0.5))
}

impl DesktopSurface {
    /// Present `tree`. `salt` is this device's peer salt for new ids.
    pub fn new(tree: DesktopTree, salt: u16, tiles: Box<dyn TileSource>) -> (Self, DesktopHandle) {
        let mut minter = IdMinter::new(salt);
        observe_ids(&mut minter, &tree);
        let shared = Rc::new(RefCell::new(Shared {
            tree,
            revision: 1,
            queue: Vec::new(),
            minter,
            viewport_width: ui::widget::DEFAULT_STRIP_USABLE_PX,
            replaced: false,
            shown_tabs: Vec::new(),
            focused_presence: Vec::new(),
            rebuild: Vec::new(),
            search_request: None,
            search_open: false,
            presentation: Presentation::default(),
            presented: 0,
        }));
        let mut surface = Self {
            shared: Rc::clone(&shared),
            tiles,
            rows: HashMap::new(),
            shown: None,
            rect: Rect::ZERO,
            structure_dirty: true,
            close_reports: Vec::new(),
            reveal: None,
            unbuilt: Vec::new(),
            swipe: None,
            search: tilo_desktop_search::DesktopSearch::new(),
            search_pointer: Point::ZERO,
            platform: ui::platform::PlatformConventions::default(),
        };
        surface.show_active_strip();
        (surface, DesktopHandle { shared })
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
            let (row, deferred) = build_row_lazily(self.tiles.as_mut(), workspace, strip, focused);
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
        let mut built = false;
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
            *host = build_tile(self.tiles.as_mut(), workspace, model);
            built = true;
            if ui::time::now().duration_since(started) >= BUILD_BUDGET {
                break;
            }
        }
        waiting.extend(queue);
        drop(shared);
        self.unbuilt = waiting;
        self.structure_dirty |= built;
    }

    /// Rebuild the rows from a replaced tree, keeping live content by tab id.
    fn reconcile(&mut self) {
        if !std::mem::take(&mut self.shared.borrow_mut().replaced) {
            return;
        }
        let mut live: HashMap<TabId, TileHost> = HashMap::new();
        let unbuilt: std::collections::HashSet<TabId> =
            self.unbuilt.iter().map(|&(_, tab)| tab).collect();
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<TabId> = group
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .filter(|tab| !unbuilt.contains(tab))
                    .collect();
                for tab in tabs {
                    if let Some(host) = group.host_for_tab_mut(tab) {
                        live.insert(tab, std::mem::replace(host, TileHost::new(placeholder())));
                    }
                }
            }
        }
        self.rows.clear();
        {
            let shared = self.shared.borrow();
            // Strips with surviving content are rebuilt now so that content
            // is not dropped; the rest are built when first shown.
            for workspace in &shared.tree.workspaces {
                for strip in &workspace.strips {
                    let survives = strip
                        .window_groups
                        .iter()
                        .flat_map(|group| &group.tabs)
                        .any(|tab| live.contains_key(&tab.id));
                    if survives {
                        let row = build_row(self.tiles.as_mut(), workspace, strip, &mut live);
                        self.rows.insert(strip.id, row);
                    }
                }
            }
        }
        for mut host in live.into_values() {
            self.close_reports
                .extend(ui::component_lifecycle::close_components(&mut host));
        }
        // Rows rebuilt above built every tab; the rest rebuild when shown.
        self.unbuilt.clear();
        self.shown = None;
        self.structure_dirty = true;
    }

    /// Mirror gestures the shown row performed into the tree.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered harvest, matching the shell overlay host's pass order"
    )]
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
        let mut new_tabs = Vec::new();
        for group in &mut row.groups {
            for tab in group.take_pending_close_commits() {
                changed |= shared.tree.remove_tab(tab).is_some();
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
                    model.active_tab = tab;
                }
                shared.tree.set_active_group(strip_id, group.id);
                changed = true;
            }
            if let Some(permille) = group.take_pending_width_change()
                && let Some(model) = find_group(&mut shared.tree, strip_id, group.id)
            {
                model.width = WindowWidth::Fraction(permille);
                changed = true;
            }
            if let Some(maximized) = group.take_pending_maximize_change()
                && let Some(model) = find_group(&mut shared.tree, strip_id, group.id)
            {
                model.maximized = maximized;
                changed = true;
            }
            if group.take_pending_new_tab().is_some() {
                new_tabs.push(group.id);
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
        for group_id in new_tabs {
            let id = shared.minter.mint();
            let Some(workspace) = shared.tree.active_workspace() else {
                continue;
            };
            let Some(tab) = self.tiles.new_tab(workspace, id) else {
                continue;
            };
            let host = build_tile(self.tiles.as_mut(), workspace, &tab);
            let Some(model) = find_group(&mut shared.tree, strip_id, group_id) else {
                continue;
            };
            model.tabs.push(tab.clone());
            model.active_tab = tab.id;
            let index = model.tabs.len().saturating_sub(1);
            shared.tree.set_active_group(strip_id, group_id);
            if let Some(group) = row.groups.iter_mut().find(|g| g.id == group_id) {
                group.insert_detached_tab(index, descriptor(&tab), host);
                group.set_active_tab(tab.id);
            }
            changed = true;
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
    fn apply_commands(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let queue = std::mem::take(&mut self.shared.borrow_mut().queue);
        if queue.is_empty() {
            return;
        }
        let mut ops = Vec::new();
        {
            let mut shared = self.shared.borrow_mut();
            for command in queue {
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
                            group.active_tab = ids.tab;
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
                        if let Some(removed) = shared.tree.remove_tab(ids.tab) {
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
                    Admitted::Activity(workspace, view) => {
                        let mut next = shared.presentation.clone();
                        next.activity = Some((workspace, view));
                        next.activity_close_reason = None;
                        shared.present(next);
                        continue;
                    }
                    Admitted::CloseActivity(workspace) => {
                        let mut next = shared.presentation.clone();
                        if next.activity.is_some_and(|(open, _)| open == workspace) {
                            next.activity = None;
                            next.activity_close_reason = Some(ActivityCloseReason::Dismissed);
                        }
                        shared.present(next);
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
                        row.scroll_group_into_view(group);
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
                RowOp::ToggleMaximized(strip, group) => {
                    if let Some(group) = self.group_widget(strip, group) {
                        group.toggle_maximized();
                        // The tree already holds the new state.
                        let _ = group.take_pending_maximize_change();
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
        if next
            .activity
            .is_some_and(|(workspace, _)| workspace != tree_active)
        {
            next.activity = None;
            next.activity_close_reason = Some(ActivityCloseReason::WorkspaceChanged);
        }
        self.shared.borrow_mut().present(next);
    }

    fn shown_row_ref(&self) -> Option<&GroupRow> {
        self.rows.get(&self.shown?)
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
                *host = build_tile(self.tiles.as_mut(), workspace, tab);
            }
            row.insert_group(built, index);
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
        let (active, width) = {
            let shared = self.shared.borrow();
            let active = navigation::active_route(&shared.tree).map(|ids| ids.group);
            (active, shared.viewport_width)
        };
        if let Some(row) = self.shown_row() {
            for group in &mut row.groups {
                group.set_focused(Some(group.id) == active);
            }
            if row
                .output_spans()
                .first()
                .is_none_or(|span| span.width.to_bits() != width.to_bits())
            {
                row.set_output_spans(&[OutputSpan {
                    x: 0.0,
                    width,
                    usable: width,
                }]);
            }
        }
    }

    /// The workspace shown, for hosts that label their chrome.
    pub fn shown_workspace(&self) -> Option<WorkspaceId> {
        Some(self.shared.borrow().tree.active_workspace()?.id)
    }

    /// Whether the strip takes this wheel event. A trackpad's sideways swipe
    /// (pixel deltas) pans the strip under the fingers, 1:1 and through its
    /// momentum, when the strip can move that way; else it is the content's.
    /// A mouse's sideways wheel click (line deltas) steps focus one window,
    /// as in the shell, and falls through to content when there is no window
    /// that way. One gesture arrives as many events and all of them follow
    /// the decision its first one made.
    fn route_swipe(&mut self, delta: MouseScrollDelta, scale_factor: f64, now: Instant) -> bool {
        if self
            .swipe
            .is_some_and(|swipe| now.saturating_duration_since(swipe.last) > SWIPE_GAP)
        {
            self.swipe = None;
        }
        let Some((direction, past_threshold)) = sideways(delta) else {
            return false;
        };
        if let MouseScrollDelta::PixelDelta(p) = delta {
            // Winit reports the page's deltaX negated, in physical pixels.
            return self.route_pan(-p.x / scale_factor.max(f64::EPSILON), direction, now);
        }
        if let Some(swipe) = self.swipe.as_mut()
            && !swipe.pan
            && swipe.direction == direction
        {
            swipe.last = now;
            return swipe.stepped;
        }
        if !past_threshold {
            return false;
        }
        let stepped = self.step_focus(direction);
        self.swipe = Some(Swipe {
            direction,
            stepped,
            pan: false,
            last: now,
        });
        stepped
    }

    /// Move the shown strip `travel` logical pixels (positive toward later
    /// windows) as part of a pan gesture. A reversal mid-gesture keeps
    /// panning; the ends stop the strip without handing the rest of the
    /// gesture to the app under the pointer.
    fn route_pan(&mut self, travel: f64, direction: i32, now: Instant) -> bool {
        let taken = if let Some(swipe) = self.swipe.as_mut()
            && swipe.pan
        {
            swipe.last = now;
            swipe.stepped
        } else {
            let taken = self.shown_row_ref().is_some_and(|row| can_pan(row, travel));
            self.swipe = Some(Swipe {
                direction,
                stepped: taken,
                pan: true,
                last: now,
            });
            taken
        };
        if taken && let Some(row) = self.shown_row() {
            let offset = row.scroll_x() + travel;
            pan_row(row, offset);
        }
        taken
    }

    /// Focus the window `steps` along the shown strip, walking every tab of
    /// every group in order and stopping at either end, as Super+Left/Right
    /// does in the shell. False when there is nowhere to go.
    fn step_focus(&self, steps: i32) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(current) = navigation::active_route(&shared.tree) else {
            return false;
        };
        let target = {
            let Some(strip) = shared
                .tree
                .workspaces
                .iter()
                .find(|w| w.id == current.workspace)
                .and_then(|w| w.strips.iter().find(|s| s.id == current.strip))
            else {
                return false;
            };
            let windows: Vec<(GroupId, TabId)> = strip
                .window_groups
                .iter()
                .flat_map(|g| g.tabs.iter().map(move |t| (g.id, t.id)))
                .collect();
            let Some(at) = windows
                .iter()
                .position(|&(g, t)| g == current.group && t == current.tab)
            else {
                return false;
            };
            let Some(&(group, tab)) = at
                .checked_add_signed(isize::try_from(steps).unwrap_or(0))
                .and_then(|index| windows.get(index))
            else {
                return false;
            };
            RouteIds {
                group,
                tab,
                ..current
            }
        };
        shared.queue.push(Admitted::Select(target));
        true
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

/// Pan `row` to `offset` the way the shell's direct strip pan does, within
/// the legal range. A strip showing a maximized window stays at home.
fn pan_row(row: &mut GroupRow, offset: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let target = offset.clamp(0.0, row.pan_max_scroll_x());
    row.begin_strip_pan();
    let moved = row.update_strip_pan(target - row.scroll_x());
    row.end_strip_pan(ui::time::now());
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
                        *host = build_tile(tiles, workspace, tab);
                    }
                } else {
                    deferred.push((!active, index.abs_diff(focus), tab.id));
                }
            }
            built
        })
        .collect();
    deferred.sort_by_key(|&(inactive, distance, _)| (inactive, distance));
    let order = deferred.into_iter().map(|(_, _, tab)| tab).collect();
    (GroupRow::new(groups), order)
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
                        .unwrap_or_else(|| build_tile(tiles, workspace, tab));
                }
            }
            built
        })
        .collect();
    GroupRow::new(groups)
}

/// `tab`'s content from `tiles`, its application named to a panic hook
/// while it is built and whenever it runs.
fn build_tile(tiles: &mut dyn TileSource, workspace: &Workspace, tab: &Tab) -> TileHost {
    let _running = ui::widget::panic_scope::enter(Some(tab.id));
    let host = tiles.tile(workspace, tab);
    host.set_panic_scope(tab.id);
    host
}

fn descriptor(tab: &Tab) -> TabDescriptor {
    TabDescriptor::new(tab.id, tab.title.clone())
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

    fn sync(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if let Some(row) = self.shown_row() {
            row.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let before = self.shared.borrow().revision;
        let search_was_open = self.search.is_open();
        self.reconcile();
        self.harvest();
        {
            let (doc, root) = ui::doc::document(state, path);
            self.apply_commands(doc, root);
            // Search opens over the tree as these commands left it, and what
            // it chose applies in this same pass.
            self.step_search(doc);
            self.apply_commands(doc, root);
        }
        self.show_active_strip();
        self.build_deferred();
        self.present();
        let changed =
            self.shared.borrow().revision != before || search_was_open != self.search.is_open();
        let row_changed = self.shown_row().is_some_and(|row| row.update(state, path));
        self.refresh_presentation();
        changed || row_changed || self.structure_dirty
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.present();
        let row: Option<NodeId> = self
            .shown_row()
            .map(|row| row.build_layout(taffy, theme, node_map));
        let search = self.search_layout(taffy, theme, node_map);
        // The row, when shown, is always the first child.
        let children: Vec<NodeId> = row.into_iter().chain(search).collect();
        let node = taffy.parent_node(
            Style {
                display: ui::taffy::Display::Flex,
                flex_direction: ui::taffy::FlexDirection::Column,
                size: ui::taffy::Size {
                    width: ui::taffy::Dimension::percent(1.0),
                    height: ui::taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let nodes = taffy.children(node).unwrap_or_default();
        let has_row = self.shown_row().is_some();
        let child = if has_row {
            nodes.first().copied()
        } else {
            None
        };
        if self.search.is_open()
            && let Some(&palette) = nodes.get(usize::from(has_row))
        {
            self.search
                .palette_mut()
                .apply_layout(taffy, palette, inner);
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
        let shown_tabs = self
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
        if let Some(row) = self.shown_row() {
            row.draw(scene, transform, text, theme, presence);
        }
        self.draw_search(scene, transform, text, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        if let Some(row) = self.shown_row() {
            row.draw_with_presence(scene, transform, text, theme, presence, permissions);
        }
        self.draw_search(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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
        let child = ctx
            .taffy
            .children(ctx.node_id)
            .unwrap_or_default()
            .first()
            .copied();
        if let (Some(row), Some(child)) = (self.shown_row(), child) {
            let path = ctx.path.clone();
            row.handle_event(event, &mut ctx.reborrow(&path, child));
        }
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
        self.shown
            .and_then(|strip| self.rows.get(&strip))
            .map(|row| row as &dyn Widget)
            .into_iter()
            .chain(search)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let open = self.search.is_open();
        let row = self
            .shown
            .and_then(|strip| self.rows.get_mut(&strip))
            .map(|row| row as &mut (dyn Widget + '_));
        let search = open.then(|| self.search.palette_mut() as &mut (dyn Widget + '_));
        row.into_iter().chain(search).collect()
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
        self.unbuilt.iter().any(|&(strip, _)| Some(strip) == shown)
            || self
                .children()
                .into_iter()
                .any(Widget::needs_animation_frame)
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
        let own = std::mem::take(&mut self.structure_dirty);
        let row = self.shown_row().is_some_and(Widget::take_structure_dirty);
        own || row
    }
}

impl ui::component_lifecycle::ComponentLifecycle for DesktopSurface {
    /// Every tab's application, shown or not, closes with the surface.
    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        self.each_host(|host| ui::component_lifecycle::close_components(host))
    }

    fn take_component_close_reports(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        self.each_host(|host| ui::component_lifecycle::take_component_close_reports(host))
    }
}

impl ui::widget::controls::ActionControls for DesktopSurface {
    /// An action row the palette queued outlives the palette closing on it.
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(self.search.palette_mut())
            .or_else(|| ui::widget::controls::take_control_invocation(self.shown_row()?))
    }
}

impl DesktopSurface {
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
mod tests {
    use super::*;
    use ui::winit::dpi::PhysicalPosition;

    struct Notices;

    impl TileSource for Notices {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
            TileHost::new(TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    fn desktop(windows: usize) -> (DesktopSurface, DesktopHandle) {
        let mut minter = IdMinter::new(7);
        let tabs = (0..windows)
            .map(|i| Tab {
                id: minter.mint(),
                title: format!("App{i}"),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: format!("so.tilo.app{i}"),
                    label: format!("App{i}"),
                    note: String::new(),
                },
            })
            .collect();
        let tree = seed_tree(&mut minter, "Desk", tabs);
        DesktopSurface::new(tree, 7, Box::new(Notices))
    }

    fn focused(surface: &mut DesktopSurface, handle: &DesktopHandle) -> usize {
        let mut state = AutoCommit::new();
        surface.update(&mut state, &ui::automerge::ROOT);
        handle.read(|tree| {
            let active = navigation::active_route(tree).unwrap();
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .position(|g| g.id == active.group)
                .unwrap()
        })
    }

    fn pixels(dx: f64, dy: f64) -> MouseScrollDelta {
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(dx, dy))
    }

    fn ms(start: Instant, ms: u64) -> Instant {
        start.checked_add(Duration::from_millis(ms)).unwrap()
    }

    fn lines(dx: f32) -> MouseScrollDelta {
        MouseScrollDelta::LineDelta(dx, 0.0)
    }

    #[test]
    fn one_run_of_sideways_wheel_clicks_steps_one_window() {
        let (mut surface, handle) = desktop(5);
        let t = ui::time::now();
        let clicks = [0.2, 1.0, 3.0, 1.0, 0.5];
        let taken: Vec<bool> = (0u64..)
            .zip(clicks)
            .map(|(i, dx)| surface.route_swipe(lines(dx), 1.0, ms(t, 16 * i)))
            .collect();
        assert_eq!(
            taken,
            [false, true, true, true, true],
            "content keeps the sub-threshold lead-in; the strip keeps the rest"
        );
        assert_eq!(focused(&mut surface, &handle), 1);

        let t = ms(t, 1000);
        for i in 0..30 {
            surface.route_swipe(lines(1.0), 1.0, ms(t, 16 * i));
        }
        assert_eq!(focused(&mut surface, &handle), 2, "however long it runs");

        let t = ms(t, 16 * 30 + 250);
        assert!(
            surface.route_swipe(lines(1.0), 1.0, t),
            "a pause starts a new run"
        );
        assert_eq!(focused(&mut surface, &handle), 3);
        assert!(
            surface.route_swipe(lines(-1.0), 1.0, ms(t, 16)),
            "reversing mid-run is a new run"
        );
        assert_eq!(focused(&mut surface, &handle), 2);
        assert!(
            !surface.route_swipe(pixels(-80.0, 0.0), 1.0, ms(t, 32)),
            "a trackpad swipe pans rather than steps, and an unlaid strip cannot pan"
        );
        assert_eq!(focused(&mut surface, &handle), 2);
    }

    #[test]
    fn a_swipe_with_nowhere_to_go_and_vertical_scrolling_belong_to_content() {
        let (mut surface, handle) = desktop(1);
        let t = ui::time::now();
        for i in 0..10 {
            assert!(
                !surface.route_swipe(lines(1.0), 1.0, ms(t, 16 * i)),
                "a lone window's app scrolls sideways"
            );
        }
        assert_eq!(focused(&mut surface, &handle), 0);
        let t = ms(t, 1000);
        for i in 0..10 {
            assert!(
                !surface.route_swipe(lines(-1.0), 1.0, ms(t, 16 * i)),
                "and so does the first window's, backwards"
            );
        }

        let (mut surface, handle) = desktop(3);
        let t = ui::time::now();
        for (i, delta) in [
            pixels(30.0, 60.0),
            pixels(0.0, -200.0),
            MouseScrollDelta::LineDelta(0.4, 0.0),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                !surface.route_swipe(delta, 1.0, ms(t, 300 * i as u64)),
                "{delta:?}"
            );
        }
        assert_eq!(focused(&mut surface, &handle), 0);
        assert!(
            surface.route_swipe(MouseScrollDelta::LineDelta(1.0, 0.0), 1.0, ms(t, 1000)),
            "a mouse's sideways wheel click steps like the shell's"
        );
        assert_eq!(focused(&mut surface, &handle), 1);
    }
}
