//! The production desktop hierarchy, shared by native and portable hosts.
use crate::resolver::{TileRequest, TileResolver, tile_app_kind};
use crate::{RoomId, StripId, WindowGroup, WorkspaceTree};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use ui::record::RecordStore;
use ui::widget::{
    Column, Container, GroupRow, Stack, StripView, TabDescriptor, Widget, WindowGroup as UiGroup,
    WorkspaceView,
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
pub struct SurfaceChrome {
    pub navigation: NavigationChrome,
    pub topbar: Box<dyn Widget>,
    pub backdrop: Box<dyn Widget>,
    pub empty_workspace: Option<Box<dyn Widget>>,
    pub leading_inset: f64,
    /// Schedule a host frame when a retained application changes asynchronously.
    pub notify: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}
impl Default for SurfaceChrome {
    fn default() -> Self {
        Self {
            navigation: NavigationChrome::InSurface,
            topbar: Box::new(crate::topbar::TopBarChrome::from_tree(
                &WorkspaceTree::default(),
            )),
            backdrop: Box::new(crate::backdrop::DesktopBackdrop::new()),
            empty_workspace: None,
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
    pub(crate) external_navigation: bool,
    column_index: usize,
    pub(crate) navigation: crate::navigation::Navigation,
    root: Box<dyn Widget>,
    component_wake: std::sync::Arc<std::sync::atomic::AtomicBool>,
    retained_wakes: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    wake_sink: std::sync::Arc<dyn Fn() + Send + Sync>,
    context: Option<(crate::WorkspaceId, RoomId)>,
    empty_workspace_visible: Option<bool>,
    rows: HashMap<StripId, usize>,
    order: Vec<StripId>,
    failures: Vec<ui::launch::LaunchError>,
    close_reports: Vec<ui::application::ComponentCloseReport>,
    tiles: HashMap<u64, crate::TileContent>,
    selected_tabs: HashMap<crate::GroupId, crate::TabId>,
    workspace_ids: Vec<crate::WorkspaceId>,
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
pub fn build_group(
    group: &WindowGroup,
    room: RoomId,
    records: &RecordStore,
    resolver: &mut dyn TileResolver,
    failures: &mut Vec<ui::launch::LaunchError>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> UiGroup {
    let tabs = group
        .tabs
        .iter()
        .map(|tab| {
            let resolution = resolver.resolve(TileRequest { room, tab });
            let checked = matches!(
                &resolution,
                crate::resolver::TileResolution::Component(_)
                    | crate::resolver::TileResolution::Retained(_)
            );
            let mut host = resolution.mount(failures, wake.clone());
            if !checked {
                host.bind_record(records.clone(), crate::records::scratch_tab_record(tab.id));
            }
            (
                TabDescriptor {
                    id: tab.id,
                    title: tab.title.clone(),
                    app_kind: Some(tile_app_kind(&tab.tile)),
                },
                host,
            )
        })
        .collect();
    UiGroup::from_hosts(group.id, tabs, group.active_tab, group.width.to_ui())
        .with_maximized(group.maximized)
}
impl DesktopSurface {
    pub fn primary_vertical_scroll(
        &self,
        strip: StripId,
        group: crate::GroupId,
        tab: crate::TabId,
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
        group: crate::GroupId,
        tab: crate::TabId,
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

    pub fn build(
        tree: &WorkspaceTree,
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
        let mut column = Column::new()
            .with_fill_remaining_height()
            .with_vertical_padding(if external_navigation { 0.0 } else { 8.0 })
            .with_horizontal_padding(0.0, 0.0);
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
        let empty_workspace = tree
            .active_workspace()
            .is_some_and(|w| w.strips.iter().all(|s| s.window_groups.is_empty()));
        let empty_workspace_visible = chrome.empty_workspace.as_ref().map(|_| empty_workspace);
        let mut row_offset = if external_navigation { 0 } else { 2 };
        if empty_workspace {
            if let Some(empty) = chrome.empty_workspace {
                column = column.push(empty);
                row_offset += 1;
            }
        }
        let mut rows = HashMap::new();
        let mut order = Vec::new();
        let mut failures = Vec::new();
        if let Some(workspace) = tree.active_workspace() {
            for strip in &workspace.strips {
                let groups = strip
                    .window_groups
                    .iter()
                    .map(|g| {
                        build_group(
                            g,
                            workspace.room,
                            records,
                            resolver,
                            &mut failures,
                            wake_sink.clone(),
                        )
                    })
                    .collect();
                let mut row = GroupRow::new(groups).with_leading_inset(chrome.leading_inset);
                row.set_external_navigation(external_navigation);
                rows.insert(strip.id, row_offset);
                order.push(strip.id);
                row_offset += 1;
                column = column.push(Box::new(row));
            }
        }
        let mut foreground = Column::new().with_no_padding();
        if !external_navigation {
            foreground = foreground.push(chrome.topbar);
        }
        let foreground = foreground.push(Box::new(column));
        let stack = Stack::new()
            .push(chrome.backdrop)
            .push(Box::new(foreground));
        let root = Box::new(
            Container::new()
                .with_no_padding()
                .with_child(Box::new(stack))
                .filling(),
        );
        let mut surface = Self {
            external_navigation,
            column_index: usize::from(!external_navigation),
            navigation: Default::default(),
            root,
            context: tree.active_workspace().map(|w| (w.id, w.room)),
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
        surface
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
    pub fn workspace_context(&self) -> Option<(crate::WorkspaceId, RoomId)> {
        self.context
    }
    pub fn strip_ids(&self) -> Vec<StripId> {
        self.order.clone()
    }
    pub fn row(&self, id: StripId) -> Option<&GroupRow> {
        self.row_at(*self.rows.get(&id)?)
    }
    pub fn row_mut(&mut self, id: StripId) -> Option<&mut GroupRow> {
        self.row_at_mut(*self.rows.get(&id)?)
    }
    fn row_at(&self, index: usize) -> Option<&GroupRow> {
        child(self.root.as_ref(), &[0, 1, self.column_index, index])?
            .as_any()?
            .downcast_ref()
    }
    fn row_at_mut(&mut self, index: usize) -> Option<&mut GroupRow> {
        child_mut(self.root.as_mut(), &[0, 1, self.column_index, index])?
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
        {
            if view.active_index() != self.selected_workspace {
                self.selected_workspace = view.active_index();
                if let Some(id) = self.workspace_ids.get(self.selected_workspace) {
                    out.push(DesktopIntent::Local(LocalViewIntent::Workspace(*id)));
                }
            }
        }
        if let Some(view) = (!self.external_navigation)
            .then(|| child(self.root.as_ref(), &[0, 1, 1, 1]))
            .flatten()
            .and_then(Widget::as_any)
            .and_then(|v| v.downcast_ref::<StripView>())
        {
            if view.active_index() != self.selected_strip {
                self.selected_strip = view.active_index();
                if let Some(id) = self.order.get(self.selected_strip) {
                    out.push(DesktopIntent::Local(LocalViewIntent::Strip(*id)));
                }
            }
        }
        out
    }
    /// Reconcile retained groups and hosts. A changed strip set requires rebuilding with host chrome.
    pub fn reconcile(
        &mut self,
        tree: &WorkspaceTree,
        records: &RecordStore,
        resolver: &mut dyn TileResolver,
    ) -> ReconcileOutcome {
        let Some(workspace) = tree.active_workspace() else {
            return ReconcileOutcome::RebuildRequired;
        };
        if Some((workspace.id, workspace.room)) != self.context {
            return ReconcileOutcome::RebuildRequired;
        }
        if workspace.strips.iter().map(|s| s.id).collect::<Vec<_>>() != self.order {
            return ReconcileOutcome::RebuildRequired;
        }
        let empty = workspace
            .strips
            .iter()
            .all(|strip| strip.window_groups.is_empty());
        if self
            .empty_workspace_visible
            .is_some_and(|visible| visible != empty)
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
                if let Some(group) = row.groups.iter_mut().find(|g| g.id == id) {
                    reports.extend(group.close_components());
                }
                changed |= row.remove_group_by_id(id);
            }
            for (index, group) in strip.window_groups.iter().enumerate() {
                if !row.groups.iter().any(|g| g.id == group.id) {
                    changed = true;
                    let mut fresh = UiGroup::from_hosts(
                        group.id,
                        vec![],
                        group.active_tab,
                        group.width.to_ui(),
                    )
                    .with_maximized(group.maximized);
                    fresh.start_spawn_animation();
                    row.insert_group(fresh, index);
                }
                changed |= row.reorder_group_by_id(group.id, index);
                let Some(live) = row.groups.iter_mut().find(|g| g.id == group.id) else {
                    continue;
                };
                changed |= live.width() != group.width.to_ui() || live.maximized != group.maximized;
                live.set_width(group.width.to_ui());
                live.maximized = group.maximized;
                let stale: Vec<_> = live
                    .tabs()
                    .iter()
                    .filter(|t| !group.tabs.iter().any(|s| s.id == t.id))
                    .map(|t| t.id)
                    .collect();
                for id in stale {
                    if let Some((_, mut host)) = live.detach_tab(id) {
                        reports.extend(host.close_components());
                        changed = true;
                    }
                }
                for (index, tab) in group.tabs.iter().enumerate() {
                    let descriptor = TabDescriptor {
                        id: tab.id,
                        title: tab.title.clone(),
                        app_kind: Some(tile_app_kind(&tab.tile)),
                    };
                    if let Some(from) = live.tabs().iter().position(|t| t.id == tab.id) {
                        changed |= live.reorder_tab(from, index);
                        changed |= live.update_tab_descriptor(descriptor.clone());
                        let refusal = resolver.creation_refusal(tab);
                        let replace = tiles.get(&tab.id) != Some(&tab.tile)
                            || (refusal.is_none()
                                && live
                                    .host_for_tab_mut(tab.id)
                                    .is_some_and(|h| h.record_creation_refused()));
                        if replace && resolver.ready_to_replace(tab) {
                            if let Some((_, mut old)) = live.detach_tab(tab.id) {
                                reports.extend(old.close_components());
                            }
                        } else {
                            if let Some(host) = live.host_for_tab_mut(tab.id) {
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
                    }
                    let mut retained = moved.remove(&tab.id);
                    let changed_identity = tiles.get(&tab.id) != Some(&tab.tile);
                    if retained.is_some() && changed_identity && resolver.ready_to_replace(tab) {
                        if let Some((_, mut old)) = retained.take() {
                            reports.extend(old.close_components());
                        }
                    }
                    // A native surface may still be waiting for its first frame. Keep the
                    // old identity snapshot so a later reconciliation retries replacement.
                    let deferred_replacement = retained.is_some() && changed_identity;
                    let mut host = if let Some((_, host)) = retained {
                        host
                    } else {
                        let resolution = resolver.resolve(TileRequest {
                            room: workspace.room,
                            tab,
                        });
                        let checked = matches!(
                            &resolution,
                            crate::resolver::TileResolution::Component(_)
                                | crate::resolver::TileResolution::Retained(_)
                        );
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
        }
        for (_, (_, mut host)) in moved {
            reports.extend(host.close_components());
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
        self.sync_navigation(tree);
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
        tree: &WorkspaceTree,
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
            fn ready_to_replace(&self, tab: &crate::Tab) -> bool {
                self.inner.ready_to_replace(tab)
            }
            fn creation_refusal(&self, tab: &crate::Tab) -> Option<String> {
                self.inner.creation_refusal(tab)
            }
        }
        let mut hosts = HashMap::new();
        if let Some(workspace) = tree
            .active_workspace()
            .filter(|w| self.context.is_some_and(|(_, room)| room == w.room))
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
        let reports = self.close_components();
        let mut inherited = std::mem::take(&mut self.retained_wakes);
        inherited.push(self.component_wake.clone());
        let failures = std::mem::take(&mut self.failures);
        let mut retained = Retained {
            hosts,
            inner: resolver,
        };
        let mut next = Self::build(tree, records, &mut retained, chrome);
        next.inherit_navigation(self);
        for (_, mut host) in retained.hosts {
            next.close_reports.extend(host.close_components());
        }
        next.close_reports.extend(reports);
        next.failures.extend(failures);
        *self = next;
        self.structure_dirty = true;
        inherited.retain(|wake| std::sync::Arc::strong_count(wake) > 1);
        self.retained_wakes = inherited;
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
        child_mut(self.root.as_mut(), &[0, 0])?.draw(backdrop, transform, text, theme, presence);
        if self.external_navigation {
            child_mut(self.root.as_mut(), &[0, 1, 0])?
                .draw(workspace, transform, text, theme, presence);
            return Some(Rect::ZERO);
        }
        let bar = child_mut(self.root.as_mut(), &[0, 1, 0])?;
        let rect = bar.get_rect();
        bar.draw(topbar, transform, text, theme, presence);
        child_mut(self.root.as_mut(), &[0, 1, 1])?
            .draw(workspace, transform, text, theme, presence);
        Some(Rect::new(rect.x0, rect.y0, rect.x1, rect.y1 + 28.0))
    }
}
impl Widget for DesktopSurface {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        self.root.sync(doc, path);
    }
    fn update(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
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
        let node = self.root.build_layout(tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.root.apply_layout(tree, node, origin);
        self.apply_navigation();
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.root.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        tree: &TaffyTree<()>,
    ) {
        if !*handled {
            self.observe_navigation_event(event, scale);
        }
        self.root.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, tree,
        );
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
    fn name(&self) -> &str {
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
    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let reports = self.root.close_components();
        self.close_reports.extend(reports);
        self.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut out = std::mem::take(&mut self.close_reports);
        out.extend(self.root.take_component_close_reports());
        out
    }
}
