//! Portable retained host around the actual canonical DesktopSurface.
use crate::desktop::{self, Admission};
use automerge::{AutoCommit, ObjId};
use qos_desktop::{
    composition::{DesktopSurface, ReconcileOutcome, SurfaceChrome},
    intents::{DesktopIntent, LocalViewIntent},
    resolver::{ApplicationCatalogTileResolver, ComponentContextSource, TileRequest},
    *,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
    sync::Arc,
};
use taffy::{NodeId, TaffyTree};
use ui::{TextRenderer, Theme, presence::PresenceCtx};
use ui::{
    application::{
        ApplicationCatalog, ApplicationDefinition, ComponentCloseReport, HostContext, LaunchTarget,
        RestoreInput,
    },
    application_view::InstanceViewStore,
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordOwners, RecordStore},
    widget::Widget,
};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

#[derive(Clone)]
pub(crate) struct OwnedRecord {
    pub app: String,
    pub store: RecordStore,
    pub checkout: RecordCheckout,
}
fn active_route(tree: &WorkspaceTree) -> Option<crate::desktop_navigation::Route> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tree.active)?;
    let strip_id = *tree.active_strip.get(&workspace.id)?;
    let strip = workspace.strips.iter().find(|strip| strip.id == strip_id)?;
    let group_id = *tree.active_group.get(&strip.id)?;
    let group = strip
        .window_groups
        .iter()
        .find(|group| group.id == group_id)?;
    Some(crate::desktop_navigation::Route {
        workspace: workspace.id.0.to_string(),
        strip: strip.id.0.to_string(),
        group: group.id.to_string(),
        tab: group.active_tab.to_string(),
    })
}
#[derive(Clone)]
pub(crate) struct DesktopHandle {
    pub tree: Rc<RefCell<WorkspaceTree>>,
    pub permissions: Rc<RefCell<ui::permissions::PermissionContext>>,
    pub records: Rc<RefCell<BTreeMap<(String, String), OwnedRecord>>>,
    pub images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    pub errors: Rc<RefCell<Vec<String>>>,
    pub session_requests:
        Rc<RefCell<Vec<(uuid::Uuid, qos_desktop::session_commands::BoundCommand)>>>,
    inbox: Rc<RefCell<Option<(Admission, ui::permissions::PermissionContext)>>>,
    pub navigation: Rc<RefCell<serde_json::Value>>,
    pub navigation_revision: Rc<std::cell::Cell<u64>>,
    pub activation_sequence: Rc<std::cell::Cell<u64>>,
    pending_local_activation: Rc<RefCell<Option<crate::desktop_navigation::Route>>>,
    pub navigation_command: Rc<RefCell<Option<crate::desktop_navigation::Command>>>,
    pub navigation_result: Rc<RefCell<Option<Result<Option<String>, String>>>>,
    pub(crate) generation: String,
    pub search: Rc<RefCell<crate::system_search::SearchState>>,
    history_available: Rc<std::cell::Cell<bool>>,
}
impl DesktopHandle {
    pub(crate) fn check_local_activation(
        &self,
        frame: &ui::headless::FrameHarness,
    ) -> Result<(), String> {
        frame.check_app_activation()
    }
    pub(crate) fn perform_semantic_input(
        &self,
        frame: &mut ui::headless::FrameHarness,
        path: &str,
        target: Option<&str>,
        action: &ui::a11y::Action,
    ) -> Result<(), String> {
        if matches!(
            action,
            ui::a11y::Action::SetValue(_) | ui::a11y::Action::SetSelection { .. }
        ) && frame.inspection().keyboard_focus_owner.as_deref() != Some(path)
        {
            return Err("semantic text input does not own keyboard focus".into());
        }
        let active = active_route(&self.tree.borrow())
            .and_then(|route| route.ids().ok())
            .map(|route| route.3);
        let owner = frame.semantic_owner_instance(path);
        let switches_app = target.is_some_and(|target| target.starts_with("tab:"))
            || owner.is_some_and(|owner| Some(owner) != active);
        if switches_app {
            self.check_local_activation(frame)?;
        }
        frame.perform_semantic_named(path, target, action)?;
        self.finish_local_activation(frame);
        Ok(())
    }
    pub(crate) fn finish_local_activation(&self, frame: &mut ui::headless::FrameHarness) {
        let Some(route) = self.pending_local_activation.borrow_mut().take() else {
            return;
        };
        if self.check_local_activation(frame).is_err()
            || active_route(&self.tree.borrow()).as_ref() != Some(&route)
        {
            return;
        }
        let Ok((_, _, _, instance)) = route.ids() else {
            return;
        };
        frame.prepare_app_activation(instance);
        frame.activate_app_focus(instance);
        self.activation_sequence
            .set(self.activation_sequence.get().wrapping_add(1));
    }
    pub(crate) fn invalidate_history(&self) {
        self.history_available.set(false);
        // A pre-revocation admission must never restore a withdrawn read grant.
        self.inbox.borrow_mut().take();
    }
    /// The single accepted-activation boundary shared by toolbar and Search.
    pub fn navigate(
        &self,
        frame: &mut ui::headless::FrameHarness,
        command: crate::desktop_navigation::Command,
    ) -> Result<Option<String>, String> {
        self.pending_local_activation.borrow_mut().take();
        // Admit queued topology before validating an exact route.
        frame.relayout();
        self.pending_local_activation.borrow_mut().take();
        let previous = active_route(&self.tree.borrow());
        let target = match &command {
            crate::desktop_navigation::Command::Select { route, .. } => {
                if command.generation() != self.generation {
                    return Err("Stale desktop surface".into());
                }
                let (w, s, g, t) = route.ids()?;
                if !self
                    .tree
                    .borrow()
                    .workspaces
                    .iter()
                    .filter(|v| v.id == w)
                    .flat_map(|w| &w.strips)
                    .filter(|v| v.id == s)
                    .flat_map(|s| &s.window_groups)
                    .filter(|v| v.id == g)
                    .flat_map(|g| &g.tabs)
                    .any(|v| v.id == t)
                {
                    return Err("Application route unavailable".into());
                }
                Some(route.clone())
            }
            crate::desktop_navigation::Command::Workspace { workspace, .. } => {
                if command.generation() != self.generation {
                    return Err("Stale desktop surface".into());
                }
                let workspace = workspace
                    .parse::<u64>()
                    .ok()
                    .filter(|id| id.to_string() == *workspace)
                    .map(WorkspaceId)
                    .ok_or("Invalid navigation identity")?;
                let mut tree = self.tree.borrow().clone();
                DesktopRoot::local(&mut tree, LocalViewIntent::Workspace(workspace));
                active_route(&tree)
            }
            crate::desktop_navigation::Command::Strip {
                workspace, strip, ..
            } => {
                if command.generation() != self.generation {
                    return Err("Stale desktop surface".into());
                }
                let mut tree = self.tree.borrow().clone();
                let workspace = tree
                    .workspaces
                    .iter()
                    .find(|item| item.id.0.to_string() == *workspace)
                    .ok_or("Workspace unavailable")?
                    .id;
                let strip = tree
                    .workspaces
                    .iter()
                    .find(|item| item.id == workspace)
                    .and_then(|item| {
                        item.strips
                            .iter()
                            .find(|item| item.id.0.to_string() == *strip)
                    })
                    .ok_or("Strip unavailable")?
                    .id;
                DesktopRoot::local(&mut tree, LocalViewIntent::Workspace(workspace));
                DesktopRoot::local(&mut tree, LocalViewIntent::Strip(strip));
                active_route(&tree)
            }
            _ => None,
        };
        if target.is_some() {
            frame.check_app_activation()?;
        }
        let activates = matches!(&command, crate::desktop_navigation::Command::Select { .. })
            || target
                .as_ref()
                .and_then(|route| route.ids().ok())
                .map(|ids| ids.3)
                != previous
                    .as_ref()
                    .and_then(|route| route.ids().ok())
                    .map(|ids| ids.3);
        if activates {
            if let Some(instance) = target
                .as_ref()
                .and_then(|route| route.ids().ok())
                .map(|ids| ids.3)
            {
                frame.prepare_app_activation(instance);
            }
        }
        *self.navigation_result.borrow_mut() = None;
        *self.navigation_command.borrow_mut() = Some(command);
        frame.relayout();
        let result = self
            .navigation_result
            .borrow_mut()
            .take()
            .ok_or("Desktop did not accept navigation")??;
        if target
            .as_ref()
            .is_some_and(|route| active_route(&self.tree.borrow()).as_ref() != Some(route))
        {
            // Workspace selection is deferred so outgoing focus can be captured first.
            frame.relayout();
        }
        self.pending_local_activation.borrow_mut().take();
        if let Some(instance) = target
            .filter(|route| activates && active_route(&self.tree.borrow()).as_ref() == Some(route))
            .and_then(|route| route.ids().ok())
            .map(|ids| ids.3)
        {
            frame.activate_app_focus(instance);
            self.activation_sequence
                .set(self.activation_sequence.get().wrapping_add(1));
        }
        Ok(result)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn admit(
        &self,
        state: &qos_peer_state::StatePeer,
        permissions: ui::permissions::PermissionContext,
    ) -> Result<(), String> {
        let admission = desktop::admission(state)?;
        if !admission.complete() {
            return Err("waiting for required canonical desktop records".into());
        }
        *self.inbox.borrow_mut() = Some((admission, permissions));
        Ok(())
    }
}
struct Contexts {
    owners: RecordOwners,
    views: Rc<InstanceViewStore>,
    routes: BTreeMap<u64, String>,
    room: String,
    device: String,
    author: String,
    permissions: Rc<RefCell<ui::permissions::PermissionContext>>,
    handle: DesktopHandle,
}
impl ComponentContextSource for Contexts {
    fn checkout(
        &self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
    ) -> Result<RecordCheckout, ui::launch::LaunchError> {
        desktop::checkout(definition, request.tab.id, &self.routes)
            .map_err(ui::launch::LaunchError::Operation)
    }
    fn prepare(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
    ) -> Result<(HostContext, RestoreInput), ui::launch::LaunchError> {
        let route_app_id =
            qos_desktop::resolver::canonical_app_id(&request.tab.tile).ok_or_else(|| {
                ui::launch::LaunchError::Operation("application identity unavailable".into())
            })?;
        let (durable_app_id, _) =
            desktop::installed_application(&route_app_id).ok_or_else(|| {
                ui::launch::LaunchError::Operation("application is not installed".into())
            })?;
        let store = self
            .owners
            .open(durable_app_id, &self.room)
            .map_err(|e| ui::launch::LaunchError::Operation(e.to_string()))?;
        let checkout = self.checkout(definition, request)?;
        let restore = self
            .views
            .load(request.tab.id)
            .map_err(|e| ui::launch::LaunchError::Operation(e.to_string()))?
            .map_or(RestoreInput::None, RestoreInput::Local);
        let context = HostContext::prepare(
            definition,
            store.clone(),
            self.views.clone(),
            self.device.clone(),
            request.tab.id,
            self.permissions.clone(),
            ui::permissions::UserId(self.author.clone()),
            tilo_actions::ActionContext::default(),
            target,
        )?;
        self.handle.records.borrow_mut().insert(
            (durable_app_id.into(), checkout.id.as_str().into()),
            OwnedRecord {
                app: durable_app_id.into(),
                store,
                checkout,
            },
        );
        Ok((context, restore))
    }
}
struct ChromeSource {
    tree: Rc<RefCell<WorkspaceTree>>,
    intents: Rc<RefCell<Vec<qos_desktop::chrome::ChromeIntent>>>,
}
impl qos_desktop::chrome::DesktopChromeSource for ChromeSource {
    fn snapshot(&self) -> qos_desktop::chrome::DesktopChromeSnapshot {
        qos_desktop::chrome::DesktopChromeSnapshot::default()
    }
    fn tree(&self) -> WorkspaceTree {
        self.tree.borrow().clone()
    }
    fn intent(&self, intent: qos_desktop::chrome::ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct LocalView {
    active: WorkspaceId,
    strips: HashMap<WorkspaceId, StripId>,
    groups: HashMap<StripId, GroupId>,
    tabs: Vec<(GroupId, TabId, WindowWidth, bool)>,
}
impl LocalView {
    fn from_tree(tree: &WorkspaceTree) -> Self {
        Self {
            active: tree.active,
            strips: tree.active_strip.clone(),
            groups: tree.active_group.clone(),
            tabs: tree
                .workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .map(|g| (g.id, g.active_tab, g.width, g.maximized))
                .collect(),
        }
    }
    fn apply(&self, tree: &mut WorkspaceTree) {
        if tree.workspaces.iter().any(|w| w.id == self.active) {
            tree.active = self.active;
        }
        for w in &mut tree.workspaces {
            if let Some(id) = self
                .strips
                .get(&w.id)
                .filter(|id| w.strips.iter().any(|s| s.id == **id))
            {
                tree.active_strip.insert(w.id, *id);
            }
            for s in &mut w.strips {
                if let Some(id) = self
                    .groups
                    .get(&s.id)
                    .filter(|id| s.window_groups.iter().any(|g| g.id == **id))
                {
                    tree.active_group.insert(s.id, *id);
                }
                for g in &mut s.window_groups {
                    if let Some((_, tab, width, maximized)) =
                        self.tabs.iter().find(|(id, ..)| *id == g.id)
                    {
                        if g.tabs.iter().any(|t| t.id == *tab) {
                            g.active_tab = *tab;
                        }
                        g.width = *width;
                        g.maximized = *maximized;
                    }
                }
            }
        }
    }
}
pub(crate) struct DesktopRoot {
    source_heads: Vec<[u8; 32]>,
    admitted_room: AutoCommit,
    required_records: Vec<desktop::Requirement>,
    unsupported_apps: Vec<String>,
    pub(crate) activity: Option<crate::desktop_activity::BrowserActivity>,
    pub(crate) activity_close_reason: Option<crate::desktop_activity::ActivityCloseReason>,
    pending_view_request: Option<activity::WorkspaceViewRequest>,
    surface_node: Option<NodeId>,
    activity_node: Option<NodeId>,
    surface: DesktopSurface,
    handle: DesktopHandle,
    resolver: ApplicationCatalogTileResolver<Contexts>,
    fallback: RecordStore,
    chrome: Rc<ChromeSource>,
    chrome_intents: Rc<RefCell<Vec<qos_desktop::chrome::ChromeIntent>>>,
    storage: PortableStorage,
    view_key: StorageKey,
    notify: Arc<dyn Fn() + Send + Sync>,
    reports: Vec<ComponentCloseReport>,
    failures: Vec<ui::launch::LaunchError>,
    dirty: bool,
    view_changed: bool,
}
impl DesktopRoot {
    fn queue_activation_after_route_change(
        handle: &DesktopHandle,
        tree: &WorkspaceTree,
        previous: Option<crate::desktop_navigation::Route>,
    ) {
        let selected = active_route(tree);
        let instance = |route: &crate::desktop_navigation::Route| {
            route.ids().ok().map(|(_, _, _, instance)| instance)
        };
        if previous.as_ref().and_then(instance) != selected.as_ref().and_then(instance) {
            *handle.pending_local_activation.borrow_mut() = selected;
        }
    }

    fn apply_local(handle: &DesktopHandle, tree: &mut WorkspaceTree, intent: LocalViewIntent) {
        let previous = active_route(tree);
        Self::local(tree, intent);
        Self::queue_activation_after_route_change(handle, tree, previous);
    }

    fn switch_workspace(handle: &DesktopHandle, tree: &mut WorkspaceTree, delta: i32) {
        let previous = active_route(tree);
        if let Some(index) = tree.workspaces.iter().position(|w| w.id == tree.active) {
            let next =
                (index as i64 + i64::from(delta)).rem_euclid(tree.workspaces.len() as i64) as usize;
            tree.active = tree.workspaces[next].id;
        }
        Self::queue_activation_after_route_change(handle, tree, previous);
    }
    pub fn new(
        admission: Admission,
        storage: PortableStorage,
        room: &str,
        device: &str,
        author: &str,
        instance: u64,
        permissions: ui::permissions::PermissionContext,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(Self, DesktopHandle), String> {
        if !admission.complete() {
            return Err("required desktop records are not admitted".into());
        }
        let mut tree = admission.tree;
        let view_key = StorageKey::View {
            workspace: format!("desktop:{room}"),
            instance,
        };
        if let Some(saved) = storage.get(&view_key) {
            let view: LocalView = serde_json::from_slice(&saved.bytes)
                .map_err(|e| format!("local desktop view: {e}"))?;
            view.apply(&mut tree);
        }
        let handle = DesktopHandle {
            tree: Rc::new(RefCell::new(tree)),
            permissions: Rc::new(RefCell::new(permissions)),
            records: Rc::default(),
            images: Rc::default(),
            errors: Rc::default(),
            session_requests: Rc::default(),
            inbox: Rc::default(),
            navigation: Rc::new(RefCell::new(serde_json::Value::Null)),
            navigation_revision: Rc::default(),
            activation_sequence: Rc::default(),
            pending_local_activation: Rc::default(),
            navigation_command: Rc::default(),
            navigation_result: Rc::default(),
            generation: uuid::Uuid::new_v4().to_string(),
            search: Rc::default(),
            history_available: Rc::new(std::cell::Cell::new(true)),
        };
        let chrome_intents: Rc<RefCell<Vec<qos_desktop::chrome::ChromeIntent>>> = Rc::default();
        let chrome = Rc::new(ChromeSource {
            tree: handle.tree.clone(),
            intents: chrome_intents.clone(),
        });
        let mut owners = RecordOwners::portable(storage.clone());
        let fallback = owners
            .open("dev.qos.desktop", room)
            .map_err(|e| e.to_string())?;
        let views =
            Rc::new(InstanceViewStore::portable(storage.clone(), room).map_err(|e| e.to_string())?);
        let mut catalog = ApplicationCatalog::new();
        for definition in desktop::definitions() {
            catalog.register(definition).map_err(|e| e.to_string())?;
        }
        let mut resolver = ApplicationCatalogTileResolver {
            catalog,
            host: Contexts {
                owners,
                views,
                routes: admission.routes,
                room: room.into(),
                device: device.into(),
                author: author.into(),
                permissions: handle.permissions.clone(),
                handle: handle.clone(),
            },
        };
        let tree = handle.tree.borrow();
        let surface = ui::ImageStore::with_host_factory(Self::image_factory(&handle), || {
            DesktopSurface::build(
                &tree,
                &fallback,
                &mut resolver,
                Self::chrome(&tree, chrome.clone(), notify.clone()),
            )
        });
        drop(tree);
        let mut root = Self {
            source_heads: admission.source_heads,
            admitted_room: admission.admitted_room,
            required_records: admission.required,
            unsupported_apps: admission.unsupported,
            activity: None,
            activity_close_reason: None,
            pending_view_request: None,
            surface_node: None,
            activity_node: None,
            surface,
            handle: handle.clone(),
            resolver,
            fallback,
            chrome,
            chrome_intents,
            storage,
            view_key,
            notify,
            reports: vec![],
            failures: vec![],
            dirty: true,
            view_changed: false,
        };
        root.collect_failures();
        root.refresh_navigation(true);
        Ok((root, handle))
    }
    fn image_factory(handle: &DesktopHandle) -> Rc<dyn Fn() -> ui::ImageStore> {
        let images = handle.images.clone();
        Rc::new(move || {
            let (store, loader) = ui::ImageStore::host_driven();
            images.borrow_mut().push(loader);
            store
        })
    }
    fn chrome(
        tree: &WorkspaceTree,
        source: Rc<ChromeSource>,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> SurfaceChrome {
        SurfaceChrome {
            navigation: qos_desktop::composition::NavigationChrome::External,
            topbar: Box::new(
                qos_desktop::topbar::TopBarChrome::from_tree(tree).with_chrome(source),
            ),
            notify: Some(notify),
            ..SurfaceChrome::default()
        }
    }
    fn collect_failures(&mut self) {
        for error in self.surface.take_setup_failures() {
            self.handle.errors.borrow_mut().push(error.to_string());
            self.failures.push(error);
        }
    }
    fn local(tree: &mut WorkspaceTree, intent: LocalViewIntent) {
        match intent {
            LocalViewIntent::Workspace(id) => {
                if tree.workspaces.iter().any(|w| w.id == id) {
                    tree.active = id;
                }
            }
            LocalViewIntent::Strip(id) => {
                if let Some(w) = tree
                    .workspaces
                    .iter()
                    .find(|w| w.id == tree.active && w.strips.iter().any(|s| s.id == id))
                {
                    tree.active_strip.insert(w.id, id);
                }
            }
            LocalViewIntent::Group { strip, group } => {
                if tree
                    .workspaces
                    .iter()
                    .filter(|w| w.id == tree.active)
                    .flat_map(|w| &w.strips)
                    .any(|s| s.id == strip && s.window_groups.iter().any(|g| g.id == group))
                {
                    tree.active_strip.insert(tree.active, strip);
                    tree.active_group.insert(strip, group);
                }
            }
            other => {
                for w in &mut tree.workspaces {
                    for s in &mut w.strips {
                        for g in &mut s.window_groups {
                            match other {
                                LocalViewIntent::Tab { group, tab }
                                    if group == g.id && g.tabs.iter().any(|t| t.id == tab) =>
                                {
                                    g.active_tab = tab;
                                    if w.id == tree.active {
                                        tree.active_strip.insert(w.id, s.id);
                                        tree.active_group.insert(s.id, g.id);
                                    }
                                }
                                LocalViewIntent::Width { group, width } if group == g.id => {
                                    g.width = width
                                }
                                LocalViewIntent::Maximized { group, maximized }
                                    if group == g.id =>
                                {
                                    g.maximized = maximized
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
    pub(crate) fn activity_snapshot(&self) -> Result<activity::ActivitySnapshot, String> {
        if !self.handle.history_available.get() {
            return Err("Workspace history admission unavailable".into());
        }
        if !self
            .handle
            .permissions
            .borrow()
            .check(&ui::permissions::PermissionScope::workspace(
                ui::permissions::Verb::Read,
            ))
        {
            return Err("Workspace history access unavailable".into());
        }
        activity::ActivitySnapshot::new(
            self.handle.tree.borrow().clone(),
            self.admitted_room.clone(),
        )
    }
    fn close_activity(&mut self, reason: crate::desktop_activity::ActivityCloseReason) {
        if let Some(mut activity) = self.activity.take() {
            activity.finish();
            self.activity_close_reason = Some(reason);
            self.dirty = true;
        }
    }
    /// External operations are inert until update: a frame must capture its
    /// current focus before any widget referenced by the layout map is retired.
    pub(crate) fn queue_view_request(
        &mut self,
        request: activity::WorkspaceViewRequest,
    ) -> Result<(), String> {
        request.validate(&self.handle.tree.borrow())?;
        if let activity::WorkspaceViewRequest::OpenActivity { workspace, .. } = &request {
            if self.handle.tree.borrow().active != *workspace {
                return Err("Workspace is no longer active".into());
            }
            self.activity_snapshot()?;
        }
        self.pending_view_request = Some(request);
        self.dirty = true;
        (self.notify)();
        Ok(())
    }
    // Only call during update, after the runner has captured outgoing focus.
    pub(crate) fn apply_view_request(
        &mut self,
        request: activity::WorkspaceViewRequest,
    ) -> Result<(), String> {
        request.validate(&self.handle.tree.borrow())?;
        match request {
            activity::WorkspaceViewRequest::Select { workspace } => {
                if self.handle.tree.borrow().active != workspace {
                    self.close_activity(
                        crate::desktop_activity::ActivityCloseReason::WorkspaceChanged,
                    );
                }
                Self::apply_local(
                    &self.handle,
                    &mut self.handle.tree.borrow_mut(),
                    LocalViewIntent::Workspace(workspace),
                );
                self.view_changed = true;
            }
            activity::WorkspaceViewRequest::OpenActivity { workspace, view } => {
                if self.handle.tree.borrow().active != workspace {
                    return Err("Workspace is no longer active".into());
                }
                let snapshot = self.activity_snapshot()?;
                self.close_activity(crate::desktop_activity::ActivityCloseReason::Dismissed);
                self.activity_close_reason = None;
                let mut activity = crate::desktop_activity::BrowserActivity::new(
                    snapshot,
                    view,
                    self.storage.clone(),
                );
                activity.refresh(
                    crate::desktop_activity::capture_records(
                        &self.required_records,
                        &self.handle.records.borrow(),
                    ),
                    &self.unsupported_apps,
                );
                self.activity = Some(activity);
            }
            activity::WorkspaceViewRequest::CloseActivity { workspace } => {
                if self
                    .activity
                    .as_ref()
                    .is_some_and(|a| a.workspace == workspace)
                {
                    self.close_activity(crate::desktop_activity::ActivityCloseReason::Dismissed);
                }
            }
        }
        self.dirty = true;
        (self.notify)();
        Ok(())
    }
    fn refresh_activity(&mut self, admission_changed: bool) {
        use crate::desktop_activity::ActivityCloseReason;
        let Some(activity) = &self.activity else {
            return;
        };
        let close = if activity.workspace != self.handle.tree.borrow().active {
            Some(ActivityCloseReason::WorkspaceChanged)
        } else if self.activity_snapshot().is_err() {
            Some(ActivityCloseReason::AccessChanged)
        } else if activity.close_requested {
            Some(ActivityCloseReason::Dismissed)
        } else {
            None
        };
        if let Some(reason) = close {
            self.close_activity(reason);
            return;
        }
        if admission_changed {
            let snapshot = self
                .activity_snapshot()
                .expect("validated admitted history");
            self.activity.as_mut().unwrap().snapshot = snapshot;
        }
        let records = crate::desktop_activity::capture_records(
            &self.required_records,
            &self.handle.records.borrow(),
        );
        let activity = self.activity.as_mut().unwrap();
        activity.refresh(records, &self.unsupported_apps);
        if let Some(target) = activity.open_target.take() {
            // Open only an existing admitted app/record route. Never launch an
            // application or replay an effect from a historical row.
            let destination = self
                .handle
                .tree
                .borrow()
                .workspaces
                .iter()
                .filter(|w| w.id == activity.workspace)
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| g.tabs.iter().map(move |t| (g.id, t)))
                .find_map(|(group, tab)| {
                    let app = qos_desktop::resolver::canonical_app_id(&tab.tile)?;
                    if target.app_id.as_deref() != Some(&app) {
                        return None;
                    }
                    let checkout = desktop::checkout(
                        desktop::definition(&app)?,
                        tab.id,
                        &self.resolver.host.routes,
                    )
                    .ok()?;
                    (checkout.id.as_str() == target.id && checkout.kind.as_str() == target.kind)
                        .then_some((group, tab.id))
                });
            if let Some((group, tab)) = destination {
                self.close_activity(ActivityCloseReason::ResourceOpened);
                Self::local(
                    &mut self.handle.tree.borrow_mut(),
                    LocalViewIntent::Tab { group, tab },
                );
                self.view_changed = true;
            } else {
                self.handle
                    .errors
                    .borrow_mut()
                    .push("Historical resource is not currently available".into());
            }
        }
    }

    fn active_route(&self) -> Option<crate::desktop_navigation::Route> {
        let tree = self.handle.tree.borrow();
        let w = tree.active_workspace()?;
        let s = w
            .strips
            .iter()
            .find(|s| Some(&s.id) == tree.active_strip.get(&w.id))
            .or_else(|| w.strips.first())?;
        let g = tree
            .resolve_active_group(s.id, |_| false)
            .and_then(|id| s.window_groups.iter().find(|g| g.id == id))
            .or_else(|| s.window_groups.first())?;
        Some(crate::desktop_navigation::Route {
            workspace: w.id.0.to_string(),
            strip: s.id.0.to_string(),
            group: g.id.to_string(),
            tab: g.active_tab.to_string(),
        })
    }
    fn history_target(&self) -> Option<String> {
        let route = self.active_route()?;
        let (_, strip, group, tab) = route.ids().ok()?;
        self.surface
            .row(strip)?
            .groups
            .iter()
            .find(|g| g.id == group && g.active_tab_id() == tab)?
            .active_host()?
            .document_history_target()
    }
    fn refresh_navigation(&self, topology: bool) {
        use serde_json::json;
        let active = self.active_route();
        self.handle.search.borrow_mut().refresh(
            &self.handle.generation,
            &self.source_heads,
            &self.handle.tree.borrow(),
            &self.resolver.host.routes,
            active.as_ref(),
        );
        let mut snapshot = self.handle.navigation.borrow_mut();
        let mut changed = false;
        if topology || snapshot.is_null() {
            let tree = self.handle.tree.borrow();
            let entries: Vec<_> = tree.workspaces.iter().flat_map(|w| w.strips.iter().flat_map(move |s| s.window_groups.iter().flat_map(move |g| g.tabs.iter().map(move |t| json!({"route":{"workspace":w.id.0.to_string(),"strip":s.id.0.to_string(),"group":g.id.to_string(),"tab":t.id.to_string()},"label":t.title,"stripLabel":s.name,"workspaceLabel":w.name}))))).collect();
            let workspaces: Vec<_> = tree.workspaces.iter().map(|w| json!({"id":w.id.0.to_string(),"label":w.name,"room":w.room.to_string(),"owner":w.owner,"strips":w.strips.iter().map(|s| json!({"id":s.id.0.to_string(),"label":s.name})).collect::<Vec<_>>()})).collect();
            let group = active
                .as_ref()
                .and_then(|r| r.ids().ok())
                .and_then(|(_, s, g, _)| {
                    tree.workspaces
                        .iter()
                        .flat_map(|w| &w.strips)
                        .find(|v| v.id == s)?
                        .window_groups
                        .iter()
                        .find(|v| v.id == g)
                });
            let applications = desktop::installed_applications()
                .iter()
                .map(|(app, definition)| json!({"id":app,"label":definition.title}))
                .collect::<Vec<_>>();
            let next = json!({"version":1,"generation":self.handle.generation,"entries":entries,"workspaces":workspaces,"applications":applications,"activeWorkspace":tree.active.0.to_string(),"activeStrip":tree.active_workspace().and_then(|w| tree.active_strip.get(&w.id).copied().or_else(||w.strips.first().map(|s|s.id))).map(|s|s.0.to_string()),"active":active,"widthPermille":group.map(|g|g.width.permille()),"maximized":group.is_some_and(|g|g.maximized),"capabilities":{"select":true,"width":true,"maximize":true,"strips":true,"commands":true,"globalSearch":true,"close":true},"history":snapshot.get("history").cloned().unwrap_or(serde_json::Value::Null),"scroll":snapshot.get("scroll").cloned().unwrap_or(serde_json::Value::Null),"desktopPan":snapshot.get("desktopPan").cloned().unwrap_or(serde_json::Value::Null)});
            if *snapshot != next {
                *snapshot = next;
                changed = true;
            }
        }
        let scroll = active.as_ref().and_then(|r| {
            let (_,s,g,t)=r.ids().ok()?;
            let region=self.surface.primary_vertical_scroll(s,g,t)?;
            let owner=region.owner_generation?;
            let m=region.metrics;
            if ui::widget::PrimaryScrollAction::Start.offset(m).is_none() { return None; }
            Some(json!({"region":format!("{}/{}/{}/{}/{}/{}/{}",self.handle.generation,r.workspace,r.strip,r.group,r.tab,owner,region.key),"offset":m.offset.clamp(0.0,m.max_scroll()),"viewport":m.viewport,"content":m.content,"max":m.max_scroll()}))
        }).unwrap_or(serde_json::Value::Null);
        if snapshot["scroll"] != scroll {
            snapshot["scroll"] = scroll;
            changed = true;
        }
        let pan = {
            let tree = self.handle.tree.borrow();
            tree.active_workspace().and_then(|w| {
                let strip = tree.active_strip.get(&w.id).copied().or_else(|| w.strips.first().map(|s| s.id))?;
                let row = self.surface.row(strip)?;
                let viewport = row.get_rect().width();
                let maximized = row.groups.iter().any(|g| g.maximized);
                let max = if maximized { 0.0 } else { row.max_scroll_x() };
                Some(json!({"workspace":w.id.0.to_string(),"strip":strip.0.to_string(),"offset":if maximized {0.0} else {row.scroll_x().clamp(0.0,max)},"viewport":viewport,"content":if maximized {viewport} else {row.content_width()},"max":max}))
            }).unwrap_or(serde_json::Value::Null)
        };
        if snapshot["desktopPan"] != pan {
            snapshot["desktopPan"] = pan;
            changed = true;
        }
        let history = self
            .history_target()
            .map(|target| json!({"target":target}))
            .unwrap_or(serde_json::Value::Null);
        if snapshot["history"] != history {
            snapshot["history"] = history;
            changed = true;
        }
        let available = self.activity_snapshot().is_ok();
        let capabilities = json!({"select":true,"width":true,"maximize":true,"strips":true,"commands":true,"globalSearch":true,"close":true,
            "workspaceSelect":!self.handle.tree.borrow().workspaces.is_empty(),"workspaceHistory":available,"workspaceReplay":available});
        let activity = self.activity.as_ref().filter(|_| available).map(|activity| json!({
            "workspace":activity.workspace.0.to_string(), "view":activity.pane.view(),
            "playing":activity.pane.replay_is_playing(),
            "coverage":if activity.pane.feed_coverage().is_complete() { "complete" } else { "partial" }
        })).unwrap_or(serde_json::Value::Null);
        for (key, value) in [
            ("capabilities", capabilities),
            ("workspaceActivity", activity),
            (
                "workspaceActivityCloseReason",
                json!(self.activity_close_reason),
            ),
        ] {
            if snapshot[key] != value {
                snapshot[key] = value;
                changed = true;
            }
        }
        if changed {
            self.handle
                .navigation_revision
                .set(self.handle.navigation_revision.get().wrapping_add(1));
        }
    }
    fn navigate(
        &mut self,
        command: crate::desktop_navigation::Command,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<(bool, Option<String>), String> {
        use crate::desktop_navigation::Command;
        if command.generation() != self.handle.generation {
            return Err("Stale desktop surface".into());
        }
        if let Command::Close { route, .. } = &command {
            if self.active_route().as_ref() != Some(route) {
                return Err("Application is no longer active".into());
            }
            let (_, strip, group, tab) = route.ids()?;
            let location = qos_desktop::session_commands::TabLocation {
                strip: strip.0,
                group,
                tab,
            };
            if let Some((invocation, _)) = self.handle.session_requests.borrow().iter().find(|(_, bound)| matches!(&bound.command, qos_desktop::session_commands::DesktopCommand::CloseTab { location: pending } if pending == &location)) {
                return Ok((false, Some(invocation.to_string())));
            }
            let resources = self.resources(doc);
            let address = tilo_actions::ActionAddress::new("desktop:requests", "close-tab");
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .map_err(|error| error.to_string())?;
            let output = self
                .invoke_resource_operation_v2(
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                    doc,
                    path,
                )?
                .ok_or("Close action unavailable")?;
            return output
                .value
                .and_then(|value| value.as_str().map(str::to_owned))
                .map(|invocation| (false, Some(invocation)))
                .ok_or_else(|| "Close request identity unavailable".into());
        }
        let request = match &command {
            Command::Workspace { workspace, .. }
            | Command::WorkspaceActivity { workspace, .. }
            | Command::CloseWorkspaceActivity { workspace, .. } => {
                let id = workspace
                    .parse::<u64>()
                    .ok()
                    .filter(|id| id.to_string() == *workspace)
                    .map(WorkspaceId)
                    .ok_or("Invalid navigation identity")?;
                Some(match &command {
                    Command::Workspace { .. } => {
                        activity::WorkspaceViewRequest::Select { workspace: id }
                    }
                    Command::WorkspaceActivity { view, .. } => {
                        activity::WorkspaceViewRequest::OpenActivity {
                            workspace: id,
                            view: *view,
                        }
                    }
                    _ => {
                        if self.handle.tree.borrow().active != id {
                            return Err("Workspace is no longer active".into());
                        }
                        activity::WorkspaceViewRequest::CloseActivity { workspace: id }
                    }
                })
            }
            _ => None,
        };
        if let Some(request) = request {
            self.queue_view_request(request)?;
            return Ok((false, None));
        }
        if let Command::Pan {
            workspace,
            strip,
            offset,
            ..
        } = &command
        {
            if !offset.is_finite() {
                return Err("Invalid pan offset".into());
            }
            let tree = self.handle.tree.borrow();
            let w = tree
                .active_workspace()
                .filter(|w| w.id.0.to_string() == workspace.as_str())
                .ok_or("Workspace is no longer active")?;
            let active = tree
                .active_strip
                .get(&w.id)
                .copied()
                .or_else(|| w.strips.first().map(|s| s.id))
                .filter(|s| s.0.to_string() == strip.as_str())
                .ok_or("Strip is no longer active")?;
            drop(tree);
            return self
                .surface
                .pan_desktop(active, *offset)
                .map(|_| (false, None))
                .ok_or_else(|| "Desktop pan unavailable".into());
        }
        if let Command::History { route, target, .. } = command {
            if self.active_route().as_ref() != Some(&route)
                || self.history_target().as_ref() != Some(&target)
            {
                return Err("Document history target is no longer active".into());
            }
            let (_, s, g, t) = route.ids()?;
            let host = self
                .surface
                .row(s)
                .and_then(|row| {
                    row.groups
                        .iter()
                        .find(|group| group.id == g && group.active_tab_id() == t)
                })
                .and_then(|group| group.active_host())
                .ok_or("Document history unavailable")?;
            if !host.request_document_history(&target) {
                return Err("Document history unavailable".into());
            }
            // navigate runs during update, after outgoing layout focus capture.
            // The exact document request must succeed before retiring its overlay.
            self.close_activity(crate::desktop_activity::ActivityCloseReason::ResourceOpened);
            return Ok((false, None));
        }
        if let Command::Scroll { region, action, .. } = command {
            self.refresh_navigation(false);
            if self.handle.navigation.borrow()["scroll"]["region"].as_str() != Some(&region) {
                return Err("Scroll target is no longer active".into());
            }
            let route = self.active_route().ok_or("No active application")?;
            let (_, s, g, t) = route.ids()?;
            return match self
                .surface
                .apply_primary_vertical_scroll(s, g, t, action.into())
            {
                ui::widget::PrimaryScrollResult::Unsupported => {
                    Err("Scroll action unavailable".into())
                }
                _ => Ok((false, None)),
            };
        }
        let current = self.active_route();
        let mut tree = self.handle.tree.borrow_mut();
        if let Command::Strip {
            workspace, strip, ..
        } = command
        {
            let w = tree
                .workspaces
                .iter()
                .find(|w| w.id.0.to_string() == workspace)
                .ok_or("Workspace unavailable")?;
            let s = w
                .strips
                .iter()
                .find(|s| s.id.0.to_string() == strip)
                .ok_or("Strip unavailable")?
                .id;
            let w = w.id;
            Self::apply_local(&self.handle, &mut tree, LocalViewIntent::Workspace(w));
            Self::apply_local(&self.handle, &mut tree, LocalViewIntent::Strip(s));
            return Ok((true, None));
        }
        let route = match &command {
            Command::Select { route, .. }
            | Command::Width { route, .. }
            | Command::Maximize { route, .. } => route,
            _ => unreachable!(),
        };
        let (w, s, g, t) = route.ids()?;
        if !matches!(&command, Command::Select { .. }) && current.as_ref() != Some(route) {
            return Err("Application is no longer active".into());
        }
        if !tree
            .workspaces
            .iter()
            .filter(|v| v.id == w)
            .flat_map(|w| &w.strips)
            .filter(|v| v.id == s)
            .flat_map(|s| &s.window_groups)
            .filter(|v| v.id == g)
            .flat_map(|g| &g.tabs)
            .any(|v| v.id == t)
        {
            return Err("Application route unavailable".into());
        }
        match command {
            Command::Select { .. } => {
                self.surface.reveal_on_next_layout(s);
                if let Some(strip) = tree
                    .workspaces
                    .iter_mut()
                    .find(|v| v.id == w)
                    .and_then(|w| w.strips.iter_mut().find(|v| v.id == s))
                {
                    if strip.window_groups.iter().any(|group| group.maximized) {
                        for group in &mut strip.window_groups {
                            group.maximized = group.id == g;
                        }
                    }
                }
                Self::local(&mut tree, LocalViewIntent::Workspace(w));
                Self::local(&mut tree, LocalViewIntent::Tab { group: g, tab: t });
            }
            Command::Width { permille, .. } => {
                if !(100..=1000).contains(&permille) {
                    return Err("Width must be between 10% and 100%".into());
                }
                self.surface.reveal_on_next_layout(s);
                Self::local(
                    &mut tree,
                    LocalViewIntent::Maximized {
                        group: g,
                        maximized: false,
                    },
                );
                Self::local(
                    &mut tree,
                    LocalViewIntent::Width {
                        group: g,
                        width: WindowWidth::Fraction(permille),
                    },
                );
            }
            Command::Maximize { maximized, .. } => Self::local(
                &mut tree,
                LocalViewIntent::Maximized {
                    group: g,
                    maximized,
                },
            ),
            _ => unreachable!(),
        }
        Ok((true, None))
    }
    fn reconcile(&mut self) -> bool {
        let intents = self.surface.take_intents();
        let chrome = std::mem::take(&mut *self.chrome_intents.borrow_mut());
        let inbox = self.handle.inbox.borrow_mut().take();
        let admission_changed = inbox.is_some();
        let mut tree = self.handle.tree.borrow_mut();
        let mut changed = std::mem::take(&mut self.view_changed)
            || !intents.is_empty()
            || !chrome.is_empty()
            || inbox.is_some();
        // Explicit workspace switches are newer than gestures harvested from the
        // outgoing surface. Save those tab choices without stealing active focus.
        for intent in chrome {
            match intent {
                qos_desktop::chrome::ChromeIntent::SwitchWorkspace(delta) => {
                    Self::switch_workspace(&self.handle, &mut tree, delta);
                }
                _ => self
                    .handle
                    .errors
                    .borrow_mut()
                    .push("This desktop service is not available in this peer yet".into()),
            }
        }
        for intent in intents {
            match intent {
                DesktopIntent::Local(intent) => Self::apply_local(&self.handle, &mut tree, intent),
                DesktopIntent::Session(intent) => match qos_desktop::session_commands::bind_intent(
                    &tree,
                    qos_desktop::session_commands::CommandSource {
                        room: self.resolver.host.room.clone(),
                        heads: self.source_heads.clone(),
                    },
                    intent,
                ) {
                    Ok(command) => {
                        let mut queue = self.handle.session_requests.borrow_mut();
                        if queue.len() < 32 {
                            queue.push((uuid::Uuid::new_v4(), command));
                        } else {
                            self.handle
                                .errors
                                .borrow_mut()
                                .push("desktop request queue is full".into());
                        }
                    }
                    Err(error) => self.handle.errors.borrow_mut().push(error),
                },
            }
        }
        if let Some((admission, permissions)) = inbox {
            self.handle.history_available.set(true);
            self.source_heads = admission.source_heads;
            self.admitted_room = admission.admitted_room;
            self.required_records = admission.required;
            self.unsupported_apps = admission.unsupported;
            let local = LocalView::from_tree(&tree);
            *tree = admission.tree;
            local.apply(&mut tree);
            self.resolver.host.routes = admission.routes;
            *self.resolver.host.permissions.borrow_mut() = permissions;
        }
        drop(tree);
        // Pane resource navigation must select its destination before mounting
        // and restoring focus, so the outgoing editor is no longer eligible.
        self.refresh_activity(admission_changed);
        changed |= std::mem::take(&mut self.view_changed);
        let tree = self.handle.tree.borrow();
        if changed {
            let factory = Self::image_factory(&self.handle);
            let result = ui::ImageStore::with_host_factory(factory.clone(), || {
                self.surface
                    .reconcile(&tree, &self.fallback, &mut self.resolver)
            });
            if result == ReconcileOutcome::RebuildRequired {
                ui::ImageStore::with_host_factory(factory, || {
                    self.surface.rebuild_retaining(
                        &tree,
                        &self.fallback,
                        &mut self.resolver,
                        Self::chrome(&tree, self.chrome.clone(), self.notify.clone()),
                    )
                });
                self.dirty = true;
            }
            if let Ok(bytes) = serde_json::to_vec(&LocalView::from_tree(&tree)) {
                if self
                    .storage
                    .get(&self.view_key)
                    .is_none_or(|saved| saved.bytes != bytes)
                {
                    if let Err(e) = self.storage.put(self.view_key.clone(), bytes, None) {
                        self.handle.errors.borrow_mut().push(e);
                    }
                }
            }
        }
        self.surface.sync_navigation(&tree);
        drop(tree);
        self.refresh_navigation(changed);
        self.collect_failures();
        for report in &mut self.reports {
            let _ = report.retry_view();
            let _ = report.retry_record();
        }
        changed |= self.dirty;
        changed
    }
}

impl Widget for DesktopRoot {
    fn caller_view_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut resources = self.resources(doc);
        let mut views = tilo_actions::Resource::new(
            activity::view_actions::RESOURCE_ID,
            "desktop",
            "Workspace views",
        );
        for descriptor in activity::view_actions::workspace_view_descriptors(
            &self.handle.tree.borrow(),
            self.activity_snapshot().is_ok(),
        ) {
            views = views.with_action(descriptor.action());
        }
        resources.push(views);
        resources
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "qos.desktop.request",
            activity::view_actions::OPERATION_TYPE,
        ]
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        use qos_desktop::session_commands::*;
        use tilo_actions::{
            ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
        };
        let tree = self.handle.tree.borrow();
        let Some(workspace) = tree.active_workspace() else {
            return vec![];
        };
        let source = CommandSource {
            room: workspace.room.to_string(),
            heads: self.source_heads.clone(),
        };
        let mut actions = vec![];
        let mut add = |id: String, label: String, command: DesktopCommand| {
            if let Ok(action) = ActionSpec::operation(
                id,
                label,
                OperationKind::View,
                "qos.desktop.request",
                &BoundCommand {
                    source: source.clone(),
                    command,
                },
            ) {
                actions.push(action);
            }
        };
        let strip = tree
            .active_strip
            .get(&workspace.id)
            .and_then(|id| workspace.strips.iter().find(|s| s.id == *id))
            .or_else(|| workspace.strips.first());
        if let Some(strip) = strip {
            let active_group = tree
                .active_group
                .get(&strip.id)
                .and_then(|id| strip.window_groups.iter().find(|g| g.id == *id))
                .or_else(|| strip.window_groups.first());
            for (app, definition) in desktop::installed_applications() {
                let label = definition.title;
                add(
                    format!("open-{app}"),
                    format!("Open {label}"),
                    DesktopCommand::OpenTab {
                        app: app.into(),
                        placement: TabPlacement::NewGroup {
                            strip: strip.id.0,
                            after: active_group.map(|g| g.id),
                        },
                    },
                );
            }
            if let Some(group) = active_group {
                if let Some(tab) = group.tabs.iter().find(|t| t.id == group.active_tab) {
                    let location = TabLocation {
                        strip: strip.id.0,
                        group: group.id,
                        tab: tab.id,
                    };
                    add(
                        "close-tab".into(),
                        format!("Close {}", tab.title),
                        DesktopCommand::CloseTab { location },
                    );
                    let index = group
                        .tabs
                        .iter()
                        .position(|item| item.id == tab.id)
                        .unwrap();
                    if index > 0 {
                        add(
                            "tab-left".into(),
                            format!("Move {} left", tab.title),
                            DesktopCommand::ReorderTab {
                                location,
                                before: Some(group.tabs[index - 1].id),
                            },
                        );
                    }
                    if index + 1 < group.tabs.len() {
                        add(
                            "tab-right".into(),
                            format!("Move {} right", tab.title),
                            DesktopCommand::ReorderTab {
                                location,
                                before: group.tabs.get(index + 2).map(|item| item.id),
                            },
                        );
                    }
                    let group_index = strip
                        .window_groups
                        .iter()
                        .position(|item| item.id == group.id)
                        .unwrap();
                    if group_index > 0 {
                        add(
                            "group-left".into(),
                            "Move group left".into(),
                            DesktopCommand::ReorderGroup {
                                strip: strip.id.0,
                                group: group.id,
                                before: Some(strip.window_groups[group_index - 1].id),
                            },
                        );
                    }
                    if group_index + 1 < strip.window_groups.len() {
                        add(
                            "group-right".into(),
                            "Move group right".into(),
                            DesktopCommand::ReorderGroup {
                                strip: strip.id.0,
                                group: group.id,
                                before: strip
                                    .window_groups
                                    .get(group_index + 2)
                                    .map(|item| item.id),
                            },
                        );
                    }
                    add(
                        "duplicate-tab".into(),
                        format!("Duplicate {}", tab.title),
                        DesktopCommand::DuplicateTab { location },
                    );
                    if group.tabs.len() > 1 {
                        add(
                            "split-tab".into(),
                            format!("Split {} into a new group", tab.title),
                            DesktopCommand::SplitTab {
                                location,
                                before: None,
                            },
                        );
                    }
                    for destination in &workspace.strips {
                        if destination.id != strip.id {
                            add(
                                format!("move-strip-{}", destination.id.0),
                                format!("Move {} to strip {}", tab.title, destination.name),
                                DesktopCommand::MoveTab {
                                    location,
                                    placement: TabPlacement::NewGroup {
                                        strip: destination.id.0,
                                        after: None,
                                    },
                                },
                            );
                        }
                        for (index, target) in destination
                            .window_groups
                            .iter()
                            .enumerate()
                            .filter(|(_, g)| g.id != group.id)
                        {
                            add(
                                format!("move-group-{}", target.id),
                                format!(
                                    "Move {} to {} group {}",
                                    tab.title,
                                    destination.name,
                                    index + 1
                                ),
                                DesktopCommand::MoveTab {
                                    location,
                                    placement: TabPlacement::InGroup {
                                        strip: destination.id.0,
                                        group: target.id,
                                        after: target.tabs.last().map(|t| t.id),
                                    },
                                },
                            );
                        }
                    }
                }
            }
        } else {
            for (app, definition) in desktop::installed_applications() {
                let label = definition.title;
                add(
                    format!("open-{app}"),
                    format!("Open {label}"),
                    DesktopCommand::OpenTab {
                        app: app.into(),
                        placement: TabPlacement::EmptyWorkspace,
                    },
                );
            }
        }
        for strip in &workspace.strips {
            if strip.window_groups.is_empty() && workspace.strips.len() > 1 {
                add(
                    format!("remove-strip-{}", strip.id.0),
                    format!("Remove empty strip {}", strip.name),
                    DesktopCommand::RemoveStrip { strip: strip.id.0 },
                );
            }
        }
        drop(add);
        for (id, label, strip) in std::iter::once(("new-strip".into(), "New strip".into(), None))
            .chain(workspace.strips.iter().map(|strip| {
                (
                    format!("rename-strip-{}", strip.id.0),
                    format!("Rename strip {}", strip.name),
                    Some(strip.id.0),
                )
            }))
        {
            let source = source.clone();
            actions.push(ActionSpec::typed_operation(
                id,
                label,
                "Name",
                OperationKind::View,
                "qos.desktop.request",
                vec![ActionParam::required("name", ParamType::Text, "Name")],
                move |args| {
                    let name = args.text("name").unwrap_or_default().to_string();
                    EncodedAction::encode_operation(
                        "qos.desktop.request",
                        &BoundCommand {
                            source: source.clone(),
                            command: strip.map_or_else(
                                || DesktopCommand::CreateStrip { name: name.clone() },
                                |strip| DesktopCommand::RenameStrip {
                                    strip,
                                    name: name.clone(),
                                },
                            ),
                        },
                    )
                    .expect("typed desktop command encodes")
                },
            ));
        }
        actions.push(ActionSpec::typed_operation(
            "rename-workspace",
            "Rename workspace",
            "Name",
            OperationKind::View,
            "qos.desktop.request",
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "Workspace name",
            )],
            move |args| {
                EncodedAction::encode_operation(
                    "qos.desktop.request",
                    &BoundCommand {
                        source: source.clone(),
                        command: DesktopCommand::RenameWorkspace {
                            name: args.text("name").unwrap_or_default().into(),
                        },
                    },
                )
                .expect("typed desktop command encodes")
            },
        ));
        let mut resource = Resource::new("desktop:requests", "desktop", "Desktop");
        for action in actions {
            resource = resource.with_action(action);
        }
        vec![resource]
    }
    fn invoke_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        if kind == tilo_actions::OperationKind::View
            && type_id == activity::view_actions::OPERATION_TYPE
        {
            let request = serde_json::from_slice(payload)
                .map_err(|e| format!("Invalid workspace view request: {e}"))?;
            self.queue_view_request(request)?;
            return Ok(Some(Default::default()));
        }
        if kind != tilo_actions::OperationKind::View || type_id != "qos.desktop.request" {
            return Ok(None);
        }
        let command: qos_desktop::session_commands::BoundCommand =
            serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let mut queue = self.handle.session_requests.borrow_mut();
        if queue.len() >= 32 {
            self.handle
                .errors
                .borrow_mut()
                .push("desktop request queue is full".into());
            return Err("desktop request queue is full".into());
        }
        let invocation = uuid::Uuid::new_v4();
        queue.push((invocation, command));
        self.dirty = true;
        (self.notify)();
        Ok(Some(tilo_actions::ActionOutput {
            value: Some(serde_json::Value::String(invocation.to_string())),
            resources: vec![],
        }))
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        self.surface.sync(doc, path);
    }
    fn update(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
        // Process the current admission before validating a queued local view.
        let mut changed = self.reconcile();
        if let Some(request) = self.pending_view_request.take() {
            if let Err(error) = self.apply_view_request(request) {
                self.handle.errors.borrow_mut().push(error);
            }
            changed |= self.reconcile();
        }
        let command = self.handle.navigation_command.borrow_mut().take();
        if let Some(command) = command {
            let result = self.navigate(command, doc, path);
            self.view_changed = result.as_ref().is_ok_and(|(changed, _)| *changed);
            *self.handle.navigation_result.borrow_mut() =
                Some(result.map(|(_, invocation)| invocation));
            changed |= self.reconcile();
        }
        let updated = self.surface.update(doc, path);
        changed || updated
    }
    fn needs_redraw(&self) -> bool {
        // Composition consumes structural dirtiness before a queued navigation
        // request reaches the next update. Keep that deferred work scheduled.
        self.pending_view_request.is_some()
            || self.dirty
            || (self.activity.is_some() && !self.handle.history_available.get())
            || self.handle.inbox.borrow().is_some()
            || self.activity.as_ref().is_some_and(Widget::needs_redraw)
            || self.surface.needs_redraw()
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let surface = self.surface.build_layout(tree, theme, map);
        self.surface_node = Some(surface);
        self.activity_node = self
            .activity
            .as_mut()
            .map(|activity| activity.build_layout(tree, theme, map));
        if let Some(activity) = self.activity_node {
            tree.new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[activity, surface],
            )
            .unwrap()
        } else {
            tree.new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[surface],
            )
            .unwrap()
        }
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.surface
            .apply_layout(tree, self.surface_node.unwrap_or(node), origin);
        if let (Some(activity), Some(node)) = (&mut self.activity, self.activity_node) {
            activity.apply_layout(tree, node, origin);
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
        self.surface.draw(scene, transform, text, theme, presence);
        if let Some(activity) = &mut self.activity {
            activity.draw(scene, transform, text, theme, presence);
        }
        self.refresh_navigation(false);
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
        if let Some(activity) = &mut self.activity {
            activity.handle_event(
                event,
                scale,
                modifiers,
                doc,
                path,
                handled,
                focus,
                self.activity_node.unwrap_or(node),
                tree,
            );
            return;
        }
        self.surface.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, tree,
        );
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(activity) = &self.activity {
            vec![activity]
        } else {
            vec![&self.surface]
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(activity) = &mut self.activity {
            vec![activity]
        } else {
            vec![&mut self.surface]
        }
    }
    fn name(&self) -> &str {
        "CertifiedDesktop"
    }
    fn get_rect(&self) -> Rect {
        self.surface.get_rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
            | self.surface.take_structure_dirty()
            | self
                .activity
                .as_mut()
                .is_some_and(Widget::take_structure_dirty)
    }
    fn wants_pointer_capture(&self) -> bool {
        self.surface.wants_pointer_capture()
    }
    fn close_components(&mut self) -> Vec<ComponentCloseReport> {
        self.close_activity(crate::desktop_activity::ActivityCloseReason::Disposed);
        self.reports.extend(self.surface.close_components());
        std::mem::take(&mut self.reports)
    }
    fn take_component_close_reports(&mut self) -> Vec<ComponentCloseReport> {
        self.surface.take_component_close_reports()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    fn history_document(tree: &WorkspaceTree) -> AutoCommit {
        let mut doc = AutoCommit::new();
        if let Some(workspace) = tree.active_workspace() {
            doc.put(automerge::ROOT, "room", workspace.room.to_string())
                .unwrap();
            doc.put(automerge::ROOT, "name", &workspace.name).unwrap();
            qos_desktop::projection::RoomProjection::project_owned_contents(
                &mut doc,
                &workspace.strips,
                5,
                None,
            );
            doc.commit();
        }
        doc
    }
    fn fixture_with_topology(
        configure: impl FnOnce(&mut WorkspaceTree),
    ) -> (DesktopRoot, DesktopHandle, PortableStorage) {
        let cache = PortableStorage::new("desktop-host-test", "device").unwrap();
        let mut owners = RecordOwners::portable(cache.clone());
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId(1), "Work");
        let room = workspace.room.to_string();
        let mut tabs = vec![];
        let mut routes = BTreeMap::new();
        // Keep the real initialized stores alive until their checked components take custody.
        let mut retained = vec![];
        for (i, (durable_app_id, definition)) in
            desktop::installed_applications().into_iter().enumerate()
        {
            let id = (5 << 48) | ((i as u64) + 10);
            let record = if i == 0 {
                RecordCheckout::new(
                    ui::record::RecordId::for_tab(1),
                    definition.record_kind.clone(),
                )
            } else {
                qos_desktop::records::component_record(
                    durable_app_id,
                    id,
                    definition.record_kind.clone(),
                )
            };
            let store = owners.open(durable_app_id, &room).unwrap();
            store.create_record(&record).unwrap();
            store.with(&record.id, |doc| match i {
                0 => word_processor::initialize_empty_document(doc).unwrap(),
                1 => chat::doc::ChatDoc::seed_starter(doc, "author", "Author", "AU"),
                2 => notes::notebook::initialize_empty(doc).unwrap(),
                _ => calculator::doc::initialize_empty(doc).unwrap(),
            });
            if i == 0 {
                routes.insert(id, record.id.as_str().into());
            }
            tabs.push(Tab {
                id,
                title: definition.title.into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other(durable_app_id.into()),
                    label: definition.title.into(),
                },
            });
            retained.push(store);
        }
        workspace.strips = vec![Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: 3,
                active_tab: tabs[0].id,
                tabs,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        tree.active = workspace.id;
        tree.workspaces.push(workspace);
        configure(&mut tree);
        let permissions = ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
            vec![],
        );
        let admission = Admission {
            admitted_room: history_document(&tree),
            source_heads: vec![[7; 32]],
            tree,
            routes,
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        };
        let (root, handle) = DesktopRoot::new(
            admission,
            cache.clone(),
            &room,
            "device",
            "author",
            1,
            permissions,
            Arc::new(|| {}),
        )
        .unwrap();
        assert!(
            handle.errors.borrow().is_empty(),
            "{:?}",
            handle.errors.borrow()
        );
        (root, handle, cache)
    }

    #[test]
    fn workspace_activity_actions_are_local_and_select_empty_workspaces() {
        use qos_desktop::activity::{ActivityView, view_actions::*};
        let (mut root, handle, _) = fixture();
        let other = WorkspaceId(u64::MAX - 1);
        handle
            .tree
            .borrow_mut()
            .workspaces
            .push(Workspace::new(other, "Work"));
        let actions = root.caller_view_resources(&AutoCommit::new());
        assert!(actions.iter().any(|r| r.id.0 == RESOURCE_ID));
        let doc = AutoCommit::new();
        for view in [
            ActivityView::CatchUp,
            ActivityView::Explore,
            ActivityView::Replay,
        ] {
            let request = WorkspaceViewRequest::OpenActivity {
                workspace: WorkspaceId(1),
                view,
            };
            let previous_view = root.activity.as_ref().map(|activity| activity.pane.view());
            root.invoke_resource_operation_v2(
                tilo_actions::OperationKind::View,
                OPERATION_TYPE,
                &serde_json::to_vec(&request).unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                root.activity.as_ref().map(|activity| activity.pane.view()),
                previous_view,
                "operations must defer structural changes until the frame captures focus"
            );
            root.update(&mut AutoCommit::new(), &automerge::ROOT);
            assert_eq!(root.activity.as_ref().unwrap().pane.view(), view);
            assert!(!root.activity.as_ref().unwrap().pane.replay_is_playing());
            assert!(handle.session_requests.borrow().is_empty());
        }
        let remembered = LocalView::from_tree(&handle.tree.borrow());
        root.apply_view_request(WorkspaceViewRequest::Select { workspace: other })
            .unwrap();
        assert_eq!(handle.tree.borrow().active, other);
        assert!(root.activity.is_none());
        assert!(
            root.apply_view_request(WorkspaceViewRequest::Select {
                workspace: WorkspaceId(999)
            })
            .is_err()
        );
        assert_eq!(handle.tree.borrow().active, other);
        root.apply_view_request(WorkspaceViewRequest::Select {
            workspace: WorkspaceId(1),
        })
        .unwrap();
        let returned = LocalView::from_tree(&handle.tree.borrow());
        assert_eq!(returned.tabs, remembered.tabs);
        assert_eq!(returned.strips, remembered.strips);
        assert!(handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn workspace_activity_capture_never_settles_pending_edits_and_filters_stale_owners() {
        use crate::desktop_activity::capture_records;
        let (_root, handle, storage) = fixture();
        let owners = handle.records.borrow();
        let writer = owners
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap();
        let required = vec![desktop::Requirement {
            app_id: writer.app.clone(),
            record_id: writer.checkout.id.as_str().into(),
        }];
        writer.store.with(&writer.checkout.id, |doc| {
            doc.commit();
        });
        assert!(capture_records(&required, &owners)[0].document.is_some());
        writer.store.with(&writer.checkout.id, |doc| {
            doc.put(automerge::ROOT, "pending-history-test", "unfinished")
                .unwrap();
        });
        let pending = writer
            .store
            .read(&writer.checkout.id, |doc| {
                (doc.pending_ops(), doc.base_heads().to_vec())
            })
            .unwrap();
        assert!(pending.0 > 0);
        let generation = writer.store.action_generation(&writer.checkout.id).unwrap();
        let writes = storage.pending_writes().len();
        assert!(capture_records(&required, &owners)[0].document.is_none());
        assert_eq!(
            writer
                .store
                .read(&writer.checkout.id, |doc| (
                    doc.pending_ops(),
                    doc.base_heads().to_vec()
                ))
                .unwrap(),
            pending
        );
        assert_eq!(
            writer.store.action_generation(&writer.checkout.id).unwrap(),
            generation
        );
        assert_eq!(storage.pending_writes().len(), writes);
        assert!(
            capture_records(&[], &owners).is_empty(),
            "obsolete owners cannot enter a newer admission"
        );
        let missing = vec![desktop::Requirement {
            app_id: writer.app.clone(),
            record_id: ui::record::RecordId::for_tab(98765).as_str().into(),
        }];
        assert!(capture_records(&missing, &owners)[0].document.is_none());
    }

    #[test]
    fn workspace_activity_modal_retains_desktop_and_closes_on_access_loss() {
        use activity::{ActivityView, WorkspaceViewRequest};
        let (mut root, handle, _) = fixture();
        let writer = root.surface.row(StripId(2)).unwrap().groups[0]
            .active_host()
            .unwrap() as *const _;
        root.apply_view_request(WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view: ActivityView::Replay,
        })
        .unwrap();
        assert!(
            root.children()
                .iter()
                .all(|child| child.name() == "WorkspaceActivity")
        );
        assert!(root.activity.as_ref().unwrap().traps_focus());
        let mut boxed = Box::new(root);
        let root_ptr = std::ptr::from_mut(boxed.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            boxed,
            (1000, 700),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("Workspace activity")
        );
        // SAFETY: frame owns the stable boxed root; no harness operation
        // overlaps the temporary borrow used to find the real pane control.
        let play = unsafe {
            (&*root_ptr)
                .activity
                .as_ref()
                .unwrap()
                .pane
                .accessibility_snapshot()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Play replay"))
                .unwrap()
                .bounds
                .unwrap()
                .to_rect()
                .center()
        };
        frame.click_at(play.x, play.y);
        assert!(unsafe {
            (&*root_ptr)
                .activity
                .as_ref()
                .unwrap()
                .pane
                .replay_is_playing()
        });
        let before = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| {
                        (doc.base_heads().to_vec(), doc.pending_ops())
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        frame.type_text("must not reach the editor");
        assert_eq!(
            before,
            handle
                .records
                .borrow()
                .values()
                .map(|r| r
                    .store
                    .read(&r.checkout.id, |doc| (
                        doc.base_heads().to_vec(),
                        doc.pending_ops()
                    ))
                    .unwrap())
                .collect::<Vec<_>>()
        );
        frame.key("Escape");
        frame.relayout();
        assert!(frame.inspection_json().unwrap().contains("WordProcessor"));
        assert!(handle.session_requests.borrow().is_empty());
        // Permission updates retire the modal before another event can use it.
        let (mut root, handle, _) = fixture();
        assert!(unsafe { (&*root_ptr).activity.is_none() });
        assert_eq!(writer, unsafe {
            (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                .active_host()
                .unwrap() as *const _
        });
        root.apply_view_request(WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view: ActivityView::Explore,
        })
        .unwrap();
        *handle.permissions.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(None, "revoked"),
            vec![],
        );
        root.reconcile();
        assert!(root.activity.is_none());
        assert!(
            root.apply_view_request(WorkspaceViewRequest::OpenActivity {
                workspace: WorkspaceId(1),
                view: ActivityView::Explore
            })
            .is_err()
        );
    }

    #[test]
    fn workspace_activity_admission_replaces_sources_and_reports_unsupported_coverage() {
        use activity::{ActivityView, WorkspaceViewRequest};
        let (mut root, handle, _) = fixture();
        let requirements = handle
            .records
            .borrow()
            .values()
            .map(|r| desktop::Requirement {
                app_id: r.app.clone(),
                record_id: if r.checkout.id.is_package_primary() {
                    String::new()
                } else {
                    r.checkout.id.as_str().into()
                },
            })
            .collect::<Vec<_>>();
        root.required_records = requirements.clone();
        root.apply_view_request(WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view: ActivityView::Explore,
        })
        .unwrap();
        assert_eq!(
            root.activity
                .as_ref()
                .unwrap()
                .pane
                .feed_coverage()
                .referenced_records,
            4
        );
        let writer = requirements
            .into_iter()
            .find(|r| r.app_id == "dev.qos.word-processor")
            .unwrap();
        let mut source = root.admitted_room.clone();
        source
            .put(automerge::ROOT, "name", "Updated admission")
            .unwrap();
        source.commit();
        let heads = source.get_heads();
        let topology = handle.tree.borrow().clone();
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: source,
                source_heads: heads.iter().map(|head| head.0).collect(),
                tree: topology,
                routes: root.resolver.host.routes.clone(),
                required: vec![writer],
                missing: vec![],
                unsupported: vec!["unsupported.actual.app".into()],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        root.reconcile();
        assert_eq!(
            handle.records.borrow().len(),
            4,
            "old live owners deliberately remain mounted"
        );
        let activity = root.activity.as_mut().unwrap();
        assert_eq!(activity.snapshot.doc.get_heads(), heads);
        assert_eq!(activity.pane.feed_coverage().referenced_records, 1);
        assert_eq!(
            activity.pane.feed_coverage().excluded_apps,
            ["unsupported.actual.app"]
        );
        assert!(!activity.pane.feed_coverage().is_complete());
        assert_eq!(
            root.source_heads,
            heads.iter().map(|head| head.0).collect::<Vec<_>>()
        );
    }

    #[test]
    fn workspace_activity_revoked_admission_and_resource_opening_use_current_authority() {
        use crate::desktop_activity::ActivityCloseReason;
        use activity::{ActivityView, WorkspaceViewRequest};
        let (mut root, handle, _) = fixture();
        let request = WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view: ActivityView::Explore,
        };
        root.apply_view_request(request.clone()).unwrap();
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        let target = ui::HistoryResourceTarget {
            app_id: Some(writer.app.clone()),
            app_label: None,
            id: writer.checkout.id.as_str().into(),
            kind: writer.checkout.kind.as_str().into(),
            label: "untrusted label".into(),
        };
        root.activity.as_mut().unwrap().open_target = Some(ui::HistoryResourceTarget {
            app_id: Some("dev.qos.notes".into()),
            ..target.clone()
        });
        root.refresh_activity(false);
        assert!(
            root.activity.is_some(),
            "a record ID in the wrong app must not resolve"
        );
        root.activity.as_mut().unwrap().open_target = Some(target);
        root.refresh_activity(false);
        assert!(root.activity.is_none());
        assert_eq!(
            root.activity_close_reason,
            Some(ActivityCloseReason::ResourceOpened)
        );
        root.apply_view_request(request.clone()).unwrap();
        handle.invalidate_history();
        root.reconcile();
        assert!(root.activity.is_none());
        assert_eq!(
            root.activity_close_reason,
            Some(ActivityCloseReason::AccessChanged)
        );
        assert!(root.apply_view_request(request).is_err());
        assert!(handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn workspace_activity_real_palette_uses_local_handler_and_restores_editor_focus() {
        let (root, handle, _) = fixture();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1000, 700),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.set_platform_conventions(ui::platform::PlatformConventions::new(
            ui::platform::Platform::Linux,
        ));
        frame.click_at(200.0, 180.0);
        let focused = frame.focused_access_node().unwrap();
        frame.set_modifiers(&["ctrl".into()]);
        frame.key("k");
        frame.set_modifiers(&[]);
        frame.type_text("Replay workspace activity");
        frame.key("Enter");
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("Workspace activity")
        );
        assert!(unsafe {
            (&*root_ptr)
                .activity
                .as_ref()
                .is_some_and(|activity| activity.pane.view() == activity::ActivityView::Replay)
        });
        assert!(handle.session_requests.borrow().is_empty());
        assert!(
            frame.take_caller_operation().is_none(),
            "workspace views must not enter the certified broker"
        );
        frame.key("Escape");
        frame.relayout();
        assert_eq!(frame.focused_access_node().unwrap().name, focused.name);
        frame
            .open_caller_search("Explore workspace activity")
            .unwrap();
        frame.key("Enter");
        let chat = handle
            .records
            .borrow()
            .values()
            .find(|record| record.app == "dev.qos.chat")
            .unwrap()
            .clone();
        // A pane outcome is processed during update, after outgoing focus capture.
        unsafe {
            (&mut *root_ptr).activity.as_mut().unwrap().open_target =
                Some(ui::HistoryResourceTarget {
                    app_id: Some(chat.app),
                    app_label: None,
                    id: chat.checkout.id.as_str().into(),
                    kind: chat.checkout.kind.as_str().into(),
                    label: "Chat".into(),
                });
        }
        frame.relayout();
        assert_eq!(
            unsafe { (&*root_ptr).activity_close_reason },
            Some(crate::desktop_activity::ActivityCloseReason::ResourceOpened)
        );
        assert_eq!(
            handle.navigation.borrow()["active"]["tab"],
            ((5_u64 << 48) | 11).to_string()
        );
        assert_ne!(
            frame.focused_access_node().and_then(|node| node.name),
            focused.name
        );
    }

    #[test]
    fn workspace_activity_and_system_search_preserve_modal_and_editor_focus_ownership() {
        let (root, handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 700),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.click_at(200.0, 180.0);
        let owner = frame
            .inspection()
            .keyboard_focus_owner
            .expect("retained editor owner");
        let route = handle.navigation.borrow()["active"].clone();
        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::WorkspaceActivity {
                generation: handle.generation.clone(),
                workspace: "1".into(),
                view: activity::ActivityView::Replay,
            });
        frame.relayout();
        frame.poll_frame().unwrap();
        frame.relayout();
        assert_eq!(
            handle.navigation.borrow()["workspaceActivity"]["view"],
            "replay"
        );
        assert!(
            handle
                .open_global_search(&mut frame, &handle.generation, "")
                .is_err(),
            "history modal must keep input ownership"
        );
        frame.key("Escape");
        frame.relayout();
        assert!(handle.navigation.borrow()["workspaceActivity"].is_null());
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(owner.as_str())
        );
        handle
            .open_global_search(&mut frame, &handle.generation, "Notes")
            .unwrap();
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some("system-search")
        );
        frame.key("Escape");
        frame.relayout();
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(owner.as_str())
        );
        assert_eq!(handle.navigation.borrow()["active"], route);
        assert!(handle.session_requests.borrow().is_empty());
        assert!(frame.take_caller_operation().is_none());
    }

    #[test]
    fn workspace_activity_queued_views_revalidate_admission_and_destination() {
        use activity::{ActivityView, WorkspaceViewRequest};
        let (mut root, handle, _) = fixture();
        let open = WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view: ActivityView::Explore,
        };
        root.queue_view_request(open.clone()).unwrap();
        assert!(root.activity.is_none());
        // An admission captured before revocation cannot later re-enable history.
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: root.admitted_room.clone(),
                tree: handle.tree.borrow().clone(),
                routes: root.resolver.host.routes.clone(),
                source_heads: root.source_heads.clone(),
                required: root.required_records.clone(),
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        handle.invalidate_history();
        root.update(&mut AutoCommit::new(), &automerge::ROOT);
        assert!(root.activity.is_none());
        assert!(
            handle
                .errors
                .borrow()
                .iter()
                .any(|error| error.contains("admission unavailable"))
        );
        let (mut root, handle, _) = fixture();
        root.queue_view_request(open).unwrap();
        handle
            .tree
            .borrow_mut()
            .workspaces
            .push(Workspace::new(WorkspaceId(2), "Other"));
        handle.tree.borrow_mut().active = WorkspaceId(2);
        root.update(&mut AutoCommit::new(), &automerge::ROOT);
        assert!(root.activity.is_none());
        assert_eq!(handle.tree.borrow().active, WorkspaceId(2));
        assert!(handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn workspace_navigation_idle_accepted_request_wakes_queued_transition() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            tree.workspaces[0].strips.clear();
        });
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 600),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        assert_external_frame_idle(&mut frame, "empty workspace before navigation");
        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::WorkspaceActivity {
                generation: handle.generation.clone(),
                workspace: "1".into(),
                view: activity::ActivityView::Replay,
            });
        // Match desktop_navigate: one synchronous acceptance/layout pass, then
        // only the ordinary host frame clock. No input or second relayout.
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert!(
            handle.navigation.borrow()["workspaceActivity"].is_null(),
            "acceptance must leave replacement queued"
        );
        assert!(
            frame.poll_frame().unwrap(),
            "accepted queued transition must request a frame while the desktop is idle"
        );
        assert_eq!(
            handle.navigation.borrow()["workspaceActivity"]["view"],
            "replay"
        );
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("Workspace activity")
        );
        assert!(handle.session_requests.borrow().is_empty());
        assert_external_frame_idle(&mut frame, "paused replay after queued transition");
    }

    #[test]
    fn workspace_navigation_refresh_preserves_search_close_and_history_capabilities() {
        let (mut root, handle, _) = fixture();
        for topology in [false, true, false] {
            root.refresh_navigation(topology);
            for capability in [
                "globalSearch",
                "close",
                "workspaceSelect",
                "workspaceHistory",
                "workspaceReplay",
            ] {
                assert_eq!(
                    handle.navigation.borrow()["capabilities"][capability],
                    true,
                    "missing {capability} after topology={topology}"
                );
            }
        }
        handle.invalidate_history();
        root.reconcile();
        for capability in ["globalSearch", "close", "workspaceSelect"] {
            assert_eq!(
                handle.navigation.borrow()["capabilities"][capability],
                true,
                "history revocation removed {capability}"
            );
        }
        for capability in ["workspaceHistory", "workspaceReplay"] {
            assert_eq!(
                handle.navigation.borrow()["capabilities"][capability],
                false
            );
        }
        assert!(handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn workspace_navigation_admitted_state_and_queued_commands() {
        let (mut root, handle, _) = fixture_with_topology(|tree| {
            tree.workspaces
                .push(Workspace::new(WorkspaceId(9_007_199_254_740_993), "Empty"));
        });
        assert_eq!(
            handle.navigation.borrow()["capabilities"]["workspaceHistory"],
            true
        );
        let command = |kind: &str, workspace: &str, view: Option<&str>| {
            let mut wire = serde_json::json!({"kind":kind,"generation":handle.generation,"workspace":workspace});
            if let Some(view) = view {
                wire["view"] = view.into();
            }
            serde_json::from_value(wire).unwrap()
        };
        root.navigate(
            command("workspace-activity", "1", Some("replay")),
            &AutoCommit::new(),
            &automerge::ROOT,
        )
        .unwrap();
        assert!(
            root.activity.is_none(),
            "bridge acceptance must only queue the pane"
        );
        root.update(&mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(
            handle.navigation.borrow()["workspaceActivity"],
            serde_json::json!({"workspace":"1","view":"replay","playing":false,"coverage":"complete"})
        );
        root.activity
            .as_mut()
            .unwrap()
            .pane
            .set_view(activity::ActivityView::Tasks);
        root.refresh_navigation(false);
        assert_eq!(
            handle.navigation.borrow()["workspaceActivity"]["view"],
            "tasks"
        );
        for workspace in ["01", "-1", "18446744073709551616", "2", "9007199254740993"] {
            assert!(
                root.navigate(
                    command("workspace-activity", workspace, Some("explore")),
                    &AutoCommit::new(),
                    &automerge::ROOT
                )
                .is_err()
            );
        }
        let mut stale = serde_json::json!({"kind":"close-workspace-activity","generation":"old","workspace":"1"});
        assert!(
            root.navigate(
                serde_json::from_value(stale.clone()).unwrap(),
                &AutoCommit::new(),
                &automerge::ROOT
            )
            .is_err()
        );
        stale["kind"] = "workspace".into();
        assert!(
            root.navigate(
                serde_json::from_value(stale).unwrap(),
                &AutoCommit::new(),
                &automerge::ROOT
            )
            .is_err()
        );
        root.navigate(
            command("close-workspace-activity", "1", None),
            &AutoCommit::new(),
            &automerge::ROOT,
        )
        .unwrap();
        assert!(root.activity.is_some());
        root.update(&mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(
            handle.navigation.borrow()["workspaceActivityCloseReason"],
            "dismissed"
        );
        root.navigate(
            command("workspace", "9007199254740993", None),
            &AutoCommit::new(),
            &automerge::ROOT,
        )
        .unwrap();
        assert_eq!(handle.tree.borrow().active, WorkspaceId(1));
        root.update(&mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(
            handle.navigation.borrow()["activeWorkspace"],
            "9007199254740993"
        );
        assert!(handle.navigation.borrow()["active"].is_null());
        handle.invalidate_history();
        root.reconcile();
        assert_eq!(
            handle.navigation.borrow()["capabilities"]["workspaceHistory"],
            false
        );
        assert_eq!(
            handle.navigation.borrow()["capabilities"]["workspaceReplay"],
            false
        );
        assert!(
            root.navigate(
                command("workspace-activity", "9007199254740993", Some("replay")),
                &AutoCommit::new(),
                &automerge::ROOT
            )
            .is_err()
        );
        assert!(handle.session_requests.borrow().is_empty());
    }

    fn fixture() -> (DesktopRoot, DesktopHandle, PortableStorage) {
        fixture_with_topology(|_| {})
    }

    fn assert_external_frame_idle(frame: &mut ui::headless::FrameHarness, context: &str) {
        let output = frame.frame_output();
        assert!(
            !output.needs_redraw,
            "{context}: settled presentation stayed dirty"
        );
        assert!(
            !output.needs_animation_frame,
            "{context}: settled presentation kept animating"
        );
        assert!(
            output
                .next_wake_deadline
                .is_none_or(|deadline| deadline > ui::clock::Instant::now()),
            "{context}: settled presentation retained a due wake deadline"
        );
        assert!(
            !frame.poll_frame().unwrap(),
            "{context}: idle poll produced another scene"
        );
    }

    fn settle_external_navigation(root: *mut DesktopRoot, frame: &mut ui::headless::FrameHarness) {
        // Initial group framing is a legitimate finite animation. Advance its
        // real clock seam, then run the ordinary frame pipeline once more.
        let settled_at = ui::clock::Instant::now() + ui::clock::Duration::from_secs(1);
        // SAFETY: `frame` retains this boxed root and no harness method runs
        // while the temporary exclusive references are alive.
        unsafe {
            let strips = (&*root).surface.strip_ids();
            for strip in strips {
                (&mut *root)
                    .surface
                    .row_mut(strip)
                    .unwrap()
                    .tick_snap_at(settled_at);
            }
        }
        frame.relayout();
    }

    fn settle_notes_status_bubble(frame: &mut ui::headless::FrameHarness) {
        assert!(
            frame.frame_output().needs_animation_frame,
            "selecting Notes must preserve its finite visible status animation"
        );
        std::thread::sleep(
            ui::status_bubble::DEFAULT_LINGER + ui::clock::Duration::from_millis(10),
        );
        frame.relayout();
    }

    #[test]
    fn external_ordinary_inactive_notes_renders_when_selected_without_redraw_loop() {
        let (root, handle, _) = fixture_with_topology(|topology| {
            let groups = &mut topology.workspaces[0].strips[0].window_groups;
            let chat = groups[0].tabs.remove(1);
            let calculator = groups[0].tabs.pop().unwrap();
            assert_eq!(calculator.id, (5 << 48) | 13);
            groups.push(WindowGroup {
                id: 4,
                active_tab: chat.id,
                tabs: vec![chat],
                width: WindowWidth::Normal,
                maximized: false,
            });
            topology.active_strip.insert(topology.active, StripId(2));
            topology.active_group.insert(StripId(2), 3);
        });
        let routes = root.resolver.host.routes.clone();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1280, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        settle_external_navigation(root_ptr, &mut frame);

        let initial = frame.inspection_json().unwrap();
        assert!(
            initial.contains("WordProcessor"),
            "Writer must be presented"
        );
        assert!(initial.contains("ChatApp"), "Chat must be presented");
        assert!(
            !initial.contains("NotesApp"),
            "inactive Notes must stay retained off-screen"
        );
        assert!(
            frame
                .resources()
                .iter()
                .any(|resource| resource.kind == "notes.notebook"),
            "inactive Notes must retain its resource owner"
        );
        assert_external_frame_idle(&mut frame, "Writer/Chat with inactive Notes");

        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: crate::desktop_navigation::Route {
                    workspace: "1".into(),
                    strip: "2".into(),
                    group: "3".into(),
                    tab: ((5_u64 << 48) | 12).to_string(),
                },
            });
        frame.relayout();
        settle_external_navigation(root_ptr, &mut frame);
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert!(
            frame.inspection_json().unwrap().contains("NotesApp"),
            "selecting retained Notes must present its real checked widget"
        );
        settle_notes_status_bubble(&mut frame);
        assert_external_frame_idle(&mut frame, "selected Notes");

        let refreshed = handle.tree.borrow().clone();
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&refreshed),
                source_heads: vec![[8; 32]],
                tree: refreshed,
                routes,
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        assert!(
            frame.frame_output().needs_redraw,
            "an admitted remote snapshot must still wake the real root"
        );
        assert!(
            frame.poll_frame().unwrap(),
            "remote admission must produce a frame"
        );
        assert_external_frame_idle(&mut frame, "reconciled remote snapshot");
    }

    #[test]
    fn external_hidden_notes_strip_renders_when_selected_without_redraw_loop() {
        let (root, handle, _) = fixture_with_topology(|topology| {
            let notes = topology.workspaces[0].strips[0].window_groups[0]
                .tabs
                .remove(2);
            topology.workspaces[0].strips.push(Strip {
                id: StripId(9),
                name: "Notes strip".into(),
                window_groups: vec![WindowGroup {
                    id: 90,
                    active_tab: notes.id,
                    tabs: vec![notes],
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            });
            topology.active_strip.insert(topology.active, StripId(2));
        });
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1280, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        settle_external_navigation(root_ptr, &mut frame);

        assert!(
            !frame.inspection_json().unwrap().contains("NotesApp"),
            "the retained hidden strip must not be in the presentation tree"
        );
        assert_external_frame_idle(&mut frame, "hidden Notes strip");

        let hidden_title = "Hidden update";
        let hidden_notes_instance = (5_u64 << 48) | 12;
        frame
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: hidden_notes_instance,
                    address: tilo_actions::ActionAddress::new("notes.notebook", "new-page"),
                },
                serde_json::json!({"value": hidden_title})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        frame.relayout();
        assert!(
            !frame.inspection_json().unwrap().contains("NotesApp"),
            "updating the retained owner must not present its hidden strip"
        );
        let notes_record = handle
            .records
            .borrow()
            .values()
            .find(|record| {
                record.app == "dev.qos.notes"
                    && record.checkout.id == ui::record::RecordId::for_tab(hidden_notes_instance)
            })
            .unwrap()
            .clone();
        assert!(
            notes_record
                .store
                .read(&notes_record.checkout.id, |doc| {
                    notes::notebook::project(doc, None)
                        .unwrap()
                        .pages()
                        .iter()
                        .any(|page| page.title == hidden_title)
                })
                .unwrap(),
            "the real retained Notes record must accept the update while hidden"
        );

        *handle.navigation_command.borrow_mut() = Some(crate::desktop_navigation::Command::Strip {
            generation: handle.generation.clone(),
            workspace: "1".into(),
            strip: "9".into(),
        });
        frame.relayout();
        settle_external_navigation(root_ptr, &mut frame);
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert!(
            frame.inspection_json().unwrap().contains("NotesApp"),
            "selecting the retained strip must present its real checked widget"
        );
        frame
            .expect(&ui::headless::SemanticExpectation::named("note_title").value(hidden_title))
            .unwrap();
        settle_notes_status_bubble(&mut frame);
        assert_external_frame_idle(&mut frame, "selected Notes strip");
    }

    #[test]
    fn external_primary_scroll_is_real_private_and_rejects_stale_region() {
        use crate::desktop_navigation::{Command, ScrollAction};
        let (root, handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 420),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        let before = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let first = handle.navigation.borrow().clone();
        assert!(first["scroll"]["max"].as_f64().unwrap() > 0.0);
        *handle.navigation_command.borrow_mut() = Some(Command::Scroll {
            generation: handle.generation.clone(),
            region: first["scroll"]["region"].as_str().unwrap().into(),
            action: ScrollAction::End,
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        let end = handle.navigation.borrow().clone();
        assert_eq!(end["scroll"]["offset"], end["scroll"]["max"]);
        assert!(end["scroll"]["offset"].as_f64().unwrap() > 0.0);
        let after_scroll = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            before, after_scroll,
            "scrolling must not change any document heads"
        );
        let chat = end["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["label"] == "Chat")
            .unwrap()["route"]
            .clone();
        *handle.navigation_command.borrow_mut() = Some(Command::Select {
            generation: handle.generation.clone(),
            route: serde_json::from_value(chat).unwrap(),
        });
        frame.relayout();
        // First native Chat presentation may commit its existing read watermark.
        let before_stale = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        *handle.navigation_command.borrow_mut() = Some(Command::Scroll {
            generation: handle.generation.clone(),
            region: first["scroll"]["region"].as_str().unwrap().into(),
            action: ScrollAction::Start,
        });
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_err()
        );
        let after = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(before_stale, after);
        assert!(handle.session_requests.borrow().is_empty());
    }
    #[test]
    fn external_width_and_maximize_are_local_and_reject_stale_routes() {
        use crate::desktop_navigation::{Command, Route};
        let (root, handle, _) = fixture();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1000, 600),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        let route: Route =
            serde_json::from_value(handle.navigation.borrow()["active"].clone()).unwrap();
        let heads = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        // SAFETY: `frame` owns this root for the test lifetime and no harness
        // call overlaps these temporary shared references.
        let host = unsafe {
            std::ptr::from_ref(
                (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                    .active_host()
                    .unwrap(),
            )
        };
        for permille in [333, 500, 667, 1000] {
            *handle.navigation_command.borrow_mut() = Some(Command::Maximize {
                generation: handle.generation.clone(),
                route: route.clone(),
                maximized: true,
            });
            frame.relayout();
            frame.poll_frame().unwrap();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_ok()
            );
            assert_eq!(handle.navigation.borrow()["maximized"], true);
            assert_eq!(handle.navigation.borrow()["desktopPan"]["max"], 0.0);
            assert_eq!(handle.navigation.borrow()["desktopPan"]["offset"], 0.0);
            *handle.navigation_command.borrow_mut() = Some(Command::Width {
                generation: handle.generation.clone(),
                route: route.clone(),
                permille,
            });
            frame.relayout();
            frame.poll_frame().unwrap();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_ok()
            );
            assert_eq!(handle.navigation.borrow()["widthPermille"], permille);
            assert_eq!(handle.navigation.borrow()["maximized"], false);
        }
        *handle.navigation_command.borrow_mut() = Some(Command::Maximize {
            generation: handle.generation.clone(),
            route: route.clone(),
            maximized: true,
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        for command in [
            Command::Width {
                generation: handle.generation.clone(),
                route: route.clone(),
                permille: 99,
            },
            Command::Width {
                generation: handle.generation.clone(),
                route: Route {
                    tab: "0".into(),
                    ..route.clone()
                },
                permille: 333,
            },
            Command::Width {
                generation: "disposed-mount".into(),
                route: route.clone(),
                permille: 333,
            },
        ] {
            *handle.navigation_command.borrow_mut() = Some(command);
            frame.relayout();
            frame.poll_frame().unwrap();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_err()
            );
            assert_eq!(handle.navigation.borrow()["widthPermille"], 1000);
            assert_eq!(handle.navigation.borrow()["maximized"], true);
        }
        *handle.navigation_command.borrow_mut() = Some(Command::Maximize {
            generation: handle.generation.clone(),
            route: route.clone(),
            maximized: false,
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        let mut stale = route.clone();
        stale.tab = "0".into();
        for command in [
            Command::Maximize {
                generation: handle.generation.clone(),
                route: stale,
                maximized: true,
            },
            Command::Maximize {
                generation: "disposed-mount".into(),
                route: route.clone(),
                maximized: true,
            },
        ] {
            *handle.navigation_command.borrow_mut() = Some(command);
            frame.relayout();
            frame.poll_frame().unwrap();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_err()
            );
        }
        assert_eq!(
            handle.navigation.borrow()["active"],
            serde_json::to_value(&route).unwrap()
        );
        assert!(frame.inspection_json().unwrap().contains("WordProcessor"));
        // SAFETY: same stable frame-owned root as above.
        assert_eq!(host, unsafe {
            std::ptr::from_ref(
                (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                    .active_host()
                    .unwrap(),
            )
        });
        assert_eq!(handle.navigation.borrow()["widthPermille"], 1000);
        assert_eq!(handle.navigation.borrow()["maximized"], false);
        assert!(handle.session_requests.borrow().is_empty());
        assert_eq!(
            heads,
            handle
                .records
                .borrow()
                .values()
                .map(|r| r
                    .store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn external_horizontal_pan_moves_real_groups_and_rejects_stale_targets() {
        use crate::desktop_navigation::{Command, Route};
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .enumerate()
                .map(|(index, tab)| WindowGroup {
                    id: 3 + index as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Fraction(667),
                    maximized: false,
                })
                .collect();
            tree.active_group.insert(strip.id, 3);
        });
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1000, 420),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        settle_external_navigation(root_ptr, &mut frame);
        let first = handle.navigation.borrow().clone();
        let pan = &first["desktopPan"];
        let max = pan["max"].as_f64().unwrap();
        assert!(max > 0.0, "four real groups must overflow the viewport");
        let before = unsafe {
            (&*root_ptr)
                .surface
                .row(StripId(2))
                .unwrap()
                .groups
                .iter()
                .map(|g| (g.id, g.get_rect()))
                .collect::<Vec<_>>()
        };
        let content = pan["content"].clone();
        let vertical = first["scroll"].clone();
        let heads = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let pan_to = |offset| Command::Pan {
            generation: handle.generation.clone(),
            workspace: "1".into(),
            strip: "2".into(),
            offset,
        };
        *handle.navigation_command.borrow_mut() = Some(pan_to(max));
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        let at_end = handle.navigation.borrow().clone();
        assert_eq!(at_end["desktopPan"]["offset"], at_end["desktopPan"]["max"]);
        assert_eq!(at_end["desktopPan"]["content"], content);
        assert_eq!(at_end["scroll"], vertical);
        let after = unsafe {
            (&*root_ptr)
                .surface
                .row(StripId(2))
                .unwrap()
                .groups
                .iter()
                .map(|g| (g.id, g.get_rect()))
                .collect::<Vec<_>>()
        };
        for ((before_id, before_rect), (after_id, after_rect)) in before.iter().zip(&after) {
            assert_eq!(before_id, after_id);
            assert_eq!(before_rect.width(), after_rect.width());
            assert_eq!(before_rect.height(), after_rect.height());
            assert!(after_rect.x0 < before_rect.x0);
        }
        for command in [
            Command::Pan {
                generation: handle.generation.clone(),
                workspace: "1".into(),
                strip: "9".into(),
                offset: 0.0,
            },
            Command::Pan {
                generation: "disposed-mount".into(),
                workspace: "1".into(),
                strip: "2".into(),
                offset: 0.0,
            },
            pan_to(f64::NAN),
        ] {
            *handle.navigation_command.borrow_mut() = Some(command);
            frame.relayout();
            frame.poll_frame().unwrap();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_err()
            );
            assert_eq!(handle.navigation.borrow()["desktopPan"]["offset"], max);
        }
        frame.resize((1200, 420)).unwrap();
        let resized = handle.navigation.borrow()["desktopPan"].clone();
        let resized_offset = resized["offset"].as_f64().unwrap();
        assert!(
            resized_offset > 0.0 && resized_offset <= resized["max"].as_f64().unwrap(),
            "resize keeps manual pan bounded without reframing the selected app"
        );
        let selected_route = first["active"].clone();
        *handle.navigation_command.borrow_mut() = Some(Command::Select {
            generation: handle.generation.clone(),
            route: serde_json::from_value(selected_route).unwrap(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        settle_external_navigation(root_ptr, &mut frame);
        assert!(
            handle.navigation.borrow()["desktopPan"]["offset"]
                .as_f64()
                .unwrap()
                < max,
            "reselecting the same app must reveal it after manual pan"
        );
        let chat = handle.navigation.borrow()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["label"] == "Chat")
            .unwrap()["route"]
            .clone();
        *handle.navigation_command.borrow_mut() = Some(Command::Select {
            generation: handle.generation.clone(),
            route: serde_json::from_value::<Route>(chat.clone()).unwrap(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert_eq!(handle.navigation.borrow()["active"], chat);
        settle_external_navigation(root_ptr, &mut frame);
        let row = unsafe { (&*root_ptr).surface.row(StripId(2)).unwrap() };
        let selected = row.groups.iter().find(|g| g.id == 4).unwrap().get_rect();
        assert!(
            selected.x0 >= row.get_rect().x0 - 1.0 && selected.x1 <= row.get_rect().x1 + 1.0,
            "selection must reframe its exact group"
        );
        assert_eq!(
            heads,
            handle
                .records
                .borrow()
                .values()
                .map(|r| r
                    .store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap())
                .collect::<Vec<_>>()
        );
        assert!(handle.session_requests.borrow().is_empty());
        // All four app leaves are visible here; whole-frame idle is covered by
        // the separately queued PreviewCanvas regression work.
    }
    #[test]
    fn external_close_uses_the_real_root_request_and_rejects_stale_routes() {
        use crate::desktop_navigation::{Command, Route};
        use qos_desktop::session_commands::DesktopCommand;
        let (root, handle, _) = fixture();
        let before = serde_json::to_string(&*handle.tree.borrow()).unwrap();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 600),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        assert_eq!(handle.navigation.borrow()["capabilities"]["close"], true);
        let route: Route =
            serde_json::from_value(handle.navigation.borrow()["active"].clone()).unwrap();
        let close = || Command::Close {
            generation: handle.generation.clone(),
            route: route.clone(),
        };

        *handle.navigation_command.borrow_mut() = Some(close());
        frame.relayout();
        let invocation = handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .unwrap()
            .expect("close invocation");
        let requests = handle.session_requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0.to_string(), invocation);
        assert_eq!(requests[0].1.source.heads, vec![[7; 32]]);
        assert_eq!(
            requests[0].1.source.room,
            handle
                .tree
                .borrow()
                .active_workspace()
                .unwrap()
                .room
                .to_string()
        );
        assert!(
            matches!(&requests[0].1.command,DesktopCommand::CloseTab{location} if location.strip==2 && location.group==3 && location.tab.to_string()==route.tab)
        );
        drop(requests);
        assert_eq!(
            serde_json::to_string(&*handle.tree.borrow()).unwrap(),
            before,
            "close request must not remove local topology"
        );

        *handle.navigation_command.borrow_mut() = Some(close());
        frame.relayout();
        assert_eq!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .unwrap()
                .as_deref(),
            Some(invocation.as_str()),
            "repeat close reuses the pending invocation"
        );
        assert_eq!(handle.session_requests.borrow().len(), 1);

        let mut stale_route = route.clone();
        stale_route.tab = "0".into();
        for command in [
            Command::Close {
                generation: handle.generation.clone(),
                route: stale_route,
            },
            Command::Close {
                generation: "stale-generation".into(),
                route: route.clone(),
            },
        ] {
            *handle.navigation_command.borrow_mut() = Some(command);
            frame.relayout();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_err()
            );
        }
        assert_eq!(
            handle.session_requests.borrow().len(),
            1,
            "invalid close must not enqueue a request"
        );
        assert_eq!(
            serde_json::to_string(&*handle.tree.borrow()).unwrap(),
            before
        );
    }
    #[test]
    fn workspace_navigation_document_history_interlock_preserves_exact_owner() {
        use crate::desktop_navigation::Command;
        let (root, handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 600),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        let notes = handle.navigation.borrow()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["label"] == "Notes")
            .unwrap()["route"]
            .clone();
        *handle.navigation_command.borrow_mut() = Some(Command::Select {
            generation: handle.generation.clone(),
            route: serde_json::from_value(notes).unwrap(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        let nav = handle.navigation.borrow().clone();
        *handle.navigation_command.borrow_mut() = Some(serde_json::from_value(serde_json::json!({"kind":"workspace-activity","generation":handle.generation,"workspace":"1","view":"replay"})).unwrap());
        frame.relayout();
        frame.poll_frame().unwrap();
        frame.relayout();
        assert_eq!(
            handle.navigation.borrow()["workspaceActivity"]["view"],
            "replay"
        );
        let mut route: crate::desktop_navigation::Route =
            serde_json::from_value(nav["active"].clone()).unwrap();
        route.workspace = "9007199254740993".into();
        *handle.navigation_command.borrow_mut() = Some(Command::History {
            generation: handle.generation.clone(),
            route,
            target: nav["history"]["target"].as_str().unwrap().into(),
        });
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_err()
        );
        assert!(!handle.navigation.borrow()["workspaceActivity"].is_null());
        let before = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        *handle.navigation_command.borrow_mut() = Some(Command::History {
            generation: handle.generation.clone(),
            route: serde_json::from_value(nav["active"].clone()).unwrap(),
            target: nav["history"]["target"].as_str().unwrap().into(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert!(handle.navigation.borrow()["workspaceActivity"].is_null());
        assert_eq!(
            handle.navigation.borrow()["workspaceActivityCloseReason"],
            "resource-opened"
        );
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("ActivityTimeline")
        );
        assert!(
            !frame
                .inspection_json()
                .unwrap()
                .contains("Workspace activity")
        );
        let after = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(before, after);
        assert!(handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn external_history_opens_retained_framework_without_changing_heads() {
        use crate::desktop_navigation::Command;
        let (root, handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 600),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        // Writer owns its existing app-local history panel; Notes registers the
        // shared framework history which this external entry point exposes.
        assert!(handle.navigation.borrow()["history"].is_null());
        let notes = handle.navigation.borrow()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["label"] == "Notes")
            .unwrap()["route"]
            .clone();
        *handle.navigation_command.borrow_mut() = Some(Command::Select {
            generation: handle.generation.clone(),
            route: serde_json::from_value(notes).unwrap(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        let nav = handle.navigation.borrow().clone();
        assert!(
            !nav["scroll"].is_null(),
            "Notes live editor is the primary region before opening History"
        );
        let before = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        *handle.navigation_command.borrow_mut() = Some(Command::History {
            generation: handle.generation.clone(),
            route: serde_json::from_value(nav["active"].clone()).unwrap(),
            target: nav["history"]["target"].as_str().unwrap().into(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("ActivityTimeline")
        );
        assert!(
            handle.navigation.borrow()["scroll"].is_null(),
            "History owns input until returning to the live editor"
        );
        let after = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                r.store
                    .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(before, after);
    }
    #[test]
    fn external_strips_fill_viewport_and_preserve_inactive_owners_and_scroll() {
        use crate::desktop_navigation::{Command, ScrollAction};
        let (mut root, handle, _) = fixture();
        let mut topology = handle.tree.borrow().clone();
        let notes = topology.workspaces[0].strips[0].window_groups[0]
            .tabs
            .remove(2);
        topology.workspaces[0].strips.push(Strip {
            id: StripId(9),
            name: "Notes strip".into(),
            window_groups: vec![WindowGroup {
                id: 90,
                active_tab: notes.id,
                tabs: vec![notes],
                width: WindowWidth::Normal,
                maximized: false,
            }],
        });
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&topology),
                source_heads: vec![[7; 32]],
                tree: topology,
                routes: root.resolver.host.routes.clone(),
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        root.reconcile();
        let mut root = Box::new(root);
        let ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1000, 420),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        let region = handle.navigation.borrow()["scroll"]["region"]
            .as_str()
            .unwrap()
            .to_string();
        *handle.navigation_command.borrow_mut() = Some(Command::Scroll {
            generation: handle.generation.clone(),
            region: region.clone(),
            action: ScrollAction::End,
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        let offset = handle.navigation.borrow()["scroll"]["offset"]
            .as_f64()
            .unwrap();
        let records = handle.records.borrow().len();
        for (strip, other) in [(9, 2), (2, 9)] {
            *handle.navigation_command.borrow_mut() = Some(Command::Strip {
                generation: handle.generation.clone(),
                workspace: "1".into(),
                strip: strip.to_string(),
            });
            frame.relayout();
            frame.poll_frame().unwrap();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_ok()
            );
            // Frame owns this box; no frame mutation while the references are live.
            let surface = unsafe { &(&*ptr).surface };
            assert!(surface.row(StripId(strip)).unwrap().get_rect().height() > 390.0);
            assert_eq!(
                surface.row(StripId(other)).unwrap().get_rect().height(),
                0.0
            );
            assert_eq!(surface.strip_ids().len(), 2);
            assert_eq!(handle.records.borrow().len(), records);
        }
        assert_eq!(handle.navigation.borrow()["scroll"]["region"], region);
        assert_eq!(handle.navigation.borrow()["scroll"]["offset"], offset);
        assert!(handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn real_caller_palette_enqueues_bound_command_without_mutating_canonical_tree() {
        let (root, handle, _) = fixture();
        let before = serde_json::to_string(&*handle.tree.borrow()).unwrap();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            Box::new(root),
            (1024, 768),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.set_platform_conventions(ui::platform::PlatformConventions::new(
            ui::platform::Platform::Linux,
        ));
        frame.set_modifiers(&["ctrl".into()]);
        frame.key("k");
        frame.set_modifiers(&[]);
        frame.type_text("Open Notes");
        frame.key("Enter");
        let requests = handle.session_requests.borrow();
        assert_eq!(
            requests.len(),
            1,
            "actual palette must dispatch a desktop request"
        );
        assert!(!requests[0].0.is_nil());
        assert_eq!(requests[0].1.source.heads, vec![[7; 32]]);
        assert!(
            matches!(&requests[0].1.command, qos_desktop::session_commands::DesktopCommand::OpenTab { app, .. } if app == "dev.qos.notes")
        );
        assert!(
            frame.take_caller_operation().is_none(),
            "local View queue owns the opaque command"
        );
        assert_eq!(
            serde_json::to_string(&*handle.tree.borrow()).unwrap(),
            before
        );
    }

    #[test]
    fn empty_workspace_palette_enqueues_durable_notes_command() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            tree.workspaces[0].strips.clear();
        });
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            Box::new(root),
            (1024, 768),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.set_platform_conventions(ui::platform::PlatformConventions::new(
            ui::platform::Platform::Linux,
        ));
        frame.set_modifiers(&["ctrl".into()]);
        frame.key("k");
        frame.set_modifiers(&[]);
        frame.type_text("Open Notes");
        frame.key("Enter");
        let requests = handle.session_requests.borrow();
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            &requests[0].1.command,
            qos_desktop::session_commands::DesktopCommand::OpenTab {
                app,
                placement: qos_desktop::session_commands::TabPlacement::EmptyWorkspace,
            } if app == "dev.qos.notes"
        ));
    }
    fn browser_semantic_route(frame: &ui::headless::FrameHarness, label: &str) -> (String, String) {
        fn find(node: &ui::headless::InspectionNode, label: &str) -> Option<(String, String)> {
            fn target(node: &ui::a11y::AccessNode, label: &str) -> Option<String> {
                if node.name.as_deref() == Some(label) && node.action_target.is_some() {
                    return node.action_target.clone();
                }
                node.children.iter().find_map(|node| target(node, label))
            }
            if let Some(target) = node.semantics.as_ref().and_then(|node| target(node, label)) {
                return Some((node.path.clone(), target));
            }
            node.children.iter().find_map(|node| find(node, label))
        }
        find(&frame.inspection().root, label).expect("exported browser semantic route")
    }
    #[test]
    fn desktop_cold_keyboard_focus_reveals_chat_without_prior_writer_focus() {
        let (mut root, handle, _storage) = fixture();
        let mut topology = handle.tree.borrow().clone();
        let groups = &mut topology.workspaces[0].strips[0].window_groups;
        let chat = groups[0].tabs.remove(1);
        groups[0].tabs.pop();
        groups.push(WindowGroup {
            id: 4,
            active_tab: chat.id,
            tabs: vec![chat],
            width: WindowWidth::Normal,
            maximized: false,
        });
        topology.active_strip.insert(topology.active, StripId(2));
        topology.active_group.insert(StripId(2), 3);
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&topology),
                source_heads: vec![[7; 32]],
                tree: topology,
                routes: root.resolver.host.routes.clone(),
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        root.reconcile();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            root,
            (740, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 3);
        let (owner, target) = browser_semantic_route(&frame, "Message composer");
        frame
            .perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus)
            .unwrap();
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(owner.as_str())
        );
        assert_eq!(
            handle.tree.borrow().active_group[&StripId(2)],
            4,
            "authoritative cold Chat focus must outrank retained Writer focus flags"
        );
        unsafe {
            (&mut *root_ptr)
                .surface
                .row_mut(StripId(2))
                .unwrap()
                .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
        }
        frame.relayout();
        let row = unsafe { (&*root_ptr).surface.row(StripId(2)).unwrap() };
        assert!(row.groups[1].focused());
        assert!(row.groups[1].get_rect().x1 <= row.get_rect().x1 + 0.5);
    }

    #[test]
    fn desktop_cold_chat_pointer_and_explicit_focus_own_first_key_and_ime() {
        fn access(
            node: &ui::headless::InspectionNode,
            label: &str,
        ) -> Option<ui::a11y::AccessNode> {
            fn semantic(node: &ui::a11y::AccessNode, label: &str) -> Option<ui::a11y::AccessNode> {
                if node.name.as_deref() == Some(label) {
                    return Some(node.clone());
                }
                node.children
                    .iter()
                    .find_map(|child| semantic(child, label))
            }
            if node.test_id.as_deref() == Some(label) {
                return node.semantics.clone();
            }
            node.semantics
                .as_ref()
                .and_then(|node| semantic(node, label))
                .or_else(|| node.children.iter().find_map(|node| access(node, label)))
        }
        for (pointer, beforeinput, pointer_move) in [
            (true, false, false),
            (false, false, false),
            (true, true, true),
            (true, false, true),
        ] {
            let (mut root, handle, _storage) = fixture();
            let mut topology = handle.tree.borrow().clone();
            let groups = &mut topology.workspaces[0].strips[0].window_groups;
            let chat = groups[0].tabs.remove(1);
            groups[0].tabs.pop();
            groups.push(WindowGroup {
                id: 4,
                active_tab: chat.id,
                tabs: vec![chat],
                width: WindowWidth::Normal,
                maximized: false,
            });
            topology.active_strip.insert(topology.active, StripId(2));
            topology.active_group.insert(StripId(2), 3);
            *handle.inbox.borrow_mut() = Some((
                Admission {
                    admitted_room: history_document(&topology),
                    source_heads: vec![[7; 32]],
                    tree: topology,
                    routes: root.resolver.host.routes.clone(),
                    required: vec![],
                    missing: vec![],
                    unsupported: vec![],
                    pending: 0,
                },
                handle.permissions.borrow().clone(),
            ));
            root.reconcile();
            let mut root = Box::new(root);
            let root_ptr = std::ptr::from_mut(root.as_mut());
            let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
                ui::state::State::new(),
                root,
                (740, 720),
                ui::Theme::dark(),
                ui::TextRenderer::new(),
                ui::permissions::UserId("author".into()),
            );
            assert!(
                access(&frame.inspection().root, "word_processor")
                    .unwrap()
                    .state
                    .focused,
                "cold Writer retains a local focus flag"
            );
            assert!(
                access(&frame.inspection().root, "Message composer")
                    .unwrap()
                    .state
                    .focused,
                "cold Chat already has a local composer caret"
            );
            let before: Vec<_> = handle
                .records
                .borrow()
                .values()
                .filter(|record| record.app != "dev.qos.chat")
                .map(|record| {
                    (
                        record.clone(),
                        record
                            .store
                            .read(&record.checkout.id, |doc| doc.clone().save())
                            .unwrap(),
                    )
                })
                .collect();
            let (owner, target) = browser_semantic_route(&frame, "Message composer");
            if pointer {
                let rect = access(&frame.inspection().root, "Message composer")
                    .unwrap()
                    .bounds
                    .unwrap()
                    .to_rect();
                // The real partially visible composer, without any previous
                // Writer gesture or semantic activation warming its flags.
                if pointer_move {
                    // Real pointer streams can move while pressed without
                    // selecting any text, especially in an empty composer.
                    use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
                    let moved = WindowEvent::CursorMoved {
                        device_id: DeviceId::dummy(),
                        position: winit::dpi::PhysicalPosition::new(
                            (rect.x0 + 740.0) / 2.0,
                            rect.center().y,
                        ),
                    };
                    frame.send_host_event(&moved);
                    frame.send_host_event(&WindowEvent::MouseInput {
                        device_id: DeviceId::dummy(),
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                    });
                    frame.send_host_event(&moved);
                    frame.send_host_event(&WindowEvent::MouseInput {
                        device_id: DeviceId::dummy(),
                        state: ElementState::Released,
                        button: MouseButton::Left,
                    });
                } else {
                    frame.click_at((rect.x0 + 740.0) / 2.0, rect.center().y);
                }
            } else {
                frame
                    .perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus)
                    .unwrap();
            }
            assert_eq!(
                frame.inspection().keyboard_focus_owner.as_deref(),
                Some(owner.as_str()),
                "cold pointer={pointer}"
            );
            assert!(
                !access(&frame.inspection().root, "word_processor")
                    .unwrap()
                    .state
                    .focused,
                "the previous visible group must lose its focused control"
            );
            assert!(
                frame.frame_output().ime.is_some(),
                "cold Chat must expose its actual IME owner"
            );
            // Exercise both ordinary keys and the hidden editor's beforeinput
            // route, which delivers committed text through IME.
            if beforeinput {
                use winit::event::{Ime, Modifiers, WindowEvent};
                frame.send_host_event(&WindowEvent::Focused(true));
                frame.send_host_event(&WindowEvent::Ime(Ime::Enabled));
                // Printable hidden-editor keys publish modifier snapshots,
                // but the text itself is delivered only by beforeinput/IME.
                frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::default()));
                frame.send_host_event(&WindowEvent::Ime(Ime::Commit("N".into())));
                frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::default()));
            } else {
                frame.key("N");
            }
            assert_eq!(
                access(&frame.inspection().root, "Message composer")
                    .unwrap()
                    .value
                    .as_deref(),
                Some("N")
            );
            let selection = access(&frame.inspection().root, "Message composer")
                .unwrap()
                .text_selection
                .unwrap();
            assert_eq!(
                (selection.anchor, selection.focus),
                (1, 1),
                "first insertion must leave a collapsed caret"
            );
            if pointer_move {
                if beforeinput {
                    frame.send_host_event(&winit::event::WindowEvent::Ime(
                        winit::event::Ime::Commit("x".into()),
                    ));
                } else {
                    frame.key("x");
                }
                assert_eq!(
                    access(&frame.inspection().root, "Message composer")
                        .unwrap()
                        .value
                        .as_deref(),
                    Some("Nx")
                );
                frame.key("Backspace");
            }
            frame.type_text("avigation two-peer check");
            for event in [
                winit::event::WindowEvent::Focused(true),
                winit::event::WindowEvent::Ime(winit::event::Ime::Enabled),
                winit::event::WindowEvent::Ime(winit::event::Ime::Commit(" IME".into())),
                winit::event::WindowEvent::Ime(winit::event::Ime::Disabled),
                winit::event::WindowEvent::Focused(false),
                winit::event::WindowEvent::Focused(true),
                winit::event::WindowEvent::Ime(winit::event::Ime::Enabled),
                winit::event::WindowEvent::Ime(winit::event::Ime::Commit(" resumed".into())),
            ] {
                frame.send_host_event(&event);
            }
            assert_eq!(
                access(&frame.inspection().root, "Message composer")
                    .unwrap()
                    .value
                    .as_deref(),
                Some("Navigation two-peer check IME resumed")
            );
            assert_eq!(
                frame.inspection().keyboard_focus_owner.as_deref(),
                Some(owner.as_str())
            );
            assert!(frame.frame_output().ime.is_some());
            assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 4);
            for (record, bytes) in before {
                assert_eq!(
                    record
                        .store
                        .read(&record.checkout.id, |doc| doc.clone().save())
                        .unwrap(),
                    bytes,
                    "cold pointer={pointer} typing must not mutate {}",
                    record.app
                );
            }
            // SAFETY: the boxed root stays in the harness and this access ends before its next call.
            unsafe {
                (&mut *root_ptr)
                    .surface
                    .row_mut(StripId(2))
                    .unwrap()
                    .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
            }
            frame.relayout();
        }
    }

    #[test]
    fn desktop_navigation_reveals_real_chat_and_preserves_local_records_and_return_focus() {
        let (mut root, handle, storage) = fixture();
        let mut topology = handle.tree.borrow().clone();
        let groups = &mut topology.workspaces[0].strips[0].window_groups;
        let chat = groups[0].tabs.remove(1);
        groups[0].tabs.pop();
        groups.push(WindowGroup {
            id: 4,
            active_tab: chat.id,
            tabs: vec![chat],
            width: WindowWidth::Normal,
            maximized: false,
        });
        topology.active_strip.insert(topology.active, StripId(2));
        topology.active_group.insert(StripId(2), 4);
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&topology),
                source_heads: vec![[7; 32]],
                tree: topology,
                routes: root.resolver.host.routes.clone(),
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        root.reconcile();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            root,
            (740, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        assert_eq!(
            handle.tree.borrow().active_group[&StripId(2)],
            4,
            "mounting retained apps is not a new local focus gesture"
        );
        fn rect(node: &ui::headless::InspectionNode, label: &str) -> Option<Rect> {
            fn semantic(node: &ui::a11y::AccessNode, label: &str) -> Option<Rect> {
                if node.name.as_deref() == Some(label) {
                    return node.bounds.map(|bounds| bounds.to_rect());
                }
                node.children
                    .iter()
                    .find_map(|child| semantic(child, label))
            }
            node.semantics
                .as_ref()
                .and_then(|node| semantic(node, label))
                .or_else(|| node.children.iter().find_map(|child| rect(child, label)))
        }
        // SAFETY: frame retains the boxed root; each exclusive access ends
        // before invoking a harness method or using another root reference.
        let settle = |frame: &mut ui::headless::FrameHarness| {
            unsafe {
                (&mut *root_ptr)
                    .surface
                    .row_mut(StripId(2))
                    .unwrap()
                    .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
            }
            frame.relayout();
        };
        frame.click_named("word_processor").unwrap();
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "Retained draft".into(),
        )));
        settle(&mut frame);
        let before: Vec<_> = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                (
                    r.clone(),
                    r.store
                        .read(&r.checkout.id, |doc| doc.clone().save())
                        .unwrap(),
                )
            })
            .collect();
        let writer_owner = frame.inspection().keyboard_focus_owner.unwrap();
        let (composer, _) = browser_semantic_route(&frame, "Message composer");
        let composer_rect = rect(&frame.inspection().root, "Message composer").unwrap();
        assert!(
            composer_rect.x0 < 740.0 && composer_rect.x1 > 740.0,
            "real narrow-panel precondition: {composer_rect:?}"
        );
        // Physical pointer at the visible part of the existing composer.
        frame.click_at((composer_rect.x0 + 740.0) / 2.0, composer_rect.center().y);
        frame.poll_frame().unwrap();
        settle(&mut frame);
        assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 4);
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(composer.as_str())
        );
        let send_rect = rect(&frame.inspection().root, "Send message").unwrap();
        assert!(
            send_rect.x1 <= 740.0 && send_rect.x0 >= 0.0,
            "Send must be usable: {send_rect:?}"
        );
        unsafe {
            let row = (&*root_ptr).surface.row(StripId(2)).unwrap();
            assert!(row.groups[1].focused());
            assert!(!row.groups[0].focused());
            let saved: LocalView =
                serde_json::from_slice(&storage.get(&(&*root_ptr).view_key).unwrap().bytes)
                    .unwrap();
            assert_eq!(saved.groups[&StripId(2)], 4);
        }
        for (record, bytes) in &before {
            assert_eq!(
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
                *bytes,
                "pointer navigation alone must not alter shared records"
            );
        }
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "Chat pointer draft".into(),
        )));
        let chat = handle
            .records
            .borrow()
            .values()
            .find(|record| record.app == "dev.qos.chat")
            .unwrap()
            .clone();
        assert!(
            chat.store
                .read(&chat.checkout.id, |doc| serde_json::to_string(
                    &automerge::AutoSerde::from(doc)
                )
                .unwrap())
                .unwrap()
                .contains("Chat pointer draft")
        );
        for (record, bytes) in &before {
            if record.app != "dev.qos.chat" {
                assert_eq!(
                    record
                        .store
                        .read(&record.checkout.id, |doc| doc.clone().save())
                        .unwrap(),
                    *bytes,
                    "Chat typing must not reach the previously focused Writer"
                );
            }
        }
        let before: Vec<_> = handle
            .records
            .borrow()
            .values()
            .map(|r| {
                (
                    r.clone(),
                    r.store
                        .read(&r.checkout.id, |doc| doc.clone().save())
                        .unwrap(),
                )
            })
            .collect();
        // Keyboard/semantic focus returns to the same retained editor and caret.
        frame
            .perform_semantic_named(&writer_owner, None, &ui::a11y::Action::Focus)
            .unwrap();
        frame.poll_frame().unwrap();
        settle(&mut frame);
        assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 3);
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(writer_owner.as_str())
        );
        assert!(frame.inspection_json().unwrap().contains("Retained draft"));
        for (record, bytes) in before {
            assert_eq!(
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
                bytes,
                "navigation must not alter {} shared bytes",
                record.app
            );
        }
        assert!(handle.session_requests.borrow().is_empty());
        assert!(handle.errors.borrow().is_empty());
        // A local group shortcut may leave Writer's retained caret suspended.
        // A subsequent remote strip insertion must not turn it into new input.
        handle.tree.borrow_mut().active_group.insert(StripId(2), 4);
        frame.relayout();
        settle(&mut frame);
        let mut refreshed = handle.tree.borrow().clone();
        refreshed.workspaces[0].strips.push(Strip {
            id: StripId(99),
            name: "Remote strip".into(),
            window_groups: vec![],
        });
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&refreshed),
                source_heads: vec![[7; 32]],
                tree: refreshed,
                routes: unsafe { (&*root_ptr).resolver.host.routes.clone() },
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        frame.relayout();
        frame.relayout();
        settle(&mut frame);
        assert_eq!(
            handle.tree.borrow().active_group[&StripId(2)],
            4,
            "retained Writer caret is not a new focus gesture after remote rebuild"
        );
    }

    #[test]
    fn desktop_group_focus_selects_its_strip_and_rejects_stale_identity() {
        let (root, handle, _) = fixture();
        let mut tree = handle.tree.borrow().clone();
        let mut second = tree.workspaces[0].strips[0].clone();
        second.id = StripId(9);
        second.window_groups[0].id = 90;
        tree.workspaces[0].strips.push(second);
        DesktopRoot::local(
            &mut tree,
            LocalViewIntent::Group {
                strip: StripId(9),
                group: 90,
            },
        );
        assert_eq!(tree.active_strip[&tree.active], StripId(9));
        assert_eq!(tree.active_group[&StripId(9)], 90);
        let before = serde_json::to_vec(&tree).unwrap();
        DesktopRoot::local(
            &mut tree,
            LocalViewIntent::Group {
                strip: StripId(2),
                group: 90,
            },
        );
        assert_eq!(serde_json::to_vec(&tree).unwrap(), before);
        assert!(root.handle.session_requests.borrow().is_empty());
    }

    #[test]
    fn browser_external_tab_activation_updates_local_selection_and_preserves_real_input() {
        let (root, handle, _) = fixture();
        let mut topology = handle.tree.borrow().clone();
        let groups = &mut topology.workspaces[0].strips[0].window_groups;
        let chat = groups[0].tabs.remove(1);
        groups[0].tabs.pop();
        groups.push(WindowGroup {
            id: 4,
            active_tab: chat.id,
            tabs: vec![chat],
            width: WindowWidth::Normal,
            maximized: false,
        });
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&topology),
                source_heads: vec![[7; 32]],
                tree: topology,
                routes: root.resolver.host.routes.clone(),
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            root,
            (1280, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.poll_frame().unwrap();
        for tab in [(5 << 48) | 12, (5 << 48) | 10] {
            *handle.navigation_command.borrow_mut() =
                Some(crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: crate::desktop_navigation::Route {
                        workspace: "1".into(),
                        strip: "2".into(),
                        group: "3".into(),
                        tab: tab.to_string(),
                    },
                });
            frame.relayout();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_ok()
            );
            assert_eq!(
                handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
                tab
            );
        }
        assert_eq!(
            unsafe {
                (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                    .children()
                    .len()
            },
            1,
            "external navigation omits native tab semantics"
        );
        let chat = handle
            .records
            .borrow()
            .values()
            .find(|record| record.app == "dev.qos.chat")
            .unwrap()
            .clone();
        let chat_before = chat
            .store
            .read(&chat.checkout.id, |doc| doc.clone().save())
            .unwrap();
        fn writer_editable_rect(node: &ui::headless::InspectionNode) -> Option<Rect> {
            if node.test_id.as_deref() == Some("word_processor") && node.focusable {
                return Some(Rect::new(
                    node.rect[0],
                    node.rect[1],
                    node.rect[2],
                    node.rect[3],
                ));
            }
            node.children.iter().find_map(writer_editable_rect)
        }
        let writer_rect = writer_editable_rect(&frame.inspection().root)
            .expect("real Writer editable coordinate");
        frame.click_at(writer_rect.center().x, writer_rect.center().y);
        assert!(
            frame.frame_output().ime.is_some(),
            "clicked Writer must own the native IME route"
        );
        frame.send_host_event(&winit::event::WindowEvent::Focused(true));
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Enabled));
        frame.send_host_event(&winit::event::WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
        // Browser beforeinput forwards committed text through this native event.
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "Two-group pointer Writer".into(),
        )));
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|record| record.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        assert!(
            writer
                .store
                .read(&writer.checkout.id, |doc| serde_json::to_string(
                    &automerge::AutoSerde::from(doc)
                )
                .unwrap())
                .unwrap()
                .contains("Two-group pointer Writer")
        );
        assert_eq!(
            chat.store
                .read(&chat.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            chat_before,
            "Writer input must not mutate Chat"
        );
        let writer_before = writer
            .store
            .read(&writer.checkout.id, |doc| doc.clone().save())
            .unwrap();
        let (composer, composer_target) = browser_semantic_route(&frame, "Message composer");
        frame
            .perform_semantic_named(&composer, Some(&composer_target), &ui::a11y::Action::Focus)
            .unwrap();
        frame
            .perform_semantic_named(
                &composer,
                Some(&composer_target),
                &ui::a11y::Action::SetValue("Two-group Chat draft".into()),
            )
            .unwrap();
        frame.key("Enter");
        assert!(
            chat.store
                .read(&chat.checkout.id, |doc| serde_json::to_string(
                    &automerge::AutoSerde::from(doc)
                )
                .unwrap())
                .unwrap()
                .contains("Two-group Chat draft")
        );
        assert_eq!(
            writer
                .store
                .read(&writer.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            writer_before,
            "Chat input must not mutate Writer"
        );
        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: crate::desktop_navigation::Route {
                    workspace: "1".into(),
                    strip: "2".into(),
                    group: "3".into(),
                    tab: ((5_u64 << 48) | 12).to_string(),
                },
            });
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        frame.poll_frame().unwrap();
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            (5 << 48) | 12,
            "external Notes selection"
        );
    }
    #[test]
    fn actual_desktop_hosts_checked_writer_chat_notes_calculator_and_preserves_writer_route() {
        let (root, handle, cache) = fixture();
        let mut frame =
            ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(root), (1280, 720));
        frame.poll_frame().unwrap();
        assert_eq!(handle.records.borrow().len(), 4);
        assert!(
            frame
                .resources()
                .iter()
                .any(|r| r.kind == "word_processor.document")
        );
        frame
            .perform_named("word_processor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("Retained canonical desktop");
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        assert_eq!(writer.checkout.id.as_str(), "tab:0000000000000001");
        assert!(
            writer
                .store
                .read(&writer.checkout.id, |doc| serde_json::to_string(
                    &automerge::AutoSerde::from(doc)
                )
                .unwrap())
                .unwrap()
                .contains("Retained canonical desktop")
        );
        for (instance, resource, action, value) in [
            (
                (5 << 48) | 11,
                "chat.channel:c_general",
                "post",
                "Desktop message",
            ),
            ((5 << 48) | 12, "notes.notebook", "new-page", "Desktop page"),
            (
                (5 << 48) | 13,
                "calculator.workspace",
                "add-function",
                "sin(x)",
            ),
        ] {
            frame
                .invoke_bound_action(
                    &ui::bound_dispatch::BoundActionAddress {
                        instance_id: instance,
                        address: tilo_actions::ActionAddress::new(resource, action),
                    },
                    serde_json::json!({"value":value}).as_object().unwrap(),
                )
                .unwrap();
        }
        frame.close_components();
        let mut reports = frame.take_component_close_reports();
        assert_eq!(reports.len(), 4);
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        for report in &mut reports {
            report.retry_record().unwrap();
            report.retry_view().unwrap();
        }
    }
    /// Mount the desktop fixture with the framing animation settled and the
    /// Writer focused, so the next frame is an ordinary retained one.
    pub(super) fn settled_writer_frame() -> (ui::headless::FrameHarness, DesktopHandle) {
        let (root, handle, _) = fixture();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1280, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        settle_external_navigation(root_ptr, &mut frame);
        frame
            .perform_named("word_processor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("a");
        frame.relayout();
        (frame, handle)
    }

    #[test]
    fn writer_typing_retains_the_desktop_layout_tree() {
        let (mut frame, handle) = settled_writer_frame();
        let ids = frame.layout_node_ids();
        let focused = frame.focused();
        assert!(focused.is_some(), "the Writer owns keyboard focus");
        frame.set_retained_geometry_check(1);
        frame.type_text("b");
        let stats = frame.last_relayout();
        assert!(
            !stats.rebuilt,
            "a typed key must not recompose the desktop: {stats:?}"
        );
        assert_eq!(frame.layout_node_ids(), ids, "node ids survive a typed key");
        assert_eq!(frame.focused(), focused, "focus survives a typed key");
        assert!(
            frame
                .perform_named("word_processor", &ui::a11y::Action::Focus)
                .is_ok(),
            "bindings still resolve the Writer after a retained frame"
        );
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        assert!(
            writer
                .store
                .read(&writer.checkout.id, |doc| serde_json::to_string(
                    &automerge::AutoSerde::from(doc)
                )
                .unwrap())
                .unwrap()
                .contains("ab"),
            "the retained frame still committed the typed text"
        );
        frame.verify_retained_geometry().unwrap();
    }

    #[test]
    fn writer_typing_re_measures_no_desktop_leaf() {
        // The Writer fills its tile and draws itself, the tab bar has a fixed
        // height and every other host is inactive: a typed key changes no
        // size anywhere, so taffy has nothing to re-measure.
        let (mut frame, _handle) = settled_writer_frame();
        frame.type_text("b");
        let stats = frame.last_relayout();
        assert!(!stats.rebuilt, "{stats:?}");
        assert_eq!(
            stats.marked_leaves, 0,
            "a typed key re-measures no leaf: {stats:?}"
        );
        assert!(
            !stats.measured,
            "a typed key needs no taffy re-solve: {stats:?}"
        );
    }

    #[test]
    fn pointer_motion_over_each_app_body_is_inert_once_settled() {
        // Stage 3 follow-up: a pointer resting over any app must not keep
        // the desktop repainting. The first move may change hover; a second
        // move inside the same spot must be skipped, and when it is not the
        // dirty paths name the widget that still marks itself.
        let (mut frame, handle) = settled_writer_frame();
        frame.set_pointer_redraw_diagnostics(true);
        let device_id = winit::event::DeviceId::dummy();
        let move_to = |x: f64, y: f64| winit::event::WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(x, y),
        };
        for (label, tab) in [
            ("word_processor", (5u64 << 48) | 10),
            ("chat", (5u64 << 48) | 11),
            ("notes", (5u64 << 48) | 12),
            ("calculator", (5u64 << 48) | 13),
        ] {
            *handle.navigation_command.borrow_mut() =
                Some(crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: crate::desktop_navigation::Route {
                        workspace: "1".into(),
                        strip: "2".into(),
                        group: "3".into(),
                        tab: tab.to_string(),
                    },
                });
            frame.relayout();
            assert!(
                handle
                    .navigation_result
                    .borrow_mut()
                    .take()
                    .unwrap()
                    .is_ok(),
                "{label}: selection accepted"
            );
            // Selection animations (group framing, the Notes status bubble)
            // are finite; let them run out before judging idleness.
            std::thread::sleep(
                ui::status_bubble::DEFAULT_LINGER + ui::clock::Duration::from_millis(10),
            );
            for _ in 0..4 {
                frame.relayout();
            }
            // Well inside the tile body, below the tab bar.
            frame.send_host_event(&move_to(640.0, 420.0));
            frame.take_pointer_redraw_sources();
            frame.send_host_event(&move_to(643.0, 422.0));
            let sources = frame.take_pointer_redraw_sources();
            assert!(
                frame.last_send_skipped_relayout(),
                "{label}: settled pointer motion over the app body still produced a frame; dirty paths: {sources:?}"
            );
        }
    }

    #[test]
    fn desktop_resize_rebuilds_and_matches_a_fresh_composition() {
        let (mut frame, _handle) = settled_writer_frame();
        let ids = frame.layout_node_ids();
        frame.resize((1100, 700)).unwrap();
        assert!(
            frame.last_relayout().rebuilt,
            "a viewport change recomposes the desktop"
        );
        assert_eq!(
            frame.layout_node_ids().len(),
            ids.len(),
            "the same composition at a new size"
        );
        let rebuilt = frame.layout_node_ids();
        frame.type_text("c");
        assert!(
            !frame.last_relayout().rebuilt,
            "typing after the resize retains the new tree"
        );
        assert_eq!(frame.layout_node_ids(), rebuilt);
        frame.verify_retained_geometry().unwrap();
    }

    #[test]
    fn desktop_tab_selection_rebuilds_the_composition() {
        let (mut frame, handle) = settled_writer_frame();
        let ids = frame.layout_node_ids();
        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: crate::desktop_navigation::Route {
                    workspace: "1".into(),
                    strip: "2".into(),
                    group: "3".into(),
                    tab: ((5u64 << 48) | 12).to_string(),
                },
            });
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        // Selection may settle over more than one frame; the composition must
        // have been rebuilt by the time Notes is the active host.
        let mut rebuilt = frame.last_relayout().rebuilt;
        for _ in 0..3 {
            if rebuilt {
                break;
            }
            frame.relayout();
            rebuilt |= frame.last_relayout().rebuilt;
        }
        assert!(rebuilt, "activating another tab is a structural change");
        assert_ne!(
            frame.layout_node_ids().len(),
            ids.len(),
            "the active host swapped its subtree for a differently shaped one"
        );
        frame.relayout();
        frame.verify_retained_geometry().unwrap();
    }

    #[test]
    fn admitted_tab_close_retains_unflushed_component_owner_until_retry() {
        let (root, handle, cache) = fixture();
        let mut topology = handle.tree.borrow().clone();
        let routes = root.resolver.host.routes.clone();
        let mut frame =
            ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(root), (1280, 720));
        frame
            .perform_named("word_processor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("Unflushed close custody");
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        topology.workspaces[0].strips[0].window_groups[0]
            .tabs
            .remove(0);
        topology.workspaces[0].strips[0].window_groups[0].active_tab = (5 << 48) | 11;
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&topology),
                source_heads: vec![[8; 32]],
                tree: topology,
                routes,
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        frame.poll_frame().unwrap();
        let mut reports = frame.take_component_close_reports();
        assert!(
            !reports.is_empty(),
            "admitted close transfers real component custody"
        );
        for write in cache.pending_writes() {
            cache
                .acknowledge(&write, Err("disk denied".into()))
                .unwrap();
        }
        assert!(
            reports.iter_mut().any(|r| r.retry_record().is_err()),
            "failed durable flush remains retryable"
        );
        assert!(
            writer
                .store
                .read(&writer.checkout.id, |doc| serde_json::to_string(
                    &automerge::AutoSerde::from(doc)
                )
                .unwrap())
                .unwrap()
                .contains("Unflushed close custody")
        );
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        for report in &mut reports {
            report.retry_record().unwrap();
        }
        assert!(reports.iter().all(|r| r.record_retry.is_none()));
    }
    #[test]
    fn admitted_same_workspace_refresh_keeps_pending_selection_and_checked_owner() {
        let (mut root, handle, _) = fixture();
        let before = match root.surface.row_mut(StripId(2)).unwrap().groups[0]
            .host_for_tab_mut((5 << 48) | 10)
            .unwrap()
            .content()
        {
            ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()) as *const (),
            _ => panic!("checked owner"),
        };
        let mut refreshed = handle.tree.borrow().clone();
        refreshed.workspaces[0].strips.push(Strip {
            id: StripId(99),
            name: "Remote strip".into(),
            window_groups: vec![],
        });
        let admission = Admission {
            admitted_room: history_document(&refreshed),
            source_heads: vec![[7; 32]],
            tree: refreshed,
            routes: root.resolver.host.routes.clone(),
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        };
        *handle.inbox.borrow_mut() = Some((admission, handle.permissions.borrow().clone()));
        root.surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab((5 << 48) | 12);
        root.reconcile();
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            (5 << 48) | 12
        );
        let after = match root.surface.row_mut(StripId(2)).unwrap().groups[0]
            .host_for_tab_mut((5 << 48) | 10)
            .unwrap()
            .content()
        {
            ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()) as *const (),
            _ => panic!("checked owner"),
        };
        assert_eq!(before, after);
        assert!(root.surface.take_component_close_reports().is_empty());
    }
    #[test]
    fn pending_tab_choice_does_not_undo_a_newer_workspace_switch() {
        let (mut root, handle, _) = fixture();
        let new = Workspace::new(WorkspaceId(9), "Other");
        {
            let mut tree = handle.tree.borrow_mut();
            tree.workspaces.push(new);
            tree.active = WorkspaceId(9);
        }
        root.surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab((5 << 48) | 12);
        root.reconcile();
        let tree = handle.tree.borrow();
        assert_eq!(tree.active, WorkspaceId(9));
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups[0].active_tab,
            (5 << 48) | 12
        );
        assert!(
            !tree.active_group.contains_key(&StripId(2)),
            "outgoing selection must not steal focus"
        );
    }
    #[test]
    fn global_search_real_catalog_launch_uses_bound_queue() {
        let (root, handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            Box::new(root),
            (1000, 700),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        handle.sync_global_search(&mut frame);
        let data = handle.search.borrow().data.clone();
        assert_eq!(
            data.applications
                .iter()
                .map(|a| a.app_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "dev.qos.word-processor",
                "dev.qos.chat",
                "dev.qos.notes",
                "dev.qos.calculator",
            ]
        );
        assert_eq!(data.tabs.len(), 4);
        assert_eq!(
            handle.navigation.borrow()["capabilities"]["globalSearch"],
            true
        );
        let before = serde_json::to_value(&handle.tree.borrow().workspaces).unwrap();
        let record_heads = handle
            .records
            .borrow()
            .values()
            .map(|record| {
                record
                    .store
                    .with(&record.checkout.id, |doc| doc.get_heads())
            })
            .collect::<Vec<_>>();
        handle
            .open_global_search(&mut frame, &handle.generation, "Notes")
            .unwrap();
        assert_eq!(
            frame.global_search_scope(),
            Some(ui::search::system::SearchScope::All)
        );
        let node = frame.inspection().root.semantics.unwrap();
        let target = node.children[1].children[0]
            .action_target
            .as_deref()
            .unwrap();
        frame
            .perform_semantic_named("system-search", Some(target), &ui::a11y::Action::Activate)
            .unwrap();
        assert!(handle.session_requests.borrow().is_empty());
        handle.process_global_search(&mut frame).unwrap();
        let requests = handle.session_requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.source.heads, vec![[7; 32]]);
        assert!(
            matches!(&requests[0].1.command,qos_desktop::session_commands::DesktopCommand::OpenTab{app,..} if app=="dev.qos.notes")
        );
        assert_eq!(
            serde_json::to_value(&handle.tree.borrow().workspaces).unwrap(),
            before
        );
        assert_eq!(
            handle
                .records
                .borrow()
                .values()
                .map(|record| record
                    .store
                    .with(&record.checkout.id, |doc| doc.get_heads()))
                .collect::<Vec<_>>(),
            record_heads
        );
        assert!(handle.open_global_search(&mut frame, "stale", "").is_err());
    }

    #[test]
    fn global_search_real_notes_and_chat_body_hits_select_exact_admitted_tab() {
        use notes::{doc::NotesDoc, model::PageMeta};
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .enumerate()
                .map(|(i, tab)| WindowGroup {
                    id: 100 + i as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect();
        });
        for record in handle.records.borrow().values() {
            record.store.with(&record.checkout.id, |doc| {
                if record.app == "dev.qos.notes" {
                    let page = uuid::Uuid::new_v4();
                    NotesDoc::upsert_page_meta(
                        doc,
                        &PageMeta {
                            id: page,
                            title: "Ordinary notebook page".into(),
                            parent: None,
                            order: 0,
                        },
                    );
                    NotesDoc::apply_block_ops(
                        doc,
                        page,
                        &[ui::widget::BlockEditOp::Insert {
                            block_id: uuid::Uuid::new_v4().to_string(),
                            after: None,
                            kind: "paragraph".into(),
                            text: "quasar-notes-body-only".into(),
                        }],
                    )
                    .unwrap();
                    notes::notebook::validate(doc).unwrap();
                } else if record.app == "dev.qos.chat" {
                    chat::doc::ChatDoc::push_message(
                        doc,
                        "c_general",
                        chat::doc::NewMessage {
                            author: "author".into(),
                            ts: 1,
                            text: "nebula-chat-body-only".into(),
                            ..Default::default()
                        },
                    );
                }
            });
        }
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1100, 700),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        for (app, query) in [
            ("dev.qos.notes", "quasar-notes-body-only"),
            ("dev.qos.chat", "nebula-chat-body-only"),
        ] {
            handle
                .open_global_search(&mut frame, &handle.generation, query)
                .unwrap();
            let data = handle.search.borrow().data.clone();
            let expected = data.tabs.iter().find(|tab| tab.app_id == app).unwrap();
            assert!(!expected.title.contains(query));
            let node = frame.inspection().root.semantics.unwrap();
            let results = &node.children[1].children;
            assert!(
                !results.is_empty(),
                "no real provider body hit for {app}: {}",
                frame.inspection_json().unwrap()
            );
            let target = results[0].action_target.as_deref().unwrap();
            frame
                .perform_semantic_named("system-search", Some(target), &ui::a11y::Action::Activate)
                .unwrap();
            handle.process_global_search(&mut frame).unwrap();
            assert_eq!(
                handle.navigation.borrow()["active"]["tab"],
                expected.instance_id.to_string()
            );
            assert!(handle.session_requests.borrow().is_empty());
        }
        handle
            .open_global_search(&mut frame, &handle.generation, "nebula-chat-body-only")
            .unwrap();
        let old_target = frame.inspection().root.semantics.unwrap().children[1].children[0]
            .action_target
            .clone()
            .unwrap();
        *handle.permissions.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(None, "outsider"),
            vec![],
        );
        frame.relayout();
        assert!(
            frame
                .perform_semantic_named(
                    "system-search",
                    Some(&old_target),
                    &ui::a11y::Action::Activate
                )
                .is_err()
        );
        assert!(frame.take_global_search_intent().is_none());
    }

    #[test]
    fn global_search_refresh_revokes_exact_routes_and_stale_semantics() {
        let (root, handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 700),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        handle
            .open_global_search(&mut frame, &handle.generation, "Notes")
            .unwrap();
        let old = handle.search.borrow().data.clone();
        let tab = old
            .tabs
            .iter()
            .find(|t| t.app_id == "dev.qos.notes")
            .unwrap();
        let row = frame.inspection().root.semantics.unwrap().children[1].children[0]
            .action_target
            .clone()
            .unwrap();
        let mut tree = handle.tree.borrow().clone();
        for w in &mut tree.workspaces {
            for s in &mut w.strips {
                for g in &mut s.window_groups {
                    g.tabs.retain(|t| t.id != tab.instance_id);
                }
            }
        }
        let routes = BTreeMap::from([(
            old.tabs
                .iter()
                .find(|t| t.app_id == "dev.qos.word-processor")
                .unwrap()
                .instance_id,
            "tab:1".into(),
        )]);
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&tree),
                source_heads: vec![[8; 32]],
                tree,
                routes,
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        frame.relayout();
        handle.sync_global_search(&mut frame);
        assert!(
            !handle
                .search
                .borrow()
                .data
                .tabs
                .iter()
                .any(|t| t.instance_id == tab.instance_id)
        );
        assert!(
            handle
                .search
                .borrow()
                .resolve(&old.revision, &tab.target)
                .is_err()
        );
        let current = handle.search.borrow().data.revision.clone();
        assert!(
            handle
                .search
                .borrow()
                .resolve(&current, &tab.target)
                .is_err()
        );
        assert!(
            frame
                .perform_semantic_named("system-search", Some(&row), &ui::a11y::Action::Activate)
                .is_err()
        );
        assert!(handle.session_requests.borrow().is_empty());
        handle
            .open_global_search(
                &mut frame,
                &handle.generation,
                &"é".repeat(ui::search::MAX_SEARCH_QUERY_SCALARS),
            )
            .unwrap();
        assert!(
            handle
                .open_global_search(
                    &mut frame,
                    &handle.generation,
                    &"x".repeat(ui::search::MAX_SEARCH_QUERY_SCALARS + 1)
                )
                .is_err()
        );
    }
    #[test]
    fn explicit_selection_routes_ime_to_chat_and_restores_writer() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .enumerate()
                .map(|(index, tab)| WindowGroup {
                    id: 100 + index as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect();
        });
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1800, 900),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.click_named("word_processor").unwrap();
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "retained draft".into(),
        )));
        frame.key("ArrowLeft");
        let writer_owner = frame.inspection().keyboard_focus_owner.unwrap();
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        let writer_before = writer
            .store
            .read(&writer.checkout.id, |doc| doc.clone().save())
            .unwrap();
        let (chat_owner, _) = browser_semantic_route(&frame, "Message composer");
        let route = |label: &str| {
            serde_json::from_value::<crate::desktop_navigation::Route>(
                handle.navigation.borrow()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["label"] == label)
                    .unwrap()["route"]
                    .clone(),
            )
            .unwrap()
        };
        let chat = route("Chat");
        let writer_route = route("Writer");
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: chat.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(chat_owner.as_str())
        );
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "destination input".into(),
        )));
        assert_eq!(
            writer
                .store
                .read(&writer.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            writer_before
        );
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("destination input")
        );
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: writer_route.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(writer_owner.as_str())
        );
        let sequence = frame.input_ownership_sequence();
        let activation = handle.activation_sequence.get();
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: writer_route.clone(),
                },
            )
            .unwrap();
        assert!(frame.input_ownership_sequence() > sequence);
        assert_eq!(handle.activation_sequence.get(), activation + 1);
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "X".into(),
        )));
        let writer_text = writer
            .store
            .read(&writer.checkout.id, |doc| {
                serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
            })
            .unwrap();
        assert!(
            writer_text.contains("retained drafXt"),
            "restored and reselected caret: {writer_text}"
        );
        let before = handle.navigation.borrow()["active"].clone();
        let activation = handle.activation_sequence.get();
        assert!(
            handle
                .navigate(
                    &mut frame,
                    crate::desktop_navigation::Command::Select {
                        generation: "stale".into(),
                        route: chat.clone(),
                    }
                )
                .is_err()
        );
        assert_eq!(handle.navigation.borrow()["active"], before);
        assert_eq!(handle.activation_sequence.get(), activation);
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(writer_owner.as_str())
        );
        handle
            .open_global_search(&mut frame, &handle.generation, "")
            .unwrap();
        assert!(
            handle
                .navigate(
                    &mut frame,
                    crate::desktop_navigation::Command::Select {
                        generation: handle.generation.clone(),
                        route: chat.clone(),
                    }
                )
                .is_err(),
            "Search remains a modal owner"
        );
        assert_eq!(handle.navigation.borrow()["active"], before);
        frame.key("Escape");
        handle.process_global_search(&mut frame).unwrap();
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(writer_owner.as_str())
        );
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: chat.clone(),
                },
            )
            .unwrap();
        let writer_before_removal = writer
            .store
            .read(&writer.checkout.id, |doc| doc.clone().save())
            .unwrap();
        let mut tree = handle.tree.borrow().clone();
        tree.workspaces[0].strips[0]
            .window_groups
            .retain(|group| group.id.to_string() != chat.group);
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&tree),
                source_heads: vec![[8; 32]],
                tree,
                routes: BTreeMap::from([(
                    writer_route.ids().unwrap().3,
                    writer.checkout.id.as_str().into(),
                )]),
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        frame.relayout();
        assert!(
            frame.inspection().keyboard_focus_owner.is_none(),
            "removed target cannot resurrect Writer"
        );
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "must not leak".into(),
        )));
        assert_eq!(
            writer
                .store
                .read(&writer.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            writer_before_removal
        );
        assert!(
            handle
                .navigate(
                    &mut frame,
                    crate::desktop_navigation::Command::Select {
                        generation: handle.generation.clone(),
                        route: chat,
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn local_tab_selection_transfers_keyboard_ownership_from_chat_to_notes() {
        let (root, handle, _) = fixture();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1280, 720),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let route = |label: &str| {
            serde_json::from_value::<crate::desktop_navigation::Route>(
                handle.navigation.borrow()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["label"] == label)
                    .unwrap()["route"]
                    .clone(),
            )
            .unwrap()
        };
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: route("Chat"),
                },
            )
            .unwrap();
        let (composer, _) = browser_semantic_route(&frame, "Message composer");
        frame
            .perform_semantic_named(&composer, None, &ui::a11y::Action::Focus)
            .unwrap();
        let chat_owner = frame
            .inspection()
            .keyboard_focus_owner
            .expect("Chat composer owns input");
        let chat = handle
            .records
            .borrow()
            .values()
            .find(|record| record.app == "dev.qos.chat")
            .unwrap()
            .clone();
        let chat_before = chat
            .store
            .read(&chat.checkout.id, |doc| doc.clone().save())
            .unwrap();
        let notes = route("Notes").ids().unwrap().3;
        // SAFETY: frame retains this root and the temporary access ends before
        // the harness runs the ordinary update/reconcile/layout pipeline.
        unsafe {
            (&mut *root_ptr).surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab(notes);
        }
        frame.relayout();
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            notes
        );
        handle.finish_local_activation(&mut frame);
        let notes_owner = frame
            .inspection()
            .keyboard_focus_owner
            .expect("selected Notes owns input");
        assert_ne!(
            notes_owner, chat_owner,
            "the retained Chat composer must lose keyboard ownership"
        );
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "must not reach Chat".into(),
        )));
        assert_eq!(
            chat.store
                .read(&chat.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            chat_before
        );

        let chat_tab = route("Chat").ids().unwrap().3;
        unsafe {
            (&mut *root_ptr).surface.row_mut(StripId(2)).unwrap().groups[0]
                .set_active_tab(chat_tab);
        }
        frame.relayout();
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: route("Calculator"),
                },
            )
            .unwrap();
        let calculator_owner = frame
            .inspection()
            .keyboard_focus_owner
            .expect("Calculator owns input");
        handle.finish_local_activation(&mut frame);
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(calculator_owner.as_str()),
            "a stale local Chat request must not override a newer external Calculator selection"
        );
    }

    #[test]
    fn ordinary_app_modal_rejects_background_group_semantic_focus() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .enumerate()
                .map(|(index, tab)| WindowGroup {
                    id: 100 + index as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect();
        });
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1800, 900),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let route = |label: &str| {
            serde_json::from_value::<crate::desktop_navigation::Route>(
                handle.navigation.borrow()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["label"] == label)
                    .unwrap()["route"]
                    .clone(),
            )
            .unwrap()
        };
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: route("Writer"),
                },
            )
            .unwrap();
        fn widget_rect(node: &ui::headless::InspectionNode, widget: &str) -> Option<Rect> {
            if node.widget == widget {
                return Some(Rect::new(
                    node.rect[0],
                    node.rect[1],
                    node.rect[2],
                    node.rect[3],
                ));
            }
            node.children
                .iter()
                .find_map(|child| widget_rect(child, widget))
        }
        let point = widget_rect(&frame.inspection().root, "WordProcessor")
            .unwrap()
            .center();
        let device_id = winit::event::DeviceId::dummy();
        frame.send_host_event(&winit::event::WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        });
        for state in [
            winit::event::ElementState::Pressed,
            winit::event::ElementState::Released,
        ] {
            frame.send_host_event(&winit::event::WindowEvent::MouseInput {
                device_id,
                state,
                button: winit::event::MouseButton::Right,
            });
        }
        assert!(handle.check_local_activation(&frame).is_err());
        let before_group = handle.tree.borrow().active_group[&StripId(2)];
        let before_owner = frame.inspection().keyboard_focus_owner;
        let (composer, _) = browser_semantic_route(&frame, "Message composer");
        assert!(
            handle
                .perform_semantic_input(&mut frame, &composer, None, &ui::a11y::Action::Focus)
                .is_err()
        );
        assert_eq!(
            handle.tree.borrow().active_group[&StripId(2)],
            before_group,
            "modal preflight must reject background-group focus before selection changes"
        );
        assert_eq!(frame.inspection().keyboard_focus_owner, before_owner);
    }

    #[test]
    fn local_group_selection_transfers_keyboard_ownership_to_its_active_notes_tab() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .enumerate()
                .map(|(index, tab)| WindowGroup {
                    id: 100 + index as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect();
        });
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1800, 900),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let route = |label: &str| {
            serde_json::from_value::<crate::desktop_navigation::Route>(
                handle.navigation.borrow()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["label"] == label)
                    .unwrap()["route"]
                    .clone(),
            )
            .unwrap()
        };
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: route("Chat"),
                },
            )
            .unwrap();
        let (composer, _) = browser_semantic_route(&frame, "Message composer");
        frame
            .perform_semantic_named(&composer, None, &ui::a11y::Action::Focus)
            .unwrap();
        let chat_owner = frame
            .inspection()
            .keyboard_focus_owner
            .expect("Chat composer owns input");
        let notes = route("Notes").ids().unwrap();
        {
            let mut tree = handle.tree.borrow_mut();
            DesktopRoot::apply_local(
                &handle,
                &mut tree,
                LocalViewIntent::Group {
                    strip: notes.1,
                    group: notes.2,
                },
            );
        }
        frame.relayout();
        handle.finish_local_activation(&mut frame);
        assert_ne!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(chat_owner.as_str())
        );
        assert_eq!(handle.tree.borrow().active_group[&notes.1], notes.2);
    }

    #[derive(Clone, Copy, Debug)]
    enum LocalSelectionCase {
        Workspace,
        Strip,
        Group,
        Tab,
        ChromeWorkspace,
    }

    fn arrange_local_selection_case(tree: &mut WorkspaceTree, case: LocalSelectionCase) {
        let workspace = &mut tree.workspaces[0];
        let strip = &mut workspace.strips[0];
        let mut tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        let first = tabs.remove(0);
        let second = tabs.remove(2);
        strip.window_groups[0].tabs = vec![first];
        strip.window_groups[0].active_tab = strip.window_groups[0].tabs[0].id;
        tree.active_strip.insert(workspace.id, strip.id);
        tree.active_group
            .insert(strip.id, strip.window_groups[0].id);

        match case {
            LocalSelectionCase::Tab => {
                strip.window_groups[0].tabs.push(second);
            }
            LocalSelectionCase::Group => {
                strip.window_groups.push(WindowGroup {
                    id: 4,
                    active_tab: second.id,
                    tabs: vec![second],
                    width: WindowWidth::Normal,
                    maximized: false,
                });
            }
            LocalSelectionCase::Strip => {
                workspace.strips.push(Strip {
                    id: StripId(9),
                    name: "Second".into(),
                    window_groups: vec![WindowGroup {
                        id: 90,
                        active_tab: second.id,
                        tabs: vec![second],
                        width: WindowWidth::Normal,
                        maximized: false,
                    }],
                });
                tree.active_group.insert(StripId(9), 90);
            }
            LocalSelectionCase::Workspace | LocalSelectionCase::ChromeWorkspace => {
                let mut second_workspace = workspace.clone();
                second_workspace.id = WorkspaceId(9);
                second_workspace.name = "Second".into();
                second_workspace.strips = vec![Strip {
                    id: StripId(9),
                    name: "Second".into(),
                    window_groups: vec![WindowGroup {
                        id: 90,
                        active_tab: second.id,
                        tabs: vec![second],
                        width: WindowWidth::Normal,
                        maximized: false,
                    }],
                }];
                tree.active_strip.insert(second_workspace.id, StripId(9));
                tree.active_group.insert(StripId(9), 90);
                tree.workspaces.push(second_workspace);
            }
        }
    }

    fn focused_controls_for_instance(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        instance: ui::application::InstanceId,
    ) -> usize {
        fn focused(node: &ui::a11y::AccessNode) -> usize {
            usize::from(node.state.focused) + node.children.iter().map(focused).sum::<usize>()
        }
        let here = if frame.semantic_owner_instance(&node.path) == Some(instance) {
            node.semantics.as_ref().map_or(0, focused)
        } else {
            0
        };
        here + node
            .children
            .iter()
            .map(|child| focused_controls_for_instance(frame, child, instance))
            .sum::<usize>()
    }

    fn focusable_control_for_instance(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        instance: ui::application::InstanceId,
    ) -> Option<String> {
        if node.focusable && frame.semantic_owner_instance(&node.path) == Some(instance) {
            return Some(node.path.clone());
        }
        node.children
            .iter()
            .find_map(|child| focusable_control_for_instance(frame, child, instance))
    }

    #[test]
    fn visible_local_selections_bind_input_to_the_selected_active_instance() {
        for case in [
            LocalSelectionCase::Strip,
            LocalSelectionCase::Group,
            LocalSelectionCase::Tab,
        ] {
            let (root, handle, _) = fixture_with_topology(|tree| {
                arrange_local_selection_case(tree, case);
            });
            let mut root = Box::new(root);
            let root_ptr = std::ptr::from_mut(root.as_mut());
            let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
                ui::State::new(),
                root,
                (1800, 900),
                Theme::dark(),
                ui::TextRenderer::new(),
                ui::permissions::UserId("author".into()),
            );
            let initial_instance = active_route(&handle.tree.borrow())
                .unwrap()
                .ids()
                .unwrap()
                .3;
            let initial_control =
                focusable_control_for_instance(&frame, &frame.inspection().root, initial_instance)
                    .expect("initial app exposes a focusable control");
            frame
                .perform_semantic_named(&initial_control, None, &ui::a11y::Action::Focus)
                .unwrap();
            frame.relayout();
            handle.finish_local_activation(&mut frame);
            assert!(
                focused_controls_for_instance(&frame, &frame.inspection().root, initial_instance)
                    > 0,
                "{case:?}: precondition requires a focused control in the initial app"
            );

            match case {
                LocalSelectionCase::Workspace => {
                    let mut tree = handle.tree.borrow_mut();
                    DesktopRoot::apply_local(
                        &handle,
                        &mut tree,
                        LocalViewIntent::Workspace(WorkspaceId(9)),
                    );
                    unsafe {
                        (&mut *root_ptr).view_changed = true;
                    }
                }
                LocalSelectionCase::Strip => {
                    let mut tree = handle.tree.borrow_mut();
                    DesktopRoot::apply_local(
                        &handle,
                        &mut tree,
                        LocalViewIntent::Strip(StripId(9)),
                    );
                    unsafe {
                        (&mut *root_ptr).view_changed = true;
                    }
                }
                LocalSelectionCase::Group => {
                    let mut tree = handle.tree.borrow_mut();
                    DesktopRoot::apply_local(
                        &handle,
                        &mut tree,
                        LocalViewIntent::Group {
                            strip: StripId(2),
                            group: 4,
                        },
                    );
                    unsafe {
                        (&mut *root_ptr).view_changed = true;
                    }
                }
                LocalSelectionCase::Tab => {
                    let target =
                        handle.tree.borrow().workspaces[0].strips[0].window_groups[0].tabs[1].id;
                    let mut tree = handle.tree.borrow_mut();
                    DesktopRoot::apply_local(
                        &handle,
                        &mut tree,
                        LocalViewIntent::Tab {
                            group: 3,
                            tab: target,
                        },
                    );
                    unsafe {
                        (&mut *root_ptr).view_changed = true;
                    }
                }
                LocalSelectionCase::ChromeWorkspace => {
                    let mut tree = handle.tree.borrow_mut();
                    DesktopRoot::switch_workspace(&handle, &mut tree, 1);
                    unsafe {
                        (&mut *root_ptr).view_changed = true;
                    }
                }
            }
            frame.relayout();
            handle.finish_local_activation(&mut frame);

            let selected_instance = active_route(&handle.tree.borrow())
                .unwrap()
                .ids()
                .unwrap()
                .3;
            assert_ne!(
                selected_instance, initial_instance,
                "{case:?}: route changed app"
            );
            let owner = frame
                .inspection()
                .keyboard_focus_owner
                .unwrap_or_else(|| panic!("{case:?}: selected app owns keyboard input"));
            assert_eq!(
                frame.semantic_owner_instance(&owner),
                Some(selected_instance),
                "{case:?}: keyboard owner must belong to the active route instance"
            );
            assert_eq!(
                focused_controls_for_instance(&frame, &frame.inspection().root, initial_instance),
                0,
                "{case:?}: the previous app must not retain a focused control"
            );
        }
    }

    #[test]
    fn workspace_selectors_queue_the_selected_instance_as_next_keyboard_owner() {
        for case in [
            LocalSelectionCase::Workspace,
            LocalSelectionCase::ChromeWorkspace,
        ] {
            let (_root, handle, _) = fixture_with_topology(|tree| {
                arrange_local_selection_case(tree, case);
            });
            let previous = active_route(&handle.tree.borrow()).unwrap();
            match case {
                LocalSelectionCase::Workspace => DesktopRoot::apply_local(
                    &handle,
                    &mut handle.tree.borrow_mut(),
                    LocalViewIntent::Workspace(WorkspaceId(9)),
                ),
                LocalSelectionCase::ChromeWorkspace => {
                    DesktopRoot::switch_workspace(&handle, &mut handle.tree.borrow_mut(), 1);
                }
                _ => unreachable!(),
            }
            let selected = active_route(&handle.tree.borrow()).unwrap();
            assert_ne!(
                selected.ids().unwrap().3,
                previous.ids().unwrap().3,
                "{case:?}"
            );
            assert_eq!(
                handle.pending_local_activation.borrow().as_ref(),
                Some(&selected),
                "{case:?}: queue the exact route whose instance became active"
            );
            assert_eq!(
                handle
                    .pending_local_activation
                    .borrow()
                    .as_ref()
                    .and_then(|route| route.ids().ok())
                    .map(|ids| ids.3),
                Some(selected.ids().unwrap().3),
                "{case:?}: the queued keyboard owner must be the selected active instance"
            );
        }
    }

    #[test]
    fn non_selection_local_changes_do_not_queue_activation() {
        let (_root, handle, _) = fixture();
        for intent in [
            LocalViewIntent::Width {
                group: 3,
                width: WindowWidth::Fraction(500),
            },
            LocalViewIntent::Maximized {
                group: 3,
                maximized: true,
            },
        ] {
            DesktopRoot::apply_local(&handle, &mut handle.tree.borrow_mut(), intent);
            assert!(handle.pending_local_activation.borrow().is_none());
        }
    }

    #[test]
    fn admission_only_refresh_preserves_the_current_keyboard_owner() {
        let (root, handle, _) = fixture();
        let routes = root.resolver.host.routes.clone();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1280, 720),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let route = serde_json::from_value::<crate::desktop_navigation::Route>(
            handle.navigation.borrow()["entries"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["route"]
                .clone(),
        )
        .unwrap();
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route,
                },
            )
            .unwrap();
        let owner = frame.inspection().keyboard_focus_owner.unwrap();
        let activation = handle.activation_sequence.get();

        let tree = handle.tree.borrow().clone();
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&tree),
                source_heads: vec![[8; 32]],
                tree,
                routes,
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        frame.relayout();
        handle.finish_local_activation(&mut frame);

        assert!(handle.pending_local_activation.borrow().is_none());
        assert_eq!(handle.activation_sequence.get(), activation);
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(owner.as_str())
        );
    }

    #[test]
    fn external_strip_and_workspace_selection_activate_the_resulting_route() {
        for case in [LocalSelectionCase::Strip, LocalSelectionCase::Workspace] {
            let (root, handle, _) =
                fixture_with_topology(|tree| arrange_local_selection_case(tree, case));
            let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
                ui::State::new(),
                Box::new(root),
                (1800, 900),
                Theme::dark(),
                ui::TextRenderer::new(),
                ui::permissions::UserId("author".into()),
            );
            let initial = active_route(&handle.tree.borrow())
                .unwrap()
                .ids()
                .unwrap()
                .3;
            let control = focusable_control_for_instance(&frame, &frame.inspection().root, initial)
                .expect("initial app exposes a focusable control");
            frame
                .perform_semantic_named(&control, None, &ui::a11y::Action::Focus)
                .unwrap();
            frame.relayout();
            handle.finish_local_activation(&mut frame);
            let activation = handle.activation_sequence.get();
            let command = match case {
                LocalSelectionCase::Strip => crate::desktop_navigation::Command::Strip {
                    generation: handle.generation.clone(),
                    workspace: "1".into(),
                    strip: "9".into(),
                },
                LocalSelectionCase::Workspace => crate::desktop_navigation::Command::Workspace {
                    generation: handle.generation.clone(),
                    workspace: "9".into(),
                },
                _ => unreachable!(),
            };
            handle.navigate(&mut frame, command).unwrap();
            let selected = active_route(&handle.tree.borrow())
                .unwrap()
                .ids()
                .unwrap()
                .3;
            assert_ne!(selected, initial, "{case:?}: route changes instance");
            assert_eq!(handle.activation_sequence.get(), activation + 1, "{case:?}");
            if matches!(case, LocalSelectionCase::Strip) {
                let owner = frame.inspection().keyboard_focus_owner.unwrap();
                assert_eq!(
                    frame.semantic_owner_instance(&owner),
                    Some(selected),
                    "{case:?}"
                );
            }
            assert_eq!(
                focused_controls_for_instance(&frame, &frame.inspection().root, initial),
                0,
                "{case:?}"
            );
        }
    }

    #[test]
    fn pointer_press_on_notes_nonfocusable_surface_clears_chat_composer_focus() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            let mut chat = None;
            let mut notes = None;
            for tab in tabs {
                match tab.title.as_str() {
                    "Chat" => chat = Some(tab),
                    "Notes" => notes = Some(tab),
                    _ => {}
                }
            }
            let chat = chat.unwrap();
            let notes = notes.unwrap();
            strip.window_groups = vec![
                WindowGroup {
                    id: 4,
                    active_tab: notes.id,
                    tabs: vec![notes],
                    width: WindowWidth::Normal,
                    maximized: false,
                },
                WindowGroup {
                    id: 3,
                    active_tab: chat.id,
                    tabs: vec![chat],
                    width: WindowWidth::Normal,
                    maximized: false,
                },
            ];
            tree.active_group.insert(strip.id, 3);
        });
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1800, 900),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let route = |label: &str| {
            serde_json::from_value::<crate::desktop_navigation::Route>(
                handle.navigation.borrow()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["label"] == label)
                    .unwrap()["route"]
                    .clone(),
            )
            .unwrap()
        };
        let notes_instance = route("Notes").ids().unwrap().3;
        let (composer, composer_target) = browser_semantic_route(&frame, "Message composer");
        frame
            .perform_semantic_named(&composer, Some(&composer_target), &ui::a11y::Action::Focus)
            .unwrap();
        frame.relayout();
        handle.finish_local_activation(&mut frame);
        let activation = handle.activation_sequence.get();
        assert_eq!(
            active_route(&handle.tree.borrow())
                .unwrap()
                .ids()
                .unwrap()
                .3,
            route("Chat").ids().unwrap().3,
            "Chat must be the canonical active app before the click"
        );
        let composer_focused = |frame: &ui::headless::FrameHarness| {
            fn find(node: &ui::a11y::AccessNode) -> Option<bool> {
                if node.name.as_deref() == Some("Message composer") {
                    return Some(node.state.focused);
                }
                node.children.iter().find_map(find)
            }
            fn walk(node: &ui::headless::InspectionNode) -> Option<bool> {
                node.semantics
                    .as_ref()
                    .and_then(find)
                    .or_else(|| node.children.iter().find_map(walk))
            }
            walk(&frame.inspection().root).unwrap()
        };
        assert!(composer_focused(&frame), "Chat composer precondition");
        let chat = handle
            .records
            .borrow()
            .values()
            .find(|record| record.app == "dev.qos.chat")
            .unwrap()
            .clone();
        let chat_before = chat
            .store
            .read(&chat.checkout.id, |doc| doc.clone().save())
            .unwrap();
        let (group_rect, viewport, visible) = unsafe {
            let row = (&*root_ptr).surface.row(StripId(2)).unwrap();
            let group = row.groups.iter().find(|group| group.id == 4).unwrap();
            (
                group.get_rect(),
                row.get_rect(),
                row.children()
                    .iter()
                    .any(|widget| std::ptr::addr_eq(*widget, group as &dyn Widget)),
            )
        };
        let mut focusable = Vec::new();
        fn collect_focusable(
            frame: &ui::headless::FrameHarness,
            node: &ui::headless::InspectionNode,
            instance: ui::application::InstanceId,
            out: &mut Vec<Rect>,
        ) {
            if node.focusable && frame.semantic_owner_instance(&node.path) == Some(instance) {
                out.push(Rect::new(
                    node.rect[0],
                    node.rect[1],
                    node.rect[2],
                    node.rect[3],
                ));
            }
            for child in &node.children {
                collect_focusable(frame, child, instance, out);
            }
        }
        collect_focusable(
            &frame,
            &frame.inspection().root,
            notes_instance,
            &mut focusable,
        );
        let hit = Rect::new(
            group_rect.x0.max(viewport.x0),
            group_rect.y0.max(viewport.y0),
            group_rect.x1.min(viewport.x1),
            group_rect.y1.min(viewport.y1),
        );
        let point = (0..=((hit.height() / 8.0) as usize))
            .find_map(|y| {
                (0..=((hit.width() / 8.0) as usize)).find_map(|x| {
                    let point =
                        Point::new(hit.x0 + 2.0 + x as f64 * 8.0, hit.y0 + 2.0 + y as f64 * 8.0);
                    (hit.contains(point) && !focusable.iter().any(|bounds| bounds.contains(point)))
                        .then_some(point)
                })
            })
            .expect("visible Notes group exposes nonfocusable chrome or surface");
        assert!(visible && group_rect.contains(point) && viewport.contains(point));
        assert!(
            !focusable.iter().any(|bounds| bounds.contains(point)),
            "pointer must avoid every focusable Notes control: point={point:?} focusable={focusable:?}"
        );
        let device_id = winit::event::DeviceId::dummy();
        for event in [
            winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(point.x, point.y),
            },
            winit::event::WindowEvent::MouseInput {
                device_id,
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            winit::event::WindowEvent::MouseInput {
                device_id,
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
        ] {
            frame.send_host_event(&event);
            handle.finish_local_activation(&mut frame);
        }
        assert_eq!(
            active_route(&handle.tree.borrow())
                .unwrap()
                .ids()
                .unwrap()
                .3,
            notes_instance
        );
        assert_eq!(
            handle.activation_sequence.get(),
            activation + 1,
            "group pointer selection must cross the app activation boundary"
        );
        frame.relayout();
        let owner = frame.inspection().keyboard_focus_owner.unwrap();
        assert_eq!(frame.semantic_owner_instance(&owner), Some(notes_instance));
        assert!(
            !composer_focused(&frame),
            "Chat composer must clear its semantic focused flag"
        );
        for action in [
            ui::a11y::Action::SetValue("stale browser textarea value".into()),
            ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 0,
            },
        ] {
            assert!(
                handle
                    .perform_semantic_input(&mut frame, &composer, Some(&composer_target), &action,)
                    .is_err(),
                "a stale semantic text route must not operate after another control owns focus: {action:?}",
            );
            assert_eq!(
                chat.store
                    .read(&chat.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
                chat_before,
                "rejected stale semantic text input must not mutate Chat: {action:?}",
            );
        }
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "must not reach previous app".into(),
        )));
        assert_eq!(
            chat.store
                .read(&chat.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            chat_before
        );
    }

    #[test]
    fn global_search_escape_preserves_selected_calculator_route_and_pan() {
        let (root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .filter(|tab| tab.title == "Writer" || tab.title == "Calculator")
                .enumerate()
                .map(|(index, tab)| WindowGroup {
                    id: 100 + index as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect();
        });
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (740, 720),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.click_named("word_processor").unwrap();
        settle_external_navigation(root_ptr, &mut frame);
        let before_heads = handle
            .records
            .borrow()
            .values()
            .map(|record| {
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.clone().get_heads())
                    .unwrap()
            })
            .collect::<Vec<_>>();

        handle
            .open_global_search(&mut frame, &handle.generation, "Calculator")
            .unwrap();
        let calculator = handle
            .search
            .borrow()
            .data
            .tabs
            .iter()
            .find(|tab| tab.app_id == "dev.qos.calculator")
            .unwrap()
            .instance_id;
        let (owner, filter) = browser_semantic_route(&frame, "Filter: Records");
        frame
            .perform_semantic_named(&owner, Some(&filter), &ui::a11y::Action::Activate)
            .unwrap();
        let semantics = frame.inspection().root.semantics.unwrap();
        let rows = &semantics.children[1].children;
        assert_eq!(
            rows.len(),
            1,
            "Records filter must select the admitted Calculator tab, not launch an app"
        );
        let bounds = rows[0].bounds.unwrap().to_rect();
        frame.click_at(bounds.center().x, bounds.center().y);
        handle.process_global_search(&mut frame).unwrap();
        settle_external_navigation(root_ptr, &mut frame);
        let selected = handle.navigation.borrow().clone();
        assert_eq!(selected["active"]["tab"], calculator.to_string());
        let pan = selected["desktopPan"]["offset"].as_f64().unwrap();
        assert!(pan > 0.0, "narrow desktop must have panned to Calculator");
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(browser_semantic_route(&frame, "Calculator").0.as_str()),
            "accepted selection must focus Calculator"
        );

        handle
            .open_global_search(&mut frame, &handle.generation, "")
            .unwrap();
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "Writer".into(),
        )));
        assert_eq!(frame.global_search_query(), Some("Writer"));
        frame.key("Escape");
        handle.process_global_search(&mut frame).unwrap();
        settle_external_navigation(root_ptr, &mut frame);
        let after = handle.navigation.borrow();
        assert_eq!(
            after["active"], selected["active"],
            "dismissing Search must not reinterpret restored Writer focus as a new navigation gesture"
        );
        assert_eq!(
            after["desktopPan"]["offset"],
            selected["desktopPan"]["offset"]
        );
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(browser_semantic_route(&frame, "Calculator").0.as_str())
        );
        assert_eq!(
            handle
                .records
                .borrow()
                .values()
                .map(|record| {
                    record
                        .store
                        .read(&record.checkout.id, |doc| doc.clone().get_heads())
                        .unwrap()
                })
                .collect::<Vec<_>>(),
            before_heads
        );
        assert!(handle.session_requests.borrow().is_empty());
        assert!(frame.take_caller_operation().is_none());
        assert!(frame.take_global_search_intent().is_none());
    }

    /// Manual CPU microbenchmark for the semantics status gate on the real
    /// desktop fixture while typing into the Writer: tree serialization (paid
    /// on every frame for the change hash), the structure signature, the gate
    /// decision, and what the Stage 4 path paid on top when it parsed the bytes
    /// back into a Value and serialized them again inside the status.
    #[test]
    #[ignore = "manual CPU microbenchmark, not a browser frame-rate gate"]
    fn semantics_serialization_cost_per_typed_key() {
        use crate::semantics_status::{SemanticsDecision, SemanticsGate, structure_signature};
        fn count(node: &ui::headless::InspectionNode) -> usize {
            1 + node.children.iter().map(count).sum::<usize>()
                + node.semantics.as_ref().map_or(0, count_access)
        }
        fn count_access(node: &ui::a11y::AccessNode) -> usize {
            1 + node.children.iter().map(count_access).sum::<usize>()
        }
        let (root, _handle, _) = fixture();
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1000, 700),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        frame.click_at(200.0, 180.0);
        frame.relayout();
        assert!(
            frame.inspection().keyboard_focus_owner.is_some(),
            "the Writer editor must own keyboard focus"
        );
        let mut gate = SemanticsGate::new(std::time::Duration::from_millis(100));
        let (mut serialize, mut structure, mut decide, mut stage4_extra) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut nodes, mut bytes, mut delivered, mut deferred, mut unchanged) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        let ms = |start: std::time::Instant| start.elapsed().as_secs_f64() * 1000.0;
        for i in 0..200 {
            frame.type_text(if i % 7 == 6 { " " } else { "a" });
            frame.relayout();
            let output = frame.frame_output();
            let snapshot = &output.semantics;
            nodes = count(&snapshot.root);
            let start = std::time::Instant::now();
            let text = serde_json::to_vec(snapshot).unwrap();
            serialize.push(ms(start));
            bytes = text.len();
            let start = std::time::Instant::now();
            let _ = structure_signature(snapshot);
            structure.push(ms(start));
            let start = std::time::Instant::now();
            let decision = gate.decide(snapshot, output.ime.as_ref(), ui::clock::Instant::now());
            decide.push(ms(start));
            match decision {
                SemanticsDecision::Deliver { .. } => delivered += 1,
                SemanticsDecision::Deferred(_) => deferred += 1,
                SemanticsDecision::Unchanged => unchanged += 1,
            }
            let start = std::time::Instant::now();
            let value: serde_json::Value = serde_json::from_slice(&text).unwrap();
            let _ = value.to_string();
            stage4_extra.push(ms(start));
        }
        let percentiles = |samples: &mut Vec<f64>| {
            samples.sort_by(f64::total_cmp);
            (
                samples[samples.len() / 2],
                samples[samples.len() * 99 / 100],
            )
        };
        let (serialize_p50, serialize_p99) = percentiles(&mut serialize);
        let (structure_p50, structure_p99) = percentiles(&mut structure);
        let (decide_p50, decide_p99) = percentiles(&mut decide);
        let (extra_p50, extra_p99) = percentiles(&mut stage4_extra);
        eprintln!(
            "semantics per typed key (200 keys, desktop fixture, {nodes} nodes, {bytes} bytes JSON, native {}): serialize p50={serialize_p50:.3} p99={serialize_p99:.3} ms; structure signature p50={structure_p50:.3} p99={structure_p99:.3} ms; gate decide p50={decide_p50:.3} p99={decide_p99:.3} ms; stage-4 parse+reserialize extra p50={extra_p50:.3} p99={extra_p99:.3} ms; decisions: delivered={delivered} deferred={deferred} unchanged={unchanged}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
    }
}

#[cfg(test)]
mod layout_benchmark {
    use super::tests::settled_writer_frame;

    fn mean_us(
        samples: &[ui::profiling::FrameTiming],
        pick: impl Fn(&ui::profiling::FrameTiming) -> u64,
    ) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        samples.iter().map(|s| pick(s) as f64).sum::<f64>() / samples.len() as f64
    }

    /// Types 50 characters into the desktop fixture's Writer holding a 4 KiB
    /// document and prints the per-pass mean per typed key, first with the
    /// full-rebuild path forced (`QUICK_UI_FULL_RELAYOUT`) and then with the
    /// retained path. Run with `--release -- --ignored --nocapture`.
    #[test]
    #[ignore = "manual CPU microbenchmark of the headless frame passes, not a gate"]
    fn writer_typing_layout_passes() {
        let (mut frame, handle) = settled_writer_frame();
        // A 4 KiB body, as the browser measurement fixture carries.
        let writer = handle
            .records
            .borrow()
            .values()
            .find(|r| r.app == "dev.qos.word-processor")
            .unwrap()
            .clone();
        let body: String = std::iter::repeat("The quick brown fox jumps over the lazy dog. ")
            .take(4096 / 45 + 1)
            .collect();
        writer
            .store
            .with(&writer.checkout.id, |doc| {
                let mut editor = word_processor::WordProcessor::new();
                editor.bind(doc, &automerge::ROOT);
                editor.insert_str(doc, &body);
            })
            .unwrap();
        frame.relayout();
        frame.relayout();
        let mut report = String::new();
        for (label, forced) in [
            ("full rebuild (QUICK_UI_FULL_RELAYOUT)", Some(true)),
            ("retained", Some(false)),
        ] {
            frame.set_full_relayout(forced);
            frame.begin_profiling();
            frame.type_text("warm up");
            frame.clear_profile_samples();
            frame.type_text("Retained layout keeps the composed desktop between keys.");
            let samples = frame.take_profile_samples();
            frame.end_profiling();
            let stats = frame.last_relayout();
            report.push_str(&format!(
                "{label}: {} frames; mean us per typed key: sync {:.0} build+compute {:.0} apply {:.0} draw {:.0} input {:.0} total {:.0}; last relayout {:?}\n",
                samples.len(),
                mean_us(&samples, |s| s.sync_us),
                mean_us(&samples, |s| s.build_layout_us),
                mean_us(&samples, |s| s.apply_layout_us),
                mean_us(&samples, |s| s.draw_us),
                mean_us(&samples, |s| s.handle_event_us),
                mean_us(&samples, |s| s.total_us),
                stats,
            ));
        }
        frame.set_full_relayout(None);
        eprintln!(
            "desktop Writer typing, 4 KiB body, {} build:\n{report}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
    }
}
