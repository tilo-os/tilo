//! The production desktop hierarchy, shared by native and portable hosts.
use crate::resolver::{TileRequest, TileResolver};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use tilo_desktop_model::{
    DesktopTree, OutputPartition, RoomId, Strip, StripId, WindowGroup, WindowWidth,
};
use tilo_desktop_surface::tile_app_kind;
use tilo_sort::CompactSort as _;
use ui::record::RecordStore;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Column, Container, GroupRow, Stack, StripDeck, StripView, TabDescriptor, Widget,
    WindowGroup as UiGroup, WorkspaceView,
};
use ui::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

/// Real host services/chrome. Injecting a widget retains its native state and lifecycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavigationChrome {
    #[default]
    InSurface,
    External,
}
/// Spacing of the column that holds the strips.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum WorkArea {
    /// An 8 px band above and below the strips.
    #[default]
    Framed,
    /// `top` px separate the first strip (its windows' tab row) from the
    /// top bar and `bottom` px separate the windows from the bottom edge,
    /// with nothing else in between (the navigation leaves are zero-height
    /// and add no gap). A maximized or fullscreen window abuts the bar and
    /// the bottom edge directly. [`DesktopSurface::style_work_area`]
    /// applies this on every frame, so a maximize reconciled into a live
    /// surface (no rebuild) drops the margins too.
    Flush { top: f32, bottom: f32 },
}
pub struct SurfaceChrome {
    pub navigation: NavigationChrome,
    pub work_area: WorkArea,
    pub topbar: Box<dyn Widget>,
    pub backdrop: Box<dyn Widget>,
    pub empty_workspace: Option<Box<dyn Widget>>,
    /// Drawn above the desktop along the edges of the displays other than
    /// the primary (their compact top bars). It takes no room in layout.
    pub display_chrome: Option<Box<dyn Widget>>,
    /// The leading edge gutter: how far each strip's first window sits
    /// in from the display's leading edge.
    pub leading_inset: f64,
    /// Schedule a host frame when a retained application changes asynchronously.
    pub notify: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}
impl Default for SurfaceChrome {
    fn default() -> Self {
        Self {
            navigation: NavigationChrome::InSurface,
            work_area: WorkArea::Framed,
            topbar: Box::new(crate::topbar::TopBarChrome::from_tree(
                &DesktopTree::default(),
            )),
            backdrop: Box::new(crate::backdrop::DesktopBackdrop::new()),
            empty_workspace: None,
            display_chrome: None,
            leading_inset: 0.0,
            notify: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Unchanged,
    Updated,
    RebuildRequired,
}

pub struct DesktopSurface {
    work_area: WorkArea,
    /// Last fullscreen presentation a host reported to `style_work_area`.
    fullscreen: bool,
    pub(crate) external_navigation: bool,
    column_index: usize,
    /// Where the [`StripDeck`] holding every strip's row sits in the column.
    deck_index: usize,
    pub(crate) navigation: crate::navigation::Navigation,
    root: Box<dyn Widget>,
    component_wake: std::sync::Arc<std::sync::atomic::AtomicBool>,
    retained_wakes: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    wake_sink: std::sync::Arc<dyn Fn() + Send + Sync>,
    context: Option<(tilo_desktop_model::WorkspaceId, RoomId)>,
    empty_workspace_visible: Option<bool>,
    /// Each strip's row, by its place in the deck.
    rows: HashMap<StripId, usize>,
    order: Vec<StripId>,
    failures: Vec<ui::launch::LaunchError>,
    close_reports: Vec<ui::application::ComponentCloseReport>,
    tiles: HashMap<u64, tilo_desktop_model::TileContent>,
    selected_tabs: HashMap<tilo_desktop_model::GroupId, tilo_desktop_model::TabId>,
    workspace_ids: Vec<tilo_desktop_model::WorkspaceId>,
    selected_workspace: usize,
    selected_strip: usize,
    structure_dirty: bool,
}
fn child<'a>(root: &'a dyn Widget, path: &[usize]) -> Option<&'a dyn Widget> {
    match path.split_first() {
        None => Some(root),
        Some((first, tail)) => child(*root.children().get(*first)?, tail),
    }
}
fn child_mut<'a>(root: &'a mut dyn Widget, path: &[usize]) -> Option<&'a mut dyn Widget> {
    match path.split_first() {
        None => Some(root),
        Some((first, tail)) => child_mut(root.children_mut().into_iter().nth(*first)?, tail),
    }
}
/// A strip's groups in the order its row lays them out: display by display
/// in reading order, each display's in strip order. The strip's own order
/// when it isn't partitioned.
pub fn display_ordered<'a>(partition: &OutputPartition, strip: &'a Strip) -> Vec<&'a WindowGroup> {
    let mut groups: Vec<&WindowGroup> = strip.window_groups.iter().collect();
    if partition.displays.len() < 2 {
        return groups;
    }
    let order = partition.reading_order();
    groups.compact_sort_by_key(|g| {
        partition
            .display_of(g.id)
            .and_then(|d| order.iter().position(|id| *id == d))
            .unwrap_or(0)
    });
    groups
}
pub fn build_group(
    group: &WindowGroup,
    room: RoomId,
    records: &RecordStore,
    resolver: &mut dyn TileResolver,
    failures: &mut Vec<ui::launch::LaunchError>,
    wake: &std::sync::Arc<dyn Fn() + Send + Sync>,
) -> UiGroup {
    let tabs = group
        .tabs
        .iter()
        .map(|tab| {
            let resolution = resolver.resolve(TileRequest { room, tab });
            let checked = resolution.holds_its_own_record();
            let mut host = resolution.mount(failures, wake.clone());
            if !checked {
                host.bind_record(records.clone(), crate::records::scratch_tab_record(tab.id));
            }
            (
                TabDescriptor {
                    id: tab.id,
                    title: tab.title.clone(),
                    app_kind: Some(tile_app_kind(&tab.tile)),
                    icon: resolver.tab_icon(tab),
                },
                host,
            )
        })
        .collect();
    UiGroup::from_hosts(group.id, tabs, group.active_tab, ui_width(group.width))
        .with_maximized(group.maximized)
}
impl DesktopSurface {
    pub fn primary_vertical_scroll(
        &self,
        strip: StripId,
        group: tilo_desktop_model::GroupId,
        tab: tilo_desktop_model::TabId,
    ) -> Option<ui::widget::PrimaryScrollRegion> {
        let group = self
            .row(strip)?
            .groups
            .iter()
            .find(|g| g.id == group && g.active_tab_id() == tab)?;
        group.primary_vertical_scroll()
    }
    pub fn apply_primary_vertical_scroll(
        &mut self,
        strip: StripId,
        group: tilo_desktop_model::GroupId,
        tab: tilo_desktop_model::TabId,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.row_mut(strip)
            .and_then(|r| {
                r.groups
                    .iter_mut()
                    .find(|g| g.id == group && g.active_tab_id() == tab)
            })
            .map_or(ui::widget::PrimaryScrollResult::Unsupported, |g| {
                g.apply_primary_vertical_scroll(action)
            })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one construction pass: the widget tree and its index maps must be built together"
    )]
    pub fn build(
        tree: &DesktopTree,
        records: &RecordStore,
        resolver: &mut dyn TileResolver,
        mut chrome: SurfaceChrome,
    ) -> Self {
        let component_wake = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pending = component_wake.clone();
        let notify = chrome.notify.take();
        let wake_sink: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            pending.store(true, std::sync::atomic::Ordering::Release);
            if let Some(notify) = &notify {
                notify();
            }
        });
        if let Some(bar) = chrome
            .topbar
            .as_any_mut()
            .and_then(|v| v.downcast_mut::<crate::topbar::TopBarChrome>())
        {
            bar.sync_from_tree(tree);
        }
        let strips = tree
            .active_workspace()
            .map(|w| w.strips.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();
        let active_strip = tree
            .active_workspace()
            .and_then(|w| {
                tree.active_strip
                    .get(&w.id)
                    .and_then(|id| w.strips.iter().position(|s| s.id == *id))
            })
            .unwrap_or(0);
        let external_navigation = chrome.navigation == NavigationChrome::External;
        let column = Column::new()
            .with_fill_remaining_height()
            .with_horizontal_padding(0.0, 0.0);
        let mut column = match chrome.work_area {
            WorkArea::Framed => {
                column.with_vertical_padding(if external_navigation { 0.0 } else { 8.0 })
            }
            // Styled by `style_work_area` once the rows exist.
            WorkArea::Flush { .. } => column,
        };
        if !external_navigation {
            column = column
                .push(Box::new(WorkspaceView::new(
                    tree.workspaces.len(),
                    tree.workspaces
                        .iter()
                        .position(|w| w.id == tree.active)
                        .unwrap_or(0),
                )))
                .push(Box::new(StripView::new(strips, active_strip)));
        }
        let empty_workspace = tree.active_workspace().is_some_and(|w| !w.has_windows());
        let empty_workspace_visible = chrome.empty_workspace.as_ref().map(|_| empty_workspace);
        let mut row_offset: usize = if external_navigation { 0 } else { 2 };
        if empty_workspace && let Some(empty) = chrome.empty_workspace {
            column = column.push(empty);
            row_offset = row_offset.saturating_add(1);
        }
        let mut rows = HashMap::new();
        let mut order = Vec::new();
        let mut failures = Vec::new();
        let mut deck_rows = Vec::new();
        if let Some(workspace) = tree.active_workspace() {
            for strip in &workspace.strips {
                let groups = display_ordered(&tree.outputs, strip)
                    .into_iter()
                    .map(|g| {
                        build_group(
                            g,
                            workspace.room(),
                            records,
                            resolver,
                            &mut failures,
                            &wake_sink,
                        )
                    })
                    .collect();
                let mut row = GroupRow::new(groups).with_leading_inset(chrome.leading_inset);
                row.set_external_navigation(external_navigation);
                rows.insert(strip.id, deck_rows.len());
                order.push(strip.id);
                deck_rows.push(row);
            }
        }
        let deck_index = row_offset;
        column = column.push(Box::new(StripDeck::new(deck_rows, active_strip)));
        let mut foreground = Column::new().with_no_padding();
        if matches!(chrome.work_area, WorkArea::Flush { .. }) {
            foreground = foreground.with_gap(0.0);
        }
        if !external_navigation {
            foreground = foreground.push(chrome.topbar);
        }
        let foreground = foreground.push(Box::new(column));
        let mut stack = Stack::new()
            .push(chrome.backdrop)
            .push(Box::new(foreground));
        if let Some(display_chrome) = chrome.display_chrome {
            stack = stack.push(display_chrome);
        }
        let root = Box::new(
            Container::new()
                .with_no_padding()
                .with_child(Box::new(stack))
                .filling(),
        );
        let mut surface = Self {
            work_area: chrome.work_area,
            fullscreen: false,
            external_navigation,
            column_index: usize::from(!external_navigation),
            deck_index,
            navigation: crate::navigation::Navigation::default(),
            root,
            context: tree.active_workspace().map(|w| (w.id, w.room())),
            empty_workspace_visible,
            component_wake,
            retained_wakes: vec![],
            wake_sink,
            rows,
            order,
            failures,
            close_reports: vec![],
            selected_tabs: tree
                .active_workspace()
                .into_iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .map(|g| (g.id, g.active_tab))
                .collect(),
            workspace_ids: tree.workspaces.iter().map(|w| w.id).collect(),
            selected_workspace: tree
                .workspaces
                .iter()
                .position(|w| w.id == tree.active)
                .unwrap_or(0),
            selected_strip: active_strip,
            structure_dirty: false,
            tiles: tree
                .active_workspace()
                .into_iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs)
                .map(|t| (t.id, t.tile.clone()))
                .collect(),
        };
        surface.sync_navigation(tree);
        surface.restyle_work_area();
        surface
    }
    /// Space the strip column for what it shows now: a
    /// [`WorkArea::Flush`] column keeps its margins around tiled windows
    /// and none around a maximized or fullscreen one. Hosts that know about
    /// a fullscreen presentation call this every frame; a maximize arrives
    /// through `reconcile` without a rebuild, so the decision cannot be
    /// taken once when the column is built.
    pub fn style_work_area(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
        self.restyle_work_area();
    }
    fn restyle_work_area(&mut self) {
        let WorkArea::Flush { top, bottom } = self.work_area else {
            return;
        };
        let expanded = self.fullscreen
            || self
                .order
                .iter()
                .filter_map(|id| self.row(*id))
                .any(|row| row.groups.iter().any(|g| g.maximized));
        let (top, bottom) = if expanded { (0.0, 0.0) } else { (top, bottom) };
        if let Some(column) = self.work_area_mut() {
            column.set_vertical_padding(0.0);
            // The zero-height navigation leaves ahead of the rows are
            // separated by the column gap too; two stray gaps used to
            // stack above the first strip on top of `top`.
            column.set_gap(0.0);
            column.set_extra_top_padding(top);
            column.set_extra_bottom_padding(bottom);
        }
    }
    /// The primary display's full top bar, when the surface draws one.
    pub fn topbar(&self) -> Option<&crate::topbar::TopBarChrome> {
        (!self.external_navigation)
            .then(|| child(self.root.as_ref(), &[0, 1, 0]))
            .flatten()
            .and_then(Widget::as_any)
            .and_then(|v| v.downcast_ref::<crate::topbar::TopBarChrome>())
    }
    pub fn topbar_mut(&mut self) -> Option<&mut crate::topbar::TopBarChrome> {
        (!self.external_navigation)
            .then(|| child_mut(self.root.as_mut(), &[0, 1, 0]))
            .flatten()
            .and_then(Widget::as_any_mut)
            .and_then(|v| v.downcast_mut::<crate::topbar::TopBarChrome>())
    }
    /// The backdrop under everything, when it is the shared
    /// [`crate::backdrop::DesktopBackdrop`].
    pub fn backdrop(&self) -> Option<&crate::backdrop::DesktopBackdrop> {
        child(self.root.as_ref(), &[0, 0])
            .and_then(Widget::as_any)
            .and_then(|v| v.downcast_ref::<crate::backdrop::DesktopBackdrop>())
    }
    pub fn backdrop_mut(&mut self) -> Option<&mut crate::backdrop::DesktopBackdrop> {
        child_mut(self.root.as_mut(), &[0, 0])
            .and_then(Widget::as_any_mut)
            .and_then(|v| v.downcast_mut::<crate::backdrop::DesktopBackdrop>())
    }
    /// The layer [`SurfaceChrome::display_chrome`] installed.
    pub fn display_chrome(&self) -> Option<&dyn Widget> {
        child(self.root.as_ref(), &[0, 2])
    }
    pub fn display_chrome_mut(&mut self) -> Option<&mut dyn Widget> {
        child_mut(self.root.as_mut(), &[0, 2])
    }
    /// Frame every strip's focused group again on the next layout, as
    /// after the displays or a group's display changed.
    pub fn reframe_all(&mut self) {
        self.navigation.clear_framing();
    }
    pub fn take_component_wake(&self) -> bool {
        let mut pending = self
            .component_wake
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        for wake in &self.retained_wakes {
            pending |= wake.swap(false, std::sync::atomic::Ordering::AcqRel);
        }
        pending
    }
    pub fn workspace_context(&self) -> Option<(tilo_desktop_model::WorkspaceId, RoomId)> {
        self.context
    }
    /// The tabs this surface hosts.
    pub fn tab_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.tiles.keys().copied()
    }
    /// A frame was presented: every strip's window groups may step their
    /// width springs again ([`GroupRow::frame_presented`]).
    pub fn frame_presented(&mut self) {
        for id in self.strip_ids() {
            if let Some(row) = self.row_mut(id) {
                row.frame_presented();
            }
        }
    }

    pub fn strip_ids(&self) -> Vec<StripId> {
        self.order.clone()
    }
    /// Whether retained close visuals still need reconciliation. They are not canonical windows.
    pub fn has_closing_visuals(&self) -> bool {
        self.order
            .iter()
            .filter_map(|id| self.row(*id))
            .any(|row| row.groups.iter().any(UiGroup::is_closing))
    }

    /// Whether a same-workspace rebuild would discard a still-visible close transition.
    pub fn close_animation_in_flight(&self) -> bool {
        self.order.iter().filter_map(|id| self.row(*id)).any(|row| {
            row.groups
                .iter()
                .any(|group| group.is_closing() && !group.close_complete())
        })
    }

    /// Every private window this surface hosts, by tab, with the resources
    /// it offers this device's automation ([`ui::widget::AppWidget::device_local_resources`]).
    pub fn device_local_windows(&mut self) -> Vec<(u64, Vec<tilo_actions::Resource>)> {
        let mut windows = Vec::new();
        self.for_each_app(|tab, app| {
            if let Some(resources) = app.device_local_resources() {
                windows.push((tab, resources));
            }
        });
        windows
    }

    /// Invoke `address` on the private window of tab `tab`, through its door.
    ///
    /// # Errors
    /// Refuses a tab that hosts no private window here, and whatever its
    /// door refuses.
    pub fn invoke_device_local_action(
        &mut self,
        tab: u64,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        let mut result = Err(tilo_actions::DoorError::Unknown(address.clone()));
        self.for_each_app(|candidate, app| {
            if candidate == tab {
                result = app.invoke_device_local_action(address, arguments);
            }
        });
        result
    }

    /// Visit each tab's in-process app, across every strip.
    fn for_each_app(&mut self, mut visit: impl FnMut(u64, &mut dyn ui::widget::AppWidget)) {
        for strip in self.strip_ids() {
            let Some(row) = self.row_mut(strip) else {
                continue;
            };
            for group in &mut row.groups {
                let tabs: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for tab in tabs {
                    if let Some(app) = group
                        .host_for_tab_mut(tab)
                        .and_then(ui::widget::TileHost::app_mut)
                    {
                        visit(tab, app);
                    }
                }
            }
        }
    }

    pub fn row(&self, id: StripId) -> Option<&GroupRow> {
        self.row_at(*self.rows.get(&id)?)
    }
    pub fn row_mut(&mut self, id: StripId) -> Option<&mut GroupRow> {
        self.row_at_mut(*self.rows.get(&id)?)
    }
    fn row_at(&self, index: usize) -> Option<&GroupRow> {
        self.strip_deck()?.row(index)
    }
    fn row_at_mut(&mut self, index: usize) -> Option<&mut GroupRow> {
        self.strip_deck_mut()?.row_mut(index)
    }
    /// The deck of full-height strips, one shown at a time.
    pub fn strip_deck(&self) -> Option<&StripDeck> {
        child(
            self.root.as_ref(),
            &[0, 1, self.column_index, self.deck_index],
        )?
        .as_any()?
        .downcast_ref()
    }
    pub fn strip_deck_mut(&mut self) -> Option<&mut StripDeck> {
        child_mut(
            self.root.as_mut(),
            &[0, 1, self.column_index, self.deck_index],
        )?
        .as_any_mut()?
        .downcast_mut()
    }
    /// The strip at `index` of the workspace's strips, top to bottom.
    pub fn strip_at(&self, index: usize) -> Option<StripId> {
        self.order.get(index).copied()
    }
    /// The column holding the navigation drivers, the empty-workspace card
    /// and one row per strip, for hosts that restyle it per frame.
    /// The [`SurfaceChrome::empty_workspace`] widget, while it is shown.
    pub fn empty_workspace_mut(&mut self) -> Option<&mut dyn Widget> {
        if self.empty_workspace_visible != Some(true) {
            return None;
        }
        let index = if self.external_navigation { 0 } else { 2 };
        child_mut(self.root.as_mut(), &[0, 1, self.column_index, index])
    }
    pub fn work_area_mut(&mut self) -> Option<&mut Column> {
        child_mut(self.root.as_mut(), &[0, 1, self.column_index])?
            .as_any_mut()?
            .downcast_mut()
    }
    pub fn take_setup_failures(&mut self) -> Vec<ui::launch::LaunchError> {
        std::mem::take(&mut self.failures)
    }
    /// Drain widget tab selections since the last canonical reconciliation or harvest.
    /// Hosts must apply these local requests before reconciling a new canonical snapshot.
    /// A canonical change alone is not a widget gesture and must not be written back.
    pub fn take_tab_selection_intents(&mut self) -> Vec<crate::intents::LocalViewIntent> {
        let mut intents = Vec::new();
        let mut selected = std::mem::take(&mut self.selected_tabs);
        for strip in self.strip_ids() {
            if let Some(row) = self.row(strip) {
                for group in &row.groups {
                    if selected.insert(group.id, group.active_tab_id())
                        != Some(group.active_tab_id())
                    {
                        intents.push(crate::intents::LocalViewIntent::Tab {
                            group: group.id,
                            tab: group.active_tab_id(),
                        });
                    }
                }
            }
        }
        self.selected_tabs = selected;
        intents
    }
    /// Harvest requests without mutating the caller's canonical tree or granting session authority.
    pub fn take_intents(&mut self) -> Vec<crate::intents::DesktopIntent> {
        use crate::intents::{self, DesktopIntent, LocalViewIntent};
        let mut out: Vec<_> = self
            .take_tab_selection_intents()
            .into_iter()
            .map(DesktopIntent::Local)
            .collect();
        if let Some(intent) = self.take_group_focus_intent() {
            out.push(DesktopIntent::Local(intent));
        }
        for strip in self.strip_ids() {
            if let Some(row) = self.row_mut(strip) {
                if let Some(intent) = intents::take_group_reorder(strip, row) {
                    out.push(DesktopIntent::Session(intent));
                }
                for group in &mut row.groups {
                    if let Some(intent) = intents::take_maximize(group) {
                        out.push(DesktopIntent::Local(intent));
                    }
                    if let Some(intent) = intents::take_width(group) {
                        out.push(DesktopIntent::Local(intent));
                    }
                    if let Some(intent) = intents::take_tab_reorder(group) {
                        out.push(DesktopIntent::Session(intent));
                    }
                    if let Some(intent) = intents::take_open(strip, group) {
                        out.push(DesktopIntent::Session(intent));
                    }
                    out.extend(
                        intents::take_closes(group)
                            .into_iter()
                            .map(DesktopIntent::Session),
                    );
                }
            }
        }
        if let Some(view) = (!self.external_navigation)
            .then(|| child(self.root.as_ref(), &[0, 1, 1, 0]))
            .flatten()
            .and_then(Widget::as_any)
            .and_then(|v| v.downcast_ref::<WorkspaceView>())
            && view.active_index() != self.selected_workspace
        {
            self.selected_workspace = view.active_index();
            if let Some(id) = self.workspace_ids.get(self.selected_workspace) {
                out.push(DesktopIntent::Local(LocalViewIntent::Workspace(*id)));
            }
        }
        if let Some(view) = (!self.external_navigation)
            .then(|| child(self.root.as_ref(), &[0, 1, 1, 1]))
            .flatten()
            .and_then(Widget::as_any)
            .and_then(|v| v.downcast_ref::<StripView>())
            && view.active_index() != self.selected_strip
        {
            self.selected_strip = view.active_index();
            if let Some(id) = self.order.get(self.selected_strip) {
                out.push(DesktopIntent::Local(LocalViewIntent::Strip(*id)));
            }
        }
        // A drag carried to the deck's edge went to another strip.
        if let Some(index) = self
            .strip_deck_mut()
            .and_then(StripDeck::take_requested_strip)
            && let Some(id) = self.order.get(index).copied()
        {
            self.selected_strip = index;
            out.push(DesktopIntent::Local(LocalViewIntent::Strip(id)));
        }
        out
    }
    /// A client that exits after refusing or deferring a close request
    /// retires its still-visible window here, once. This must precede every
    /// rebuild decision, which would otherwise drop the window unanimated.
    pub fn retire_departed_requested_groups(&mut self, tree: &DesktopTree) {
        for id in self.order.clone() {
            let Some(row) = self.row_mut(id) else {
                continue;
            };
            for group in &mut row.groups {
                if group.close_was_requested()
                    && !group.is_closing()
                    && !tree.workspaces.iter().any(|w| {
                        w.strips
                            .iter()
                            .any(|s| s.window_groups.iter().any(|g| g.id == group.id))
                    })
                {
                    group.start_closing();
                }
            }
        }
    }

    /// Reconcile retained groups and hosts. A changed strip set requires rebuilding with host chrome.
    #[expect(
        clippy::too_many_lines,
        reason = "one retained-tree reconciliation: moves, removals and inserts must see each other's state"
    )]
    pub fn reconcile(
        &mut self,
        tree: &DesktopTree,
        records: &RecordStore,
        resolver: &mut dyn TileResolver,
    ) -> ReconcileOutcome {
        let Some(workspace) = tree.active_workspace() else {
            return ReconcileOutcome::RebuildRequired;
        };
        if Some((workspace.id, workspace.room())) != self.context {
            return ReconcileOutcome::RebuildRequired;
        }
        if workspace.strips.iter().map(|s| s.id).collect::<Vec<_>>() != self.order {
            return ReconcileOutcome::RebuildRequired;
        }
        self.retire_departed_requested_groups(tree);
        let empty = !workspace.has_windows();
        if self
            .empty_workspace_visible
            .is_some_and(|visible| visible != empty)
            && !self.order.iter().filter_map(|id| self.row(*id)).any(|row| {
                row.groups
                    .iter()
                    .any(|group| group.is_closing() && !group.close_complete())
            })
        {
            return ReconcileOutcome::RebuildRequired;
        }
        let mut failures = vec![];
        let mut reports = vec![];
        let mut changed = false;
        self.workspace_ids = tree.workspaces.iter().map(|w| w.id).collect();
        self.selected_workspace = self
            .workspace_ids
            .iter()
            .position(|id| *id == tree.active)
            .unwrap_or(0);
        self.selected_strip = workspace
            .strips
            .iter()
            .position(|s| Some(&s.id) == tree.active_strip.get(&workspace.id))
            .unwrap_or(0);
        let shown = self.selected_strip;
        // What asked for it: a chord says so before acting
        // ([`Self::begin_transition`]); a click or swipe was just handled.
        let source = self
            .navigation
            .transition
            .or_else(|| ui::motion::recent_input(ui::motion::frame_now()));
        if let Some(deck) = self.strip_deck_mut()
            && deck.shown() != shown
        {
            // Going to another strip slides the deck to it: a hand gets the
            // paged-strip settle, a shortcut the same within `fast`. A change
            // nobody's hand made (an agent's turn, a script) lands at once.
            match source {
                Some(source) => deck.show(shown, source),
                None => deck.jump_to(shown),
            }
            changed = true;
        }
        if let Some(view) = (!self.external_navigation)
            .then(|| child_mut(self.root.as_mut(), &[0, 1, 1, 0]))
            .flatten()
            .and_then(Widget::as_any_mut)
            .and_then(|v| v.downcast_mut::<WorkspaceView>())
        {
            changed |= view.reconcile(tree.workspaces.len(), self.selected_workspace);
        }
        if let Some(view) = (!self.external_navigation)
            .then(|| child_mut(self.root.as_mut(), &[0, 1, 1, 1]))
            .flatten()
            .and_then(Widget::as_any_mut)
            .and_then(|v| v.downcast_mut::<StripView>())
        {
            changed |= view.reconcile(
                workspace.strips.iter().map(|s| s.name.clone()).collect(),
                self.selected_strip,
            );
        }
        if let Some(bar) = child_mut(self.root.as_mut(), &[0, 1, 0])
            .and_then(Widget::as_any_mut)
            .and_then(|v| v.downcast_mut::<crate::topbar::TopBarChrome>())
        {
            bar.sync_from_tree(tree);
        }
        let wake_sink = self.wake_sink.clone();
        let mut tiles = std::mem::take(&mut self.tiles);
        let destinations: HashMap<_, _> = workspace
            .strips
            .iter()
            .flat_map(|s| {
                s.window_groups
                    .iter()
                    .flat_map(move |g| g.tabs.iter().map(move |t| (t.id, (s.id, g.id))))
            })
            .collect();
        let mut moved = HashMap::new();
        for strip in self.strip_ids() {
            if let Some(row) = self.row_mut(strip) {
                for group in &mut row.groups {
                    let ids: Vec<_> = group
                        .tabs()
                        .iter()
                        .filter(|t| {
                            destinations
                                .get(&t.id)
                                .is_some_and(|dest| *dest != (strip, group.id))
                        })
                        .map(|t| t.id)
                        .collect();
                    for id in ids {
                        if let Some(pair) = group.detach_tab(id) {
                            moved.insert(id, pair);
                            changed = true;
                        }
                    }
                }
            }
        }
        let mut revealed = Vec::new();
        for strip in &workspace.strips {
            let Some(row) = self.row_mut(strip.id) else {
                continue;
            };
            let stale: Vec<_> = row
                .groups
                .iter()
                .filter(|g| !strip.window_groups.iter().any(|s| s.id == g.id))
                .map(|g| g.id)
                .collect();
            for id in stale {
                if row
                    .groups
                    .iter()
                    .any(|g| g.id == id && g.is_closing() && !g.close_complete())
                {
                    // The declared close (and the ✕) start the close animation
                    // before the tree drop is reconciled; keep the last
                    // frame here until its motion-token deadline.
                    continue;
                }
                if let Some(group) = row.groups.iter_mut().find(|g| g.id == id) {
                    reports.extend(ui::component_lifecycle::close_components(group));
                }
                changed |= row.remove_group_by_id(id);
            }
            let closing_slots: Vec<_> = row
                .groups
                .iter()
                .enumerate()
                .filter(|(_, group)| {
                    group.is_closing()
                        && !strip.window_groups.iter().any(|live| live.id == group.id)
                })
                .map(|(index, group)| (index, group.id))
                .collect();
            // Windows opened since the last pass, as opposed to ones restored
            // with the workspace (built, not reconciled) or brought back by an
            // undo while their close still fades out.
            let mut opened = Vec::new();
            // The row lays groups out display by display; on one display
            // that is the strip's own order.
            for (index, group) in display_ordered(&tree.outputs, strip)
                .into_iter()
                .enumerate()
            {
                let mut revived = false;
                if let Some(dying) = row
                    .groups
                    .iter_mut()
                    .find(|candidate| candidate.id == group.id && candidate.is_closing())
                {
                    revived = true;
                    // Undo may restore the canonical identity during the fade. Its dying
                    // host is not a live application; resolve the restored window afresh.
                    reports.extend(ui::component_lifecycle::close_components(dying));
                    row.remove_group_by_id(group.id);
                }
                if !row.groups.iter().any(|g| g.id == group.id) {
                    changed = true;
                    let mut fresh = UiGroup::from_hosts(
                        group.id,
                        vec![],
                        group.active_tab,
                        ui_width(group.width),
                    )
                    .with_maximized(group.maximized);
                    fresh.start_spawn_animation();
                    row.insert_group(fresh, index);
                    if !revived {
                        opened.push(group.id);
                    }
                }
                let Some(live) = row.groups.iter_mut().find(|g| g.id == group.id) else {
                    continue;
                };
                // Width is a local intent committed on release. An unrelated
                // snapshot revision must not reset a captured edge to its saved
                // width or re-arm the width spring underneath the pointer, nor
                // undo a width the host has yet to persist.
                if !(live.edge_resize_in_flight() || live.has_pending_width_change())
                    || live.maximized != group.maximized
                {
                    changed |= live.width() != ui_width(group.width);
                    live.set_width(ui_width(group.width));
                }
                changed |= live.maximized != group.maximized;
                live.maximized = group.maximized;
                let stale: Vec<_> = live
                    .tabs()
                    .iter()
                    .filter(|t| !group.tabs.iter().any(|s| s.id == t.id))
                    .map(|t| t.id)
                    .collect();
                for id in stale {
                    if let Some((_, mut host)) = live.detach_tab(id) {
                        reports.extend(ui::component_lifecycle::close_components(&mut host));
                        changed = true;
                    }
                }
                for (index, tab) in group.tabs.iter().enumerate() {
                    let descriptor = TabDescriptor {
                        id: tab.id,
                        title: tab.title.clone(),
                        app_kind: Some(tile_app_kind(&tab.tile)),
                        icon: resolver.tab_icon(tab),
                    };
                    if let Some(from) = live.tabs().iter().position(|t| t.id == tab.id) {
                        changed |= live.reorder_tab(from, index);
                        changed |= live.update_tab_descriptor(descriptor.clone());
                        // A window's artwork can arrive after its tab: the
                        // desktop entry is installed, or the app sets its own.
                        changed |= live.set_tab_icon(tab.id, descriptor.icon.clone());
                        let refusal = resolver.creation_refusal(tab);
                        // A window holding its place while its app's record
                        // opens asks again: the host's storage wake brought
                        // this pass.
                        let arriving = live.host_for_tab_mut(tab.id).is_some_and(|h| {
                            matches!(h.content(), ui::widget::TileContent::Arriving)
                        });
                        let replace = tiles.get(&tab.id) != Some(&tab.tile)
                            || arriving
                            || (refusal.is_none()
                                && live
                                    .host_for_tab_mut(tab.id)
                                    .is_some_and(|h| h.record_creation_refused()));
                        if replace && resolver.ready_to_replace(tab) {
                            let resolution = resolver.resolve(TileRequest {
                                room: workspace.room(),
                                tab,
                            });
                            match resolution {
                                // Still opening: the arrival it shows goes on.
                                crate::resolver::TileResolution::Opening if arriving => {
                                    tiles.insert(tab.id, tab.tile.clone());
                                    continue;
                                }
                                // Plain content swaps inside the live host, which keeps
                                // its visibility and its arrival until the new content
                                // presents, and publishes the new content's size on the
                                // next layout.
                                crate::resolver::TileResolution::Content(content) => {
                                    if let Some(host) = live.host_for_tab_mut(tab.id) {
                                        host.replace_content(content);
                                        host.bind_record(
                                            records.clone(),
                                            crate::records::scratch_tab_record(tab.id),
                                        );
                                        if let Some(reason) = refusal {
                                            host.refuse_record_binding(reason);
                                        }
                                    }
                                    tiles.insert(tab.id, tab.tile.clone());
                                    changed = true;
                                    continue;
                                }
                                other => {
                                    let old = live.detach_tab(tab.id).map(|(_, mut old)| {
                                        reports.extend(ui::component_lifecycle::close_components(
                                            &mut old,
                                        ));
                                        old
                                    });
                                    let checked = other.holds_its_own_record();
                                    let mut host = other.mount(&mut failures, wake_sink.clone());
                                    // The app whose record opened goes on with
                                    // the arrival its window showed meanwhile.
                                    if let Some(old) = old.as_ref().filter(|_| arriving) {
                                        host.continue_arrival(old);
                                    }
                                    if !checked {
                                        host.bind_record(
                                            records.clone(),
                                            crate::records::scratch_tab_record(tab.id),
                                        );
                                    }
                                    if let Some(reason) = resolver.creation_refusal(tab) {
                                        host.refuse_record_binding(reason);
                                    }
                                    live.insert_detached_tab(index, descriptor, host);
                                    tiles.insert(tab.id, tab.tile.clone());
                                    changed = true;
                                    continue;
                                }
                            }
                        }
                        if let Some(host) = live.host_for_tab_mut(tab.id)
                            && !arriving
                        {
                            if let Some(reason) = refusal {
                                host.refuse_record_binding(reason);
                            } else if host.callback_binding().is_none() {
                                host.bind_record(
                                    records.clone(),
                                    crate::records::scratch_tab_record(tab.id),
                                );
                            }
                        }
                        continue;
                    }
                    let mut retained = moved.remove(&tab.id);
                    let changed_identity = tiles.get(&tab.id) != Some(&tab.tile);
                    if retained.is_some()
                        && changed_identity
                        && resolver.ready_to_replace(tab)
                        && let Some((_, mut old)) = retained.take()
                    {
                        reports.extend(ui::component_lifecycle::close_components(&mut old));
                    }
                    // A native surface may still be waiting for its first frame. Keep the
                    // old identity snapshot so a later reconciliation retries replacement.
                    let deferred_replacement = retained.is_some() && changed_identity;
                    let mut host = if let Some((_, host)) = retained {
                        host
                    } else {
                        let resolution = resolver.resolve(TileRequest {
                            room: workspace.room(),
                            tab,
                        });
                        let checked = resolution.holds_its_own_record();
                        let mut host = resolution.mount(&mut failures, wake_sink.clone());
                        if !checked {
                            host.bind_record(
                                records.clone(),
                                crate::records::scratch_tab_record(tab.id),
                            );
                        }
                        host
                    };
                    if let Some(reason) = resolver.creation_refusal(tab) {
                        host.refuse_record_binding(reason);
                    }
                    live.insert_detached_tab(index, descriptor, host);
                    changed = true;
                    if !deferred_replacement {
                        tiles.insert(tab.id, tab.tile.clone());
                    }
                }

                if live.active_tab_id() != group.active_tab {
                    changed |= live.set_active_tab(group.active_tab);
                }
            }
            // Closing visuals retain their strip slots while the live groups follow
            // canonical order. Reordering only live groups would move a closing
            // neighbour behind them and animate across its occupied footprint.
            let mut live_order = display_ordered(&tree.outputs, strip)
                .into_iter()
                .map(|group| group.id);
            let mut closing_order = closing_slots.iter().copied().peekable();
            let mut visual_order = Vec::with_capacity(row.groups.len());
            for index in 0..row.groups.len() {
                if closing_order.peek().is_some_and(|(slot, _)| *slot == index) {
                    if let Some((_, id)) = closing_order.next() {
                        visual_order.push(id);
                    }
                } else if let Some(id) = live_order.next() {
                    visual_order.push(id);
                }
            }
            for (index, id) in visual_order.into_iter().enumerate() {
                changed |= row.reorder_group_by_id(id, index);
            }
            // A window that opens is placed whole on screen beside the one
            // it opened next to, sharing the display with it if need be.
            for group in opened {
                changed |= row.share_with_neighbour(group);
                revealed.push((strip.id, group));
            }
        }
        for (_, (_, mut host)) in moved {
            reports.extend(ui::component_lifecycle::close_components(&mut host));
        }
        tiles.retain(|id, _| destinations.contains_key(id));
        self.tiles = tiles;
        self.selected_tabs = workspace
            .strips
            .iter()
            .flat_map(|s| &s.window_groups)
            .map(|g| (g.id, g.active_tab))
            .collect();
        self.failures.extend(failures);
        self.close_reports.extend(reports);
        for (strip, group) in revealed {
            self.navigation.reveal_opened(strip, group);
        }
        self.sync_navigation(tree);
        self.restyle_work_area();
        if changed {
            ReconcileOutcome::Updated
        } else {
            ReconcileOutcome::Unchanged
        }
    }
    /// Rebuild chrome/container structure while retaining unchanged checked
    /// owners in the same admitted room, including input and unsaved history.
    pub fn rebuild_retaining(
        &mut self,
        tree: &DesktopTree,
        records: &RecordStore,
        resolver: &mut dyn TileResolver,
        chrome: SurfaceChrome,
    ) {
        struct Retained<'a> {
            hosts: HashMap<u64, ui::widget::TileHost>,
            inner: &'a mut dyn TileResolver,
        }
        impl TileResolver for Retained<'_> {
            fn resolve(&mut self, request: TileRequest<'_>) -> crate::resolver::TileResolution {
                if let Some(host) = self.hosts.remove(&request.tab.id) {
                    crate::resolver::TileResolution::Retained(host)
                } else {
                    self.inner.resolve(request)
                }
            }
            fn ready_to_replace(&self, tab: &tilo_desktop_model::Tab) -> bool {
                self.inner.ready_to_replace(tab)
            }
            fn creation_refusal(&self, tab: &tilo_desktop_model::Tab) -> Option<String> {
                self.inner.creation_refusal(tab)
            }
        }
        let mut hosts = HashMap::new();
        if let Some(workspace) = tree
            .active_workspace()
            .filter(|w| self.context.is_some_and(|(_, room)| room == w.room()))
        {
            let keep: Vec<u64> = workspace
                .strips
                .iter()
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs)
                .filter(|tab| {
                    self.tiles.get(&tab.id).is_some_and(|old| {
                        serde_json::to_vec(old).ok() == serde_json::to_vec(&tab.tile).ok()
                    })
                })
                .map(|tab| tab.id)
                .collect();
            for strip in self.strip_ids() {
                if let Some(row) = self.row_mut(strip) {
                    for group in &mut row.groups {
                        for id in &keep {
                            if let Some((_, host)) = group.detach_tab(*id) {
                                hosts.insert(*id, host);
                            }
                        }
                    }
                }
            }
        }
        let reports = ui::component_lifecycle::close_components(self);
        let mut inherited = std::mem::take(&mut self.retained_wakes);
        inherited.push(self.component_wake.clone());
        let failures = std::mem::take(&mut self.failures);
        let mut retained = Retained {
            hosts,
            inner: resolver,
        };
        let mut next = Self::build(tree, records, &mut retained, chrome);
        next.inherit_navigation(self);
        next.style_work_area(self.fullscreen);
        for (_, mut host) in retained.hosts {
            next.close_reports
                .extend(ui::component_lifecycle::close_components(&mut host));
        }
        next.close_reports.extend(reports);
        next.failures.extend(failures);
        *self = next;
        self.structure_dirty = true;
        inherited.retain(|wake| std::sync::Arc::strong_count(wake) > 1);
        self.retained_wakes = inherited;
    }
    /// The backdrop and the window content: everything [`Self::draw_bar`]
    /// paints over. A host drawing the desktop in one scene draws what sits
    /// between the windows and the top bar (notices, floating
    /// surfaces) after this and before [`Self::draw_bar`], the
    /// order [`Self::draw_sections`] composes the same layers in.
    pub fn draw_below_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.external_navigation {
            self.root.draw(scene, transform, text, theme, presence);
            return;
        }
        for path in [&[0, 0][..], &[0, 1, 1]] {
            if let Some(layer) = child_mut(self.root.as_mut(), path) {
                layer.draw(scene, transform, text, theme, presence);
            }
        }
    }

    /// The top bar and the other displays' compact bars, over everything
    /// [`Self::draw_below_bar`] drew: anchored chrome may extend below the
    /// bar's band and must stay on top of window content. With external
    /// navigation the root drew everything already.
    pub fn draw_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.external_navigation {
            return;
        }
        for path in [&[0, 1, 0][..], &[0, 2]] {
            if let Some(layer) = child_mut(self.root.as_mut(), path) {
                layer.draw(scene, transform, text, theme, presence);
            }
        }
    }

    /// Separate recordings preserve native retained scene and capture layering.
    pub fn draw_sections(
        &mut self,
        backdrop: &mut Scene,
        topbar: &mut Scene,
        workspace: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        let bounds =
            self.draw_chrome_sections(backdrop, topbar, transform, text, theme, presence)?;
        self.draw_workspace_area(workspace, transform, text, theme, presence)?;
        Some(bounds)
    }

    /// The backdrop and the bar of [`Self::draw_sections`], without the
    /// workspace's windows: the bounds of the bar, or zero under external
    /// navigation.
    pub fn draw_chrome_sections(
        &mut self,
        backdrop: &mut Scene,
        topbar: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        child_mut(self.root.as_mut(), &[0, 0])?.draw(backdrop, transform, text, theme, presence);
        if self.external_navigation {
            return Some(Rect::ZERO);
        }
        let bar = child_mut(self.root.as_mut(), &[0, 1, 0])?;
        let rect = bar.get_rect();
        bar.draw(topbar, transform, text, theme, presence);
        if let Some(display_chrome) = child_mut(self.root.as_mut(), &[0, 2]) {
            display_chrome.draw(topbar, transform, text, theme, presence);
        }
        Some(Rect::new(rect.x0, rect.y0, rect.x1, rect.y1 + 28.0))
    }

    /// The workspace's windows of [`Self::draw_sections`] alone: what a
    /// workspace switch moves.
    pub fn draw_workspace_area(
        &mut self,
        workspace: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<()> {
        let path: &[usize] = if self.external_navigation {
            &[0, 1, 0]
        } else {
            &[0, 1, 1]
        };
        child_mut(self.root.as_mut(), path)?.draw(workspace, transform, text, theme, presence);
        Some(())
    }
}
impl Widget for DesktopSurface {
    ui::provides!(dyn ui::component_lifecycle::ComponentLifecycle);

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let wake = self.take_component_wake();
        self.root.update(doc, path) || wake
    }
    fn needs_redraw(&self) -> bool {
        self.component_wake
            .load(std::sync::atomic::Ordering::Acquire)
            || self
                .retained_wakes
                .iter()
                .any(|wake| wake.load(std::sync::atomic::Ordering::Acquire))
            || self.root.needs_redraw()
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.hold_transition();
        let node = self.root.build_layout(tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.hold_transition();
        self.root.apply_layout(tree, node, origin);
        let reveal_pending = self.apply_navigation();
        self.end_transition(reveal_pending);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_below_bar(scene, transform, text, theme, presence);
        self.draw_bar(scene, transform, text, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if !*ctx.handled {
            self.observe_navigation_event(event, ctx.scale_factor);
        }
        self.root.handle_event(event, ctx);
    }
    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        self.observe_focus_request(target, tree);
        self.root.focus_requested(target, tree);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.root.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root.children_mut()
    }
    /// The surface stands for its root's node; the root's own children are
    /// laid out under it.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(self.root.as_ref());
        true
    }
    fn name(&self) -> &'static str {
        "DesktopSurface"
    }
    fn get_rect(&self) -> Rect {
        self.root.get_rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        let inner = self.root.take_structure_dirty();
        rebuilt || inner
    }
    fn wants_pointer_capture(&self) -> bool {
        self.root.wants_pointer_capture()
    }
}

impl ui::component_lifecycle::ComponentLifecycle for DesktopSurface {
    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let reports = ui::component_lifecycle::close_components(self.root.as_mut());
        self.close_reports.extend(reports);
        self.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut out = std::mem::take(&mut self.close_reports);
        out.extend(ui::component_lifecycle::take_component_close_reports(
            self.root.as_mut(),
        ));
        out
    }
}

/// The toolkit's width for a persisted window width.
pub fn ui_width(width: WindowWidth) -> ui::widget::WindowWidth {
    match width {
        WindowWidth::Narrow => ui::widget::WindowWidth::Narrow,
        WindowWidth::Normal => ui::widget::WindowWidth::Normal,
        WindowWidth::Wide => ui::widget::WindowWidth::Wide,
        WindowWidth::Fraction(p) => ui::widget::WindowWidth::Fraction(p),
    }
}
