//! Portable retained host around the actual canonical DesktopSurface.
use crate::desktop::{self, Admission};
use automerge::{AutoCommit, ObjId};
use qos_desktop::{
    composition::{DesktopSurface, ReconcileOutcome, SurfaceChrome},
    intents::{DesktopIntent, LocalViewIntent, SessionIntent},
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
#[derive(Clone)]
pub(crate) struct DesktopHandle {
    pub tree: Rc<RefCell<WorkspaceTree>>,
    pub permissions: Rc<RefCell<ui::permissions::PermissionContext>>,
    pub records: Rc<RefCell<BTreeMap<(String, String), OwnedRecord>>>,
    pub images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    pub errors: Rc<RefCell<Vec<String>>>,
    pub session_requests: Rc<RefCell<Vec<SessionIntent>>>,
    inbox: Rc<RefCell<Option<(Admission, ui::permissions::PermissionContext)>>>,
}
impl DesktopHandle {
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
        let store = self
            .owners
            .open(definition.app_id, &self.room)
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
            qos_actions::ActionContext::default(),
            target,
        )?;
        self.handle.records.borrow_mut().insert(
            (definition.app_id.into(), checkout.id.as_str().into()),
            OwnedRecord {
                app: definition.app_id.into(),
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
}
impl DesktopRoot {
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
        for definition in [
            word_processor::definition(),
            chat::definition(),
            notes::definition(),
            calculator::definition(),
        ] {
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
        };
        root.collect_failures();
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
    fn reconcile(&mut self) -> bool {
        let intents = self.surface.take_intents();
        let chrome = std::mem::take(&mut *self.chrome_intents.borrow_mut());
        let inbox = self.handle.inbox.borrow_mut().take();
        let mut tree = self.handle.tree.borrow_mut();
        let mut changed = !intents.is_empty() || !chrome.is_empty() || inbox.is_some();
        // Explicit workspace switches are newer than gestures harvested from the
        // outgoing surface. Save those tab choices without stealing active focus.
        for intent in chrome {
            match intent {
                qos_desktop::chrome::ChromeIntent::SwitchWorkspace(delta) => {
                    if let Some(index) = tree.workspaces.iter().position(|w| w.id == tree.active) {
                        let next = (index as i64 + i64::from(delta))
                            .rem_euclid(tree.workspaces.len() as i64)
                            as usize;
                        tree.active = tree.workspaces[next].id;
                    }
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
                DesktopIntent::Local(intent) => Self::local(&mut tree, intent),
                DesktopIntent::Session(intent) => {
                    self.handle.session_requests.borrow_mut().push(intent)
                }
            }
        }
        if let Some((admission, permissions)) = inbox {
            let local = LocalView::from_tree(&tree);
            *tree = admission.tree;
            local.apply(&mut tree);
            self.resolver.host.routes = admission.routes;
            *self.resolver.host.permissions.borrow_mut() = permissions;
        }
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
        let changed = self.reconcile();
        let updated = self.surface.update(doc, path);
        changed || updated
    }
    fn needs_redraw(&self) -> bool {
        self.dirty || self.handle.inbox.borrow().is_some() || self.surface.needs_redraw()
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.surface.build_layout(tree, theme, map)
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.surface.apply_layout(tree, node, origin);
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
        self.surface.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, tree,
        );
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.surface]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.surface]
    }
    fn name(&self) -> &str {
        "CertifiedDesktop"
    }
    fn get_rect(&self) -> Rect {
        self.surface.get_rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty) | self.surface.take_structure_dirty()
    }
    fn wants_pointer_capture(&self) -> bool {
        self.surface.wants_pointer_capture()
    }
    fn close_components(&mut self) -> Vec<ComponentCloseReport> {
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
    fn fixture() -> (DesktopRoot, DesktopHandle, PortableStorage) {
        let cache = PortableStorage::new("desktop-host-test", "device").unwrap();
        let mut owners = RecordOwners::portable(cache.clone());
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId(1), "Work");
        let room = workspace.room.to_string();
        let mut tabs = vec![];
        let mut routes = BTreeMap::new();
        // Keep the real initialized stores alive until their checked components take custody.
        let mut retained = vec![];
        for (i, definition) in [
            word_processor::definition(),
            chat::definition(),
            notes::definition(),
            calculator::definition(),
        ]
        .into_iter()
        .enumerate()
        {
            let id = (5 << 48) | ((i as u64) + 10);
            let record = if i == 0 {
                RecordCheckout::new(
                    ui::record::RecordId::for_tab(1),
                    definition.record_kind.clone(),
                )
            } else {
                qos_desktop::records::component_record(
                    definition.app_id,
                    id,
                    definition.record_kind.clone(),
                )
            };
            let store = owners.open(definition.app_id, &room).unwrap();
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
                    kind: AppKind::Other(definition.app_id.into()),
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
        let permissions = ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
            vec![],
        );
        let admission = Admission {
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
    fn browser_semantic_route(frame: &ui::headless::FrameHarness, label: &str) -> (String, String) {
        fn find(node: &ui::headless::InspectionNode, label: &str) -> Option<(String, String)> {
            fn target(node: &ui::a11y::AccessNode, label: &str) -> Option<String> {
                if node.name.as_deref() == Some(label) {
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
        frame.perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus).unwrap();
        assert_eq!(frame.inspection().keyboard_focus_owner.as_deref(), Some(owner.as_str()));
        assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 4,
            "authoritative cold Chat focus must outrank retained Writer focus flags");
        unsafe {
            (&mut *root_ptr).surface.row_mut(StripId(2)).unwrap()
                .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
        }
        frame.relayout();
        let row = unsafe { (&*root_ptr).surface.row(StripId(2)).unwrap() };
        assert!(row.groups[1].focused());
        assert!(row.groups[1].get_rect().x1 <= row.get_rect().x1 + 0.5);
    }

    #[test]
    fn desktop_cold_chat_pointer_and_explicit_focus_own_first_key_and_ime() {
        fn access(node: &ui::headless::InspectionNode, label: &str) -> Option<ui::a11y::AccessNode> {
            fn semantic(node: &ui::a11y::AccessNode, label: &str) -> Option<ui::a11y::AccessNode> {
                if node.name.as_deref() == Some(label) {
                    return Some(node.clone());
                }
                node.children
                    .iter()
                    .find_map(|child| semantic(child, label))
            }
            if node.test_id.as_deref() == Some(label) { return node.semantics.clone(); }
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
            assert!(access(&frame.inspection().root, "word_processor").unwrap().state.focused,
                "cold Writer retains a local focus flag");
            assert!(access(&frame.inspection().root, "Message composer").unwrap().state.focused,
                "cold Chat already has a local composer caret");
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
    fn browser_semantic_tab_activation_updates_canonical_selection() {
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
        let (owner, target) = browser_semantic_route(&frame, "Notes");
        frame
            .perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus)
            .unwrap();
        fn focused(node: &ui::headless::InspectionNode, path: &str) -> Option<bool> {
            if node.path == path {
                return node.semantics.as_ref().map(|node| node.state.focused);
            }
            node.children.iter().find_map(|node| focused(node, path))
        }
        assert_eq!(
            focused(&frame.inspection().root, &owner),
            Some(true),
            "requested tab owner must retain focus"
        );
        frame
            .perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Activate)
            .unwrap();
        frame.poll_frame().unwrap();
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            (5 << 48) | 12
        );
        assert!(frame.inspection_json().unwrap().contains("notes"));
        // SAFETY: frame retains this boxed root throughout the test; this read
        // ends before sending input, and no borrowed reference survives a frame mutation.
        let tab_center = move |name: &str| unsafe {
            (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0].children()[0]
                .test_rects()
                .into_iter()
                .find(|(id, _)| id == name)
                .unwrap()
                .1
                .center()
        };
        let point = tab_center("tab:Writer");
        frame.click_at(point.x, point.y);
        frame.poll_frame().unwrap();
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            (5 << 48) | 10,
            "painted Writer tab click"
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
        frame.click_named("word_processor").unwrap();
        assert!(
            frame.frame_output().ime.is_some(),
            "clicked Writer must own the native IME route"
        );
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
        // Return to the Writer/Notes group's actual tabbar using keyboard
        // focus, then physically activate Notes once its tab is visible.
        frame.perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus).unwrap();
        // The input harness advances frames between press and release.
        unsafe {
            (&mut *root_ptr).surface.row_mut(StripId(2)).unwrap()
                .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
        }
        frame.relayout();
        let point = tab_center("tab:Notes");
        assert!(point.x >= 0.0 && point.x < 1280.0, "visible Notes tab: {point:?}");
        frame.click_at(point.x, point.y);
        frame.poll_frame().unwrap();
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            (5 << 48) | 12,
            "painted Notes tab click"
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
                        address: qos_actions::ActionAddress::new(resource, action),
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
}
