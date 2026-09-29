//! Portable retained host around the actual canonical DesktopSurface.
mod speculation;
use crate::desktop::{self, Admission};
use automerge::{AutoCommit, ObjId};
/// The window a desktop command receipt or committed notice names.
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(unused_imports, reason = "re-export used only by the wasm32 build")
)]
pub(crate) use speculation::opened as opened_window;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
    sync::Arc,
};
use taffy::{NodeId, TaffyTree};
use tilo_desktop::activity;
use tilo_desktop::{
    composition::{DesktopSurface, ReconcileOutcome, SurfaceChrome},
    intents::LocalViewIntent,
    resolver::{ApplicationCatalogTileResolver, ComponentContextSource, TileRequest},
    session_commands::DesktopCommandSink,
};
use tilo_desktop_model::apps::{AppCatalogue as _, OpenableApp};
use tilo_desktop_model::*;
use ui::widget::{AppModel, ModelPath};
use ui::{TextRenderer, Theme, presence::PresenceCtx};
use ui::{
    application::{
        ApplicationDefinition, ComponentCloseReport, HostContext, HostContextInputs, LaunchTarget,
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
fn active_route(tree: &DesktopTree) -> Option<crate::desktop_navigation::Route> {
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
/// Outcome of the last navigation command: the navigated route, or a refusal.
type NavigationResult = Result<Option<String>, String>;
#[derive(Clone)]
pub(crate) struct DesktopHandle {
    pub tree: Rc<RefCell<DesktopTree>>,
    pub permissions: Rc<RefCell<ui::permissions::PermissionContext>>,
    pub records: Rc<RefCell<BTreeMap<(String, String), OwnedRecord>>>,
    pub images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    pub errors: Rc<RefCell<Vec<String>>>,
    /// The desktop's own notices, shown as banners and kept in the
    /// notification centre.
    pub notifications: Rc<crate::desktop_notifications::BrowserNotifications>,
    pub session_requests:
        Rc<RefCell<Vec<(uuid::Uuid, tilo_desktop_model::commands::BoundCommand)>>>,
    inbox: Rc<RefCell<Option<(Admission, ui::permissions::PermissionContext)>>>,
    pub navigation: Rc<RefCell<serde_json::Value>>,
    pub navigation_revision: Rc<std::cell::Cell<u64>>,
    pub activation_sequence: Rc<std::cell::Cell<u64>>,
    pending_local_activation: Rc<RefCell<Option<crate::desktop_navigation::Route>>>,
    pub navigation_command: Rc<RefCell<Option<crate::desktop_navigation::Command>>>,
    pub navigation_result: Rc<RefCell<Option<NavigationResult>>>,
    pub(crate) generation: String,
    pub search: Rc<RefCell<crate::system_search::SearchState>>,
    history_available: Rc<std::cell::Cell<bool>>,
    /// LIVE_V1: window geometry captured after each draw and input dispatch.
    pub live: Rc<RefCell<crate::live_view::LiveLayout>>,
    /// LIVE_V1: tab id -> durable `(app, record)` of the component it hosts.
    pub tab_records: Rc<RefCell<BTreeMap<u64, (String, String)>>>,
    /// Optimistic local reorders awaiting durable confirmation.
    pub(crate) speculation: Rc<RefCell<speculation::Speculations>>,
    /// The page's own topology owner, for a desktop no peer replicates:
    /// bound commands are reduced here instead of queued for a peer.
    pub(crate) local: Option<Rc<RefCell<crate::local_desktop::LocalDesktop>>>,
}
impl DesktopHandle {
    /// Hand a bound command to the room's topology owner: the local owner
    /// reduces it into the next admission at once; otherwise it is
    /// speculated and queued for the peer. An open on an existing `record`
    /// needs the local owner.
    fn submit(
        &self,
        tree: Option<&DesktopTree>,
        command: tilo_desktop_model::commands::BoundCommand,
        record: Option<&str>,
    ) -> Result<(uuid::Uuid, bool), String> {
        let invocation = uuid::Uuid::new_v4();
        if let Some(local) = &self.local {
            let admission = {
                let mut local = local.borrow_mut();
                local.apply(&command, record)?;
                local.admission()?
            };
            let permissions = self.permissions.borrow().clone();
            *self.inbox.borrow_mut() = Some((admission, permissions));
            return Ok((invocation, false));
        }
        if record.is_some() {
            return Err("only the page's own desktop opens a window on an existing record".into());
        }
        let mut queue = self.session_requests.borrow_mut();
        if queue.len() >= 32 {
            return Err("desktop request queue is full".into());
        }
        let speculated =
            tree.is_some_and(|tree| DesktopRoot::speculate(self, tree, invocation, &command));
        queue.push((invocation, command));
        Ok((invocation, speculated))
    }
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
                tilo_desktop::intents::apply_local_view(
                    &mut tree,
                    LocalViewIntent::Workspace(workspace),
                );
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
                tilo_desktop::intents::apply_local_view(
                    &mut tree,
                    LocalViewIntent::Workspace(workspace),
                );
                tilo_desktop::intents::apply_local_view(&mut tree, LocalViewIntent::Strip(strip));
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
        if activates
            && let Some(instance) = target
                .as_ref()
                .and_then(|route| route.ids().ok())
                .map(|ids| ids.3)
        {
            frame.prepare_app_activation(instance);
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
        state: &tilo_peer_state::StatePeer,
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
    /// The page's content store, which standalone mounts of the same
    /// profile and device share (an EPUB Books opened, say).
    content: Option<tilo_content::ContentStore>,
}
impl Contexts {
    /// Prepare `definition`'s context for `request`: the record `target`
    /// opens, or with `create` the new record it names.
    fn context(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
        create: Option<&RecordCheckout>,
    ) -> Result<HostContext, ui::launch::LaunchError> {
        let route_app_id = tilo_desktop_model::apps::canonical_app_id(&request.tab.tile)
            .ok_or_else(|| {
                ui::launch::LaunchError::Operation("application identity unavailable".into())
            })?;
        let app_id = desktop::definition(&route_app_id)
            .ok_or_else(|| {
                ui::launch::LaunchError::Operation("application is not installed".into())
            })?
            .app_id;
        let store = self
            .owners
            .open(app_id, &self.room)
            .map_err(|e| ui::launch::LaunchError::Operation(e.to_string()))?;
        let checkout = self.checkout(definition, request)?;
        let inputs = HostContextInputs {
            records: store.clone(),
            views: self.views.clone(),
            device_id: self.device.clone(),
            instance_id: request.tab.id,
            permissions: self.permissions.clone(),
            author: ui::permissions::UserId(self.author.clone()),
            action_context: tilo_actions::ActionContext::default(),
        };
        let mut context = match create {
            Some(new) if !new.id.is_package_primary() => {
                HostContext::prepare_new_record(definition, inputs, new.id.clone())?
            }
            _ => HostContext::prepare(definition, inputs, target)?,
        };
        // A local desktop's records are the page's, which its other mounts
        // of the profile and device edit too.
        if self.handle.local.is_some() {
            context.write_as_author();
        }
        if let Some(content) = &self.content {
            context.provide_content(content.clone());
        }
        self.handle
            .tab_records
            .borrow_mut()
            .insert(request.tab.id, (app_id.into(), checkout.id.as_str().into()));
        self.handle.records.borrow_mut().insert(
            (app_id.into(), checkout.id.as_str().into()),
            OwnedRecord {
                app: app_id.into(),
                store,
                checkout,
            },
        );
        Ok(context)
    }
    /// A window the local owner created and whose record does not exist
    /// yet, launched through the app's own new-record path. `None` for
    /// every other tile.
    fn create(
        &mut self,
        catalog: &ui::application::ApplicationCatalog,
        request: &TileRequest<'_>,
    ) -> Option<tilo_desktop::resolver::TileResolution> {
        use tilo_desktop::resolver::TileResolution;
        let local = self.handle.local.clone()?;
        if !local.borrow().is_fresh(request.tab.id) {
            return None;
        }
        let app = tilo_desktop_model::apps::canonical_app_id(&request.tab.tile)?;
        let definition = catalog.resolve(&app).ok()?;
        let checkout = match self.checkout(definition, request) {
            Ok(checkout) => checkout,
            Err(error) => return Some(TileResolution::Failed(error)),
        };
        let exists = self
            .owners
            .open(definition.app_id, &self.room)
            .is_ok_and(|store| store.is_ready(&checkout.id));
        if exists {
            local.borrow_mut().settle(request.tab.id);
            return None;
        }
        let target = if checkout.id.is_package_primary() {
            LaunchTarget::PrimaryNew
        } else {
            LaunchTarget::New
        };
        let launched = self
            .context(definition, request, &target, Some(&checkout))
            .and_then(|context| definition.instantiate(context, target, RestoreInput::None));
        Some(match launched {
            Ok(component) => {
                local.borrow_mut().settle(request.tab.id);
                TileResolution::Component(component)
            }
            Err(error) => TileResolution::Failed(error),
        })
    }
}
/// The desktop's tile resolver: the installed catalogue, and for a local
/// owner's new windows the app's new-record launch.
struct Tiles<'a> {
    inner: &'a mut ApplicationCatalogTileResolver<Contexts>,
}
impl tilo_desktop::resolver::TileResolver for Tiles<'_> {
    fn resolve(&mut self, request: TileRequest<'_>) -> tilo_desktop::resolver::TileResolution {
        // A side module linked since the catalogue was taken (an app the
        // page opened later) joins it.
        if let Some(app) = tilo_desktop_model::apps::canonical_app_id(&request.tab.tile)
            && self.inner.catalog.resolve(&app).is_err()
            && desktop::definition(&app).is_some()
            && let Ok(catalog) = crate::catalog::catalog()
        {
            self.inner.catalog = catalog;
        }
        let ApplicationCatalogTileResolver { catalog, host } = &mut *self.inner;
        if let Some(created) = host.create(catalog, &request) {
            return created;
        }
        self.inner.resolve(request)
    }
    fn ready_to_replace(&self, tab: &Tab) -> bool {
        self.inner.ready_to_replace(tab)
    }
    fn creation_refusal(&self, tab: &Tab) -> Option<String> {
        self.inner.creation_refusal(tab)
    }
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
        let restore = self
            .views
            .load(request.tab.id)
            .map_err(|e| ui::launch::LaunchError::Operation(e.to_string()))?
            .map_or(RestoreInput::None, RestoreInput::Local);
        Ok((self.context(definition, request, target, None)?, restore))
    }
}
/// An optimistic open's window before its committed record exists: drawn at
/// once with the application's title, mounting nothing (no local record is
/// created). The committed tile differs (`AppPlaceholder`), so the surface
/// mounts the real application there when the echo lands.
struct PlaceholderTiles<'a, R> {
    inner: &'a mut R,
    placeholders: &'a std::collections::BTreeSet<u64>,
}
impl<R: tilo_desktop::resolver::TileResolver> tilo_desktop::resolver::TileResolver
    for PlaceholderTiles<'_, R>
{
    fn resolve(&mut self, request: TileRequest<'_>) -> tilo_desktop::resolver::TileResolution {
        if matches!(request.tab.tile, TileContent::Stub { .. })
            && self.placeholders.contains(&request.tab.id)
        {
            return tilo_desktop::resolver::TileResolution::Content(
                ui::widget::TileContent::Notice {
                    title: request.tab.title.clone(),
                    detail: "Opening…".into(),
                },
            );
        }
        self.inner.resolve(request)
    }
    fn ready_to_replace(&self, tab: &Tab) -> bool {
        self.inner.ready_to_replace(tab)
    }
    fn creation_refusal(&self, tab: &Tab) -> Option<String> {
        self.inner.creation_refusal(tab)
    }
}
struct ChromeSource {
    tree: Rc<RefCell<DesktopTree>>,
    intents: Rc<RefCell<Vec<tilo_desktop::chrome::ChromeIntent>>>,
}
impl tilo_desktop::chrome::DesktopChromeSource for ChromeSource {
    fn snapshot(&self) -> tilo_desktop::chrome::DesktopChromeSnapshot {
        tilo_desktop::chrome::DesktopChromeSnapshot::default()
    }
    fn tree(&self) -> DesktopTree {
        self.tree.borrow().clone()
    }
    fn intent(&self, intent: tilo_desktop::chrome::ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct LocalView {
    active: WorkspaceId,
    strips: HashMap<WorkspaceId, StripId>,
    groups: HashMap<StripId, GroupId>,
    #[serde(default)]
    group_order: HashMap<StripId, Vec<GroupId>>,
    tabs: Vec<(GroupId, TabId, WindowWidth, bool)>,
}
impl LocalView {
    fn from_tree(tree: &DesktopTree) -> Self {
        Self {
            active: tree.active,
            strips: tree.active_strip.clone(),
            groups: tree.active_group.clone(),
            group_order: tree
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .map(|strip| {
                    (
                        strip.id,
                        strip.window_groups.iter().map(|group| group.id).collect(),
                    )
                })
                .collect(),
            tabs: tree
                .workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .map(|g| (g.id, g.active_tab, g.width, g.maximized))
                .collect(),
        }
    }
    fn apply(&self, tree: &mut DesktopTree) {
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
                } else if let Some(closed) = self.groups.get(&s.id)
                    && !tree
                        .active_group
                        .get(&s.id)
                        .is_some_and(|id| s.window_groups.iter().any(|group| group.id == *id))
                {
                    // An admitted close removes the local selection along
                    // with its group. Continue through the group order that
                    // was visible before that patch: right first, then left.
                    // Older saved views did not record this order, but their
                    // tab rows still enumerate groups in the same order.
                    let mut order = self.group_order.get(&s.id).cloned().unwrap_or_default();
                    if order.is_empty() {
                        for (group, ..) in &self.tabs {
                            if !order.contains(group) {
                                order.push(*group);
                            }
                        }
                    }
                    tree.active_group.remove(&s.id);
                    if let Some(index) = order.iter().position(|group| group == closed) {
                        let successor = order[index + 1..]
                            .iter()
                            .chain(order[..index].iter().rev())
                            .find(|id| s.window_groups.iter().any(|group| group.id == **id));
                        if let Some(successor) = successor {
                            tree.active_group.insert(s.id, *successor);
                        }
                    }
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
/// The browser's [`DesktopCommandSink`]: view changes stay in this tab's
/// local view, bound commands are speculated and queued for the owner.
struct BrowserCommands<'a> {
    handle: &'a DesktopHandle,
    tree: &'a mut DesktopTree,
    room: &'a str,
    heads: &'a [[u8; 32]],
}
impl DesktopCommandSink for BrowserCommands<'_> {
    fn with_tree<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R {
        read(self.tree)
    }
    fn source(
        &mut self,
        room: RoomId,
    ) -> Result<tilo_desktop_model::commands::CommandSource, String> {
        if self.heads.is_empty() {
            return Err("desktop command has no admitted source revision".into());
        }
        // A peer's desktop is its one room; the local owner's has a room per
        // workspace.
        let room = if self.handle.local.is_some() {
            room.to_string()
        } else {
            self.room.to_owned()
        };
        Ok(tilo_desktop_model::commands::CommandSource {
            room,
            heads: self.heads.to_vec(),
        })
    }
    fn apply_local(&mut self, intent: LocalViewIntent) {
        DesktopRoot::apply_local(self.handle, self.tree, intent);
    }
    fn submit(
        &mut self,
        command: tilo_desktop_model::commands::BoundCommand,
    ) -> Result<(), String> {
        self.handle
            .submit(Some(self.tree), command, None)
            .map(|_| ())
    }
}
pub(crate) struct DesktopRoot {
    source_heads: Vec<[u8; 32]>,
    admitted_room: AutoCommit,
    required_records: Vec<desktop::Requirement>,
    unsupported_apps: Vec<String>,
    pub(crate) activity: Option<crate::desktop_activity::BrowserActivity>,
    overlays: crate::desktop_overlays::DesktopOverlays,
    pub(crate) activity_close_reason: Option<crate::desktop_activity::ActivityCloseReason>,
    pending_view_request: Option<activity::WorkspaceViewRequest>,
    surface_node: Option<NodeId>,
    activity_node: Option<NodeId>,
    notification_node: Option<NodeId>,
    /// The banner stack and, from the top bar, the notification centre.
    notification_layer: crate::desktop_notifications::NotificationLayer,
    surface: DesktopSurface,
    handle: DesktopHandle,
    resolver: ApplicationCatalogTileResolver<Contexts>,
    fallback: RecordStore,
    chrome: Rc<ChromeSource>,
    chrome_intents: Rc<RefCell<Vec<tilo_desktop::chrome::ChromeIntent>>>,
    storage: PortableStorage,
    view_key: StorageKey,
    notify: Arc<dyn Fn() + Send + Sync>,
    reports: Vec<ComponentCloseReport>,
    failures: Vec<ui::launch::LaunchError>,
    dirty: bool,
    view_changed: bool,
    /// Exactly the admitted topology (no local view, no speculation): the
    /// basis the rendered tree is rebuilt from when a speculation retires.
    admitted_tree: DesktopTree,
    /// Placeholder tabs whose local scratch record (in the desktop's own
    /// fallback store, never published) exists, so the "Opening…" tile binds.
    placeholder_records: std::collections::BTreeSet<u64>,
    /// Alt+Tab over the active workspace's windows. Browser windows have
    /// no live pictures, so every card shows its app mark.
    window_switcher: tilo_desktop::window_switcher::WindowSwitcher,
    /// Where the pointer last was, for the switcher's cards.
    cursor: Option<Point>,
    /// The window the switcher settled on, focused during the next update.
    pending_switch: Option<tilo_desktop::window_switcher::WindowAddress>,
}
/// What a canonical desktop is built from: its admitted records, the storage
/// they are cached in, the local permissions, and, for the page's own desktop,
/// its content store and seeded local desktop.
pub struct DesktopSources {
    pub admission: Admission,
    pub storage: PortableStorage,
    pub permissions: ui::permissions::PermissionContext,
    pub content: Option<tilo_content::ContentStore>,
    pub local: Option<crate::local_desktop::LocalDesktop>,
}

impl DesktopRoot {
    /// LIVE_V1: record the laid-out window groups, the focused group and any
    /// window drag for the live overlay (rectangles only; no document reads).
    fn capture_live(&self) {
        let mut layout = crate::live_view::LiveLayout {
            covered: self.activity.is_some(),
            ..Default::default()
        };
        // Tabs of an app this build cannot run: a legacy window, which the
        // live overlay fills with the session's video of it.
        let legacy_apps: BTreeMap<u64, String> = self
            .handle
            .tree
            .try_borrow()
            .map(|tree| {
                tree.workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .filter_map(|tab| {
                        let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
                        crate::desktop::definition(&app)
                            .is_none()
                            .then_some((tab.id, app))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for strip in self.surface.strip_ids() {
            let Some(row) = self.surface.row(strip) else {
                continue;
            };
            for group in &row.groups {
                // The shell marks the active group of the active strip.
                if group.focused {
                    layout.focused_group = Some(group.id);
                }
                let tab = group.active_tab_id();
                layout.groups.push(crate::live_view::LiveGroup {
                    id: group.id,
                    rect: group.get_rect(),
                    tab,
                    scroll: ui::Widget::primary_vertical_scroll(group)
                        .map(|region| region.metrics.offset),
                    legacy: legacy_apps
                        .get(&tab)
                        .and_then(|app| Some((app.clone(), group.content_rect_for_tab(tab)?))),
                });
            }
            if layout.drag.is_none() {
                let resize = row
                    .dragging_handle()
                    .and_then(|index| row.groups.get(index))
                    .or_else(|| {
                        row.groups
                            .iter()
                            .find(|group| group.edge_resize_in_flight())
                    })
                    .map(|group| group.id);
                layout.drag =
                    crate::live_view::LiveLayout::drag_from_row(row.group_drag_preview(), resize);
            }
        }
        if layout.focused_group.is_none() {
            // External navigation: the canonical active route (active group of
            // the active strip, else its first group) owns keyboard focus.
            if let Ok(tree) = self.handle.tree.try_borrow() {
                layout.focused_group = tree
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == tree.active)
                    .and_then(|workspace| {
                        let strip = tree
                            .active_strip
                            .get(&workspace.id)
                            .and_then(|id| workspace.strips.iter().find(|s| s.id == *id))
                            .or_else(|| workspace.strips.first())?;
                        tree.active_group
                            .get(&strip.id)
                            .copied()
                            .filter(|id| strip.window_groups.iter().any(|g| g.id == *id))
                            .or_else(|| strip.window_groups.first().map(|g| g.id))
                    });
            }
        }
        if let Some(focused) = layout.focused_group {
            layout.focused_tab = layout
                .groups
                .iter()
                .find(|group| group.id == focused)
                .map(|group| group.tab);
            layout.window_presence = self
                .surface
                .strip_ids()
                .into_iter()
                .filter_map(|strip| self.surface.row(strip))
                .find_map(|row| row.groups.iter().find(|group| group.id == focused))
                .map(|group| ui::presence::capability::subtree_presence(group))
                .unwrap_or_default();
        }
        let mut live = self.handle.live.borrow_mut();
        if *live != layout {
            *live = layout;
        }
    }
    fn queue_activation_after_route_change(
        handle: &DesktopHandle,
        tree: &DesktopTree,
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

    fn apply_local(handle: &DesktopHandle, tree: &mut DesktopTree, intent: LocalViewIntent) {
        let previous = active_route(tree);
        tilo_desktop::intents::apply_local_view(tree, intent);
        Self::queue_activation_after_route_change(handle, tree, previous);
    }

    fn switch_workspace(handle: &DesktopHandle, tree: &mut DesktopTree, delta: i32) {
        let previous = active_route(tree);
        if let Some(index) = tree.workspaces.iter().position(|w| w.id == tree.active) {
            let next =
                (index as i64 + i64::from(delta)).rem_euclid(tree.workspaces.len() as i64) as usize;
            tree.active = tree.workspaces[next].id;
        }
        Self::queue_activation_after_route_change(handle, tree, previous);
    }
    pub fn new(
        sources: DesktopSources,
        room: &str,
        device: &str,
        author: &str,
        instance: u64,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(Self, DesktopHandle), String> {
        let DesktopSources {
            admission,
            storage,
            permissions,
            content,
            local,
        } = sources;
        if !admission.complete() {
            return Err("required desktop records are not admitted".into());
        }
        let mut tree = admission.tree;
        let admitted_tree = tree.clone();
        let namespace = speculation::namespace(&admission.admitted_room, &admitted_tree);
        let view_key = StorageKey::View {
            workspace: format!("desktop:{room}"),
            instance,
        };
        let local = local.map(|local| Rc::new(RefCell::new(local)));
        // A local desktop presents as its page seeds it, not as a saved view.
        if let Some(local) = &local {
            if let Some(tab) = local.borrow_mut().take_focus() {
                crate::local_desktop::select(&mut tree, tab);
            }
        } else if let Some(saved) = storage.get(&view_key) {
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
            notifications: Rc::default(),
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
            live: Rc::default(),
            tab_records: Rc::default(),
            speculation: Rc::default(),
            local,
        };
        let chrome_intents: Rc<RefCell<Vec<tilo_desktop::chrome::ChromeIntent>>> = Rc::default();
        let chrome = Rc::new(ChromeSource {
            tree: handle.tree.clone(),
            intents: chrome_intents.clone(),
        });
        let mut owners = RecordOwners::portable(storage.clone());
        let fallback = owners
            .open("so.tilo.desktop", room)
            .map_err(|e| e.to_string())?;
        let views =
            Rc::new(InstanceViewStore::portable(storage.clone(), room).map_err(|e| e.to_string())?);
        let catalog = crate::catalog::catalog().map_err(|e| e.to_string())?;
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
                content,
            },
        };
        let tree = handle.tree.borrow();
        let surface = ui::ImageStore::with_host_factory_with_max_dimension(
            Self::image_factory(&handle),
            || {
                DesktopSurface::build(
                    &tree,
                    &fallback,
                    &mut Tiles {
                        inner: &mut resolver,
                    },
                    Self::chrome(&tree, chrome.clone(), notify.clone()),
                )
            },
        );
        drop(tree);
        let mut root = Self {
            source_heads: admission.source_heads,
            admitted_room: admission.admitted_room,
            required_records: admission.required,
            unsupported_apps: admission.unsupported,
            activity: None,
            overlays: Default::default(),
            activity_close_reason: None,
            pending_view_request: None,
            surface_node: None,
            activity_node: None,
            notification_node: None,
            notification_layer: crate::desktop_notifications::NotificationLayer::new(
                handle.notifications.clone(),
            ),
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
            admitted_tree,
            placeholder_records: Default::default(),
            window_switcher: Default::default(),
            cursor: None,
            pending_switch: None,
        };
        handle.speculation.borrow_mut().set_namespace(namespace);
        root.collect_failures();
        root.refresh_navigation(true);
        Ok((root, handle))
    }
    /// Alt+Tab and the switcher it opens. Returns whether the event was
    /// the switcher's.
    fn route_window_switcher_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        use tilo_desktop::window_switcher::{PlainWindows, SwitcherOutcome, SwitcherScope};
        use winit::event::{ElementState, WindowEvent};
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
        }
        let now = ui::time::Instant::now();
        if !self.window_switcher.is_active() {
            let WindowEvent::KeyboardInput { event: key, .. } = event else {
                return false;
            };
            if key.state != ElementState::Pressed || self.activity.is_some() {
                return false;
            }
            let Some(scope) = SwitcherScope::for_chord(&key.logical_key, modifiers.state()) else {
                return false;
            };
            if !key.repeat {
                self.window_switcher.set_host_rect(self.surface.get_rect());
                let began = self.handle.tree.try_borrow().is_ok_and(|tree| {
                    self.window_switcher.begin(
                        &tree,
                        &PlainWindows,
                        scope,
                        modifiers.state().shift_key(),
                        now,
                    )
                });
                if began {
                    (self.notify)();
                }
            }
            return true;
        }
        let outcome = self
            .window_switcher
            .route_event(event, modifiers.state(), self.cursor, now);
        match outcome {
            SwitcherOutcome::Ignored => return false,
            SwitcherOutcome::Consumed | SwitcherOutcome::Cancelled => {}
            SwitcherOutcome::Commit(address) => {
                self.pending_switch = Some(address);
                self.dirty = true;
            }
        }
        (self.notify)();
        !matches!(event, WindowEvent::ModifiersChanged(_))
    }
    /// Focus the window the switcher chose. A window closed while Alt was
    /// held is skipped. Only call during update, after the runner has
    /// captured outgoing focus.
    fn focus_switcher_window(
        &mut self,
        address: tilo_desktop::window_switcher::WindowAddress,
    ) -> bool {
        let mut tree = self.handle.tree.borrow_mut();
        let live = tree
            .active_workspace()
            .and_then(|workspace| workspace.strips.iter().find(|s| s.id == address.strip))
            .and_then(|strip| strip.window_groups.iter().find(|g| g.id == address.group))
            .is_some_and(|group| group.tabs.iter().any(|tab| tab.id == address.tab));
        if !live {
            return false;
        }
        self.surface.reveal_on_next_layout(address.strip);
        Self::apply_local(
            &self.handle,
            &mut tree,
            LocalViewIntent::Tab {
                group: address.group,
                tab: address.tab,
            },
        );
        drop(tree);
        self.view_changed = true;
        self.reconcile()
    }
    fn image_factory(handle: &DesktopHandle) -> Rc<dyn Fn(u32) -> ui::ImageStore> {
        let images = handle.images.clone();
        Rc::new(move |max_dimension| {
            let (store, loader) = ui::ImageStore::host_driven_with_max_dimension(max_dimension);
            images.borrow_mut().push(loader);
            store
        })
    }
    fn chrome(
        tree: &DesktopTree,
        source: Rc<ChromeSource>,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> SurfaceChrome {
        SurfaceChrome {
            navigation: tilo_desktop::composition::NavigationChrome::External,
            topbar: Box::new(
                tilo_desktop::topbar::TopBarChrome::from_tree(tree).with_chrome(source),
            ),
            // Navigation sits beside the surface, so no rail covers the canvas.
            empty_workspace: Some(Box::new(
                tilo_desktop::launchpad::CollaborationLaunchpad::new(|_| 0.0),
            )),
            notify: Some(notify),
            ..SurfaceChrome::default()
        }
    }
    fn collect_failures(&mut self) {
        for error in self.surface.take_setup_failures() {
            self.handle.errors.borrow_mut().push(error.to_string());
            self.handle.notifications.post(
                tilo_desktop::notifications::Notification::new(
                    tilo_desktop::i18n::t("notification-app-failed-title"),
                    error.to_string(),
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error),
            );
            self.failures.push(error);
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
        let tree = self.handle.tree.borrow().clone();
        // A local desktop admits one room per workspace.
        let room = self.handle.local.as_ref().and_then(|local| {
            let room = tree.active_workspace()?.room;
            local.borrow().room(room).cloned()
        });
        activity::ActivitySnapshot::new(tree, room.unwrap_or_else(|| self.admitted_room.clone()))
    }
    fn close_activity(&mut self, reason: crate::desktop_activity::ActivityCloseReason) {
        if let Some(mut activity) = self.activity.take() {
            activity.finish();
            self.activity_close_reason = Some(reason);
            self.dirty = true;
        }
    }
    /// Queue a desktop command for the room's topology owner, rendering its
    /// expected result on the next frame; an open with `record` shows that
    /// existing record.
    fn enqueue_command(
        &mut self,
        command: tilo_desktop_model::commands::BoundCommand,
        record: Option<&str>,
    ) -> Result<uuid::Uuid, String> {
        // Render the reorder on the next frame; the durable command
        // confirms (or snaps back) in the background.
        let submitted = match self.handle.tree.try_borrow() {
            Ok(tree) => self.handle.submit(Some(&tree), command, record),
            Err(_) => self.handle.submit(None, command, record),
        };
        let (invocation, speculated) = submitted.inspect_err(|error| {
            self.handle.errors.borrow_mut().push(error.clone());
        })?;
        self.view_changed |= speculated;
        self.dirty = true;
        (self.notify)();
        Ok(invocation)
    }
    /// Route the empty-workspace card the person chose through the same
    /// open and Activity paths the palette and navigation use.
    fn launchpad_request(&mut self, action: tilo_desktop::launchpad::LaunchpadAction) {
        use tilo_desktop::launchpad::LaunchpadAction;
        use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand};
        let (active, room) = {
            let tree = self.handle.tree.borrow();
            (
                tree.active,
                tree.active_workspace().map(|w| w.room.to_string()),
            )
        };
        let result = match (action.app_target(), action) {
            (Some((app, _)), _) => match room {
                Some(room) => self
                    .enqueue_command(
                        BoundCommand {
                            source: CommandSource {
                                room,
                                heads: self.source_heads.clone(),
                            },
                            command: DesktopCommand::OpenApp { app: app.into() },
                        },
                        None,
                    )
                    .map(|_| ()),
                None => Err("No workspace is active".into()),
            },
            (None, LaunchpadAction::Activity) => {
                self.queue_view_request(activity::WorkspaceViewRequest::OpenActivity {
                    workspace: active,
                    view: activity::ActivityView::default(),
                })
            }
            (None, _) => Err("Inviting people is not available in this peer yet".into()),
        };
        if let Err(error) = result {
            self.handle.errors.borrow_mut().push(error);
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
                        &self.resolver.host.author,
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
            &self.resolver.host.author,
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
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
                    if target.app_id.as_deref() != Some(&app) {
                        return None;
                    }
                    let checkout = desktop::checkout(
                        desktop::definition(&app)?,
                        tab.id,
                        &self.resolver.host.routes,
                    )
                    .ok()?;
                    (checkout.id.as_str() == target.id && checkout.kind.names(&target.kind))
                        .then_some((group, tab.id))
                });
            if let Some((group, tab)) = destination {
                self.close_activity(ActivityCloseReason::ResourceOpened);
                tilo_desktop::intents::apply_local_view(
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
        // Optimistic open first: paint the window this frame and leave the
        // global search index as it is. Its bookkeeping runs on the frame that
        // admits the committed window (a placeholder is never a search target).
        let opening = self
            .handle
            .speculation
            .try_borrow()
            .map(|speculation| speculation.placeholders())
            .unwrap_or_default();
        if opening.is_empty() {
            self.handle.search.borrow_mut().refresh(
                &self.handle.generation,
                &self.source_heads,
                &self.handle.tree.borrow(),
                &self.resolver.host.routes,
                active.as_ref(),
            );
        }
        let mut snapshot = self.handle.navigation.borrow_mut();
        let mut changed = false;
        if topology || snapshot.is_null() {
            let tree = self.handle.tree.borrow();
            // Optimistic opens not yet committed are marked `opening`.
            let opening = &opening;
            // Each window's app and the record it shows (null for an app's
            // one package-primary document), for the page to mount elsewhere.
            let routes = &self.resolver.host.routes;
            let shows = move |t: &tilo_desktop_model::Tab| {
                let app = tilo_desktop_model::apps::canonical_app_id(&t.tile);
                let record = app
                    .as_deref()
                    .and_then(desktop::definition)
                    .and_then(|definition| desktop::checkout(definition, t.id, routes).ok())
                    .filter(|checkout| !checkout.id.is_package_primary())
                    .map(|checkout| checkout.id.as_str().to_owned());
                (app, record)
            };
            let entries: Vec<_> = tree.workspaces.iter().flat_map(|w| w.strips.iter().flat_map(move |s| s.window_groups.iter().flat_map(move |g| g.tabs.iter().map(move |t| { let (app, record) = shows(t); let mut entry = json!({"route":{"workspace":w.id.0.to_string(),"strip":s.id.0.to_string(),"group":g.id.to_string(),"tab":t.id.to_string()},"label":t.title,"stripLabel":s.name,"workspaceLabel":w.name,"app":app,"record":record}); if opening.contains(&t.id) { entry["opening"] = json!(true); } entry })))).collect();
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
            let applications = desktop::Bundled
                .openable()
                .into_iter()
                .map(|entry| json!({"id":entry.app_id,"label":entry.title}))
                .collect::<Vec<_>>();
            let next = json!({"version":1,"generation":self.handle.generation,"entries":entries,"workspaces":workspaces,"applications":applications,"activeWorkspace":tree.active.0.to_string(),"activeStrip":tree.active_workspace().and_then(|w| tree.active_strip.get(&w.id).copied().or_else(||w.strips.first().map(|s|s.id))).map(|s|s.0.to_string()),"active":active,"widthPermille":group.map(|g|g.width.permille()),"maximized":group.is_some_and(|g|g.maximized),"capabilities":{"select":true,"width":true,"maximize":true,"strips":true,"commands":true,"globalSearch":true,"close":true,"closeInactive":true,"open":true},"history":snapshot.get("history").cloned().unwrap_or(serde_json::Value::Null),"scroll":snapshot.get("scroll").cloned().unwrap_or(serde_json::Value::Null),"desktopPan":snapshot.get("desktopPan").cloned().unwrap_or(serde_json::Value::Null)});
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
            ui::widget::PrimaryScrollAction::Start.offset(m)?;
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
        let capabilities = json!({"select":true,"width":true,"maximize":true,"strips":true,"commands":true,"globalSearch":true,"close":true,"closeInactive":true,"open":true,
            "workspaceSelect":!self.handle.tree.borrow().workspaces.is_empty(),"workspaceHistory":available,"workspaceReplay":available});
        let activity = self.activity.as_ref().filter(|_| available).map(|activity| json!({
            "workspace":activity.workspace.0.to_string(), "view":activity.pane.view(),
            "playing":activity.pane.replay_is_playing(),
            "coverage":if activity.pane.feed_coverage().is_complete() { "complete" } else { "partial" }
        })).unwrap_or(serde_json::Value::Null);
        let notifications = {
            let log =
                tilo_desktop::notifications::NotificationHost::log(&*self.handle.notifications);
            json!({"banners":log.active().len(),"retained":log.history().len(),
                "centerOpen":self.notification_layer.center_open()})
        };
        let settings = self
            .overlays
            .settings_tab()
            .filter(|_| self.activity.is_none())
            .map(|tab| {
                json!({"workspace":self.handle.tree.borrow().active.0.to_string(),
                    "tab":crate::desktop_navigation::SettingsTab::from(tab)})
            })
            .unwrap_or(serde_json::Value::Null);
        for (key, value) in [
            ("capabilities", capabilities),
            ("notifications", notifications),
            ("workspaceSettings", settings),
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
    fn run_desktop_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        if kind == tilo_actions::OperationKind::View
            && type_id == activity::view_actions::OPERATION_TYPE
        {
            let request = serde_json::from_slice(payload)
                .map_err(|e| format!("Invalid workspace view request: {e}"))?;
            self.queue_view_request(request)?;
            return Ok(Some(Default::default()));
        }
        if kind != tilo_actions::OperationKind::View || type_id != "tilo.desktop.request" {
            return Ok(None);
        }
        let command: tilo_desktop_model::commands::BoundCommand =
            serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let invocation = self.enqueue_command(command, None)?;
        Ok(Some(tilo_actions::ActionOutput {
            value: Some(serde_json::Value::String(invocation.to_string())),
            resources: vec![],
        }))
    }
    fn navigate(
        &mut self,
        command: crate::desktop_navigation::Command,
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<(bool, Option<String>), String> {
        use crate::desktop_navigation::Command;
        if command.generation() != self.handle.generation {
            return Err("Stale desktop surface".into());
        }
        if let Command::Close { route, .. } = &command {
            let (workspace_id, strip, group, tab) = route.ids()?;
            let tree = self.handle.tree.borrow();
            let workspace = tree
                .active_workspace()
                .filter(|workspace| workspace.id == workspace_id)
                .ok_or("Workspace is no longer active")?;
            let group_exists = workspace
                .strips
                .iter()
                .find(|candidate| candidate.id == strip)
                .and_then(|candidate| {
                    candidate
                        .window_groups
                        .iter()
                        .find(|candidate| candidate.id == group)
                })
                .is_some_and(|candidate| {
                    candidate.tabs.iter().any(|candidate| candidate.id == tab)
                });
            if !group_exists {
                return Err("Application is no longer available".into());
            }
            drop(tree);
            let location = tilo_desktop_model::commands::TabLocation {
                strip: strip.0,
                group,
                tab,
            };
            if let Some((invocation, _)) = self.handle.session_requests.borrow().iter().find(|(_, bound)| matches!(&bound.command, tilo_desktop_model::commands::DesktopCommand::CloseTab { location: pending } if pending == &location)) {
                return Ok((false, Some(invocation.to_string())));
            }
            let resources = self.resources_for_close(doc, Some(location));
            let address = tilo_actions::ActionAddress::new("desktop:requests", "close-tab");
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .map_err(|error| error.to_string())?;
            let output = self
                .run_desktop_operation(
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                )?
                .ok_or("Close action unavailable")?;
            return output
                .value
                .and_then(|value| value.as_str().map(str::to_owned))
                .map(|invocation| (false, Some(invocation)))
                .ok_or_else(|| "Close request identity unavailable".into());
        }
        if let Command::WorkspaceSettings { workspace, .. }
        | Command::CloseWorkspaceSettings { workspace, .. } = &command
        {
            if self.handle.tree.borrow().active.0.to_string() != *workspace {
                return Err("Workspace is no longer active".into());
            }
            match command {
                Command::WorkspaceSettings { tab, .. } => {
                    // The settings are drawn over the desktop, never over Activity.
                    self.close_activity(crate::desktop_activity::ActivityCloseReason::Dismissed);
                    self.overlays.open_settings(tab.into());
                }
                _ => self.overlays.close_settings(),
            }
            self.dirty = true;
            (self.notify)();
            return Ok((false, None));
        }
        if let Command::Open { app, record, .. } = &command {
            let app = desktop::definition(app)
                .ok_or("application cannot be opened here")?
                .app_id;
            if record
                .as_deref()
                .is_some_and(|record| ui::record::RecordId::parse(record).is_none())
            {
                return Err("invalid record".into());
            }
            let room = self
                .handle
                .tree
                .borrow()
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
                .ok_or("No workspace is active")?;
            let invocation = self.enqueue_command(
                tilo_desktop_model::commands::BoundCommand {
                    source: tilo_desktop_model::commands::CommandSource {
                        room,
                        heads: self.source_heads.clone(),
                    },
                    command: tilo_desktop_model::commands::DesktopCommand::OpenApp {
                        app: app.into(),
                    },
                },
                record.as_deref(),
            )?;
            return Ok((false, Some(invocation.to_string())));
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
                    && strip.window_groups.iter().any(|group| group.maximized)
                {
                    for group in &mut strip.window_groups {
                        group.maximized = group.id == g;
                    }
                }
                tilo_desktop::intents::apply_local_view(&mut tree, LocalViewIntent::Workspace(w));
                tilo_desktop::intents::apply_local_view(
                    &mut tree,
                    LocalViewIntent::Tab { group: g, tab: t },
                );
            }
            Command::Width { permille, .. } => {
                if !(100..=1000).contains(&permille) {
                    return Err("Width must be between 10% and 100%".into());
                }
                self.surface.reveal_on_next_layout(s);
                tilo_desktop::intents::apply_local_view(
                    &mut tree,
                    LocalViewIntent::Maximized {
                        group: g,
                        maximized: false,
                    },
                );
                tilo_desktop::intents::apply_local_view(
                    &mut tree,
                    LocalViewIntent::Width {
                        group: g,
                        width: WindowWidth::Fraction(permille),
                    },
                );
            }
            Command::Maximize { maximized, .. } => tilo_desktop::intents::apply_local_view(
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
    /// Record a queued reorder, open or close for optimistic rendering
    /// (applied by the next reconcile). The overlay ghost for remote members
    /// is derived from the layout drawn before the reorder.
    fn speculate(
        handle: &DesktopHandle,
        tree: &DesktopTree,
        invocation: uuid::Uuid,
        command: &tilo_desktop_model::commands::BoundCommand,
    ) -> bool {
        let Ok(mut speculation) = handle.speculation.try_borrow_mut() else {
            return false;
        };
        if !speculation.enabled || !speculation::eligible(&command.command) {
            return false;
        }
        // A member who cannot write the room (a viewer) would only see the
        // window flash and snap back: its command stays durable-only.
        if !handle.permissions.try_borrow().is_ok_and(|permissions| {
            permissions.check(&ui::permissions::PermissionScope::workspace(
                ui::permissions::Verb::Write,
            ))
        }) {
            return false;
        }
        let preview = handle
            .live
            .try_borrow()
            .ok()
            .and_then(|layout| speculation::preview(tree, &layout, &command.command));
        speculation.push(invocation, command, preview, tree, web_time::Instant::now())
    }
    /// Retire settled speculation and (re)apply the outstanding entries: all
    /// of them on a new admission or after a retirement (from the durable
    /// basis), else only newly queued ones. `admitted` is the local view
    /// captured before an admission replaced the tree. Returns whether the
    /// tree changed.
    fn rebase_speculation(&self, tree: &mut DesktopTree, admitted: Option<&LocalView>) -> bool {
        let Ok(mut speculation) = self.handle.speculation.try_borrow_mut() else {
            return false;
        };
        if speculation.idle() {
            return false;
        }
        let retired = speculation.retire(&self.admitted_tree, web_time::Instant::now());
        let rebuilt;
        let local = match admitted {
            Some(local) => Some(local),
            None if retired => {
                rebuilt = LocalView::from_tree(tree);
                *tree = self.admitted_tree.clone();
                rebuilt.apply(tree);
                Some(&rebuilt)
            }
            None => None,
        };
        let changed = speculation.apply(tree, local.is_some()) || retired;
        if let Some(local) = local {
            // Re-select placeholder groups the durable basis lacked, then move
            // focus from retired placeholders to their committed windows.
            local.apply(tree);
            for moved in speculation.take_focus_moves() {
                let from = StripId(moved.from.strip);
                if local.groups.get(&from) != Some(&moved.from.group) {
                    continue;
                }
                let to = StripId(moved.to.strip);
                let Some(workspace) = tree
                    .workspaces
                    .iter()
                    .find(|workspace| {
                        workspace.strips.iter().any(|strip| {
                            strip.id == to
                                && strip.window_groups.iter().any(|g| g.id == moved.to.group)
                        })
                    })
                    .map(|workspace| workspace.id)
                else {
                    continue;
                };
                if local.strips.get(&workspace) == Some(&from) {
                    tree.active_strip.insert(workspace, to);
                }
                tree.active_group.insert(to, moved.to.group);
            }
            speculation.refocus_fresh(tree);
        }
        changed
    }
    fn reconcile(&mut self) -> bool {
        let intents = self.surface.take_intents();
        let chrome = std::mem::take(&mut *self.chrome_intents.borrow_mut());
        let inbox = self.handle.inbox.borrow_mut().take();
        let admission_changed = inbox.is_some();
        let mut admitted_local = None;
        let mut tree = self.handle.tree.borrow_mut();
        let mut changed = std::mem::take(&mut self.view_changed)
            || !intents.is_empty()
            || !chrome.is_empty()
            || inbox.is_some();
        // Explicit workspace switches are newer than gestures harvested from the
        // outgoing surface. Save those tab choices without stealing active focus.
        for intent in chrome {
            match intent {
                tilo_desktop::chrome::ChromeIntent::SwitchWorkspace(delta) => {
                    Self::switch_workspace(&self.handle, &mut tree, delta);
                }
                tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceSettings => {
                    self.overlays.toggle_settings();
                }
                tilo_desktop::chrome::ChromeIntent::ToggleCollaborators => {
                    self.overlays.toggle_collaborators();
                }
                // The browser has no Quick Settings; its top-bar control opens
                // the notification centre.
                tilo_desktop::chrome::ChromeIntent::ToggleQuickSettings => {
                    self.notification_layer.toggle_center();
                }
                tilo_desktop::chrome::ChromeIntent::ShowQuickSettings => {
                    self.notification_layer.show_center();
                }
                _ => {
                    self.handle
                        .errors
                        .borrow_mut()
                        .push("This desktop service is not available in this peer yet".into());
                    self.handle
                        .notifications
                        .post(tilo_desktop::notifications::Notification::new(
                            tilo_desktop::i18n::t("notification-service-unavailable"),
                            "",
                        ));
                }
            }
        }
        let mut sink = BrowserCommands {
            handle: &self.handle,
            tree: &mut tree,
            room: &self.resolver.host.room,
            heads: &self.source_heads,
        };
        for intent in intents {
            if let Err(error) = tilo_desktop::session_commands::dispatch(&mut sink, intent) {
                self.handle.errors.borrow_mut().push(error);
            }
        }
        if let Some((admission, permissions)) = inbox {
            self.handle.history_available.set(true);
            self.source_heads = admission.source_heads;
            self.admitted_room = admission.admitted_room;
            self.required_records = admission.required;
            self.unsupported_apps = admission.unsupported;
            let local = LocalView::from_tree(&tree);
            self.admitted_tree = admission.tree.clone();
            *tree = admission.tree;
            local.apply(&mut tree);
            self.resolver.host.routes = admission.routes;
            *self.resolver.host.permissions.borrow_mut() = permissions;
            if let Ok(mut speculation) = self.handle.speculation.try_borrow_mut() {
                speculation.set_namespace(speculation::namespace(
                    &self.admitted_room,
                    &self.admitted_tree,
                ));
            }
            admitted_local = Some(local);
            // The local owner's open selects its new window, as a shell's does.
            if let Some(tab) = self
                .handle
                .local
                .as_ref()
                .and_then(|local| local.borrow_mut().take_focus())
            {
                let previous = active_route(&tree);
                if let Some(strip) = crate::local_desktop::select(&mut tree, tab) {
                    self.surface.reveal_on_next_layout(strip);
                }
                Self::queue_activation_after_route_change(&self.handle, &tree, previous);
            }
        }
        changed |= self.rebase_speculation(&mut tree, admitted_local.as_ref());
        drop(tree);
        // Pane resource navigation must select its destination before mounting
        // and restoring focus, so the outgoing editor is no longer eligible.
        self.refresh_activity(admission_changed);
        changed |= std::mem::take(&mut self.view_changed);
        let tree = self.handle.tree.borrow();
        if changed {
            let factory = Self::image_factory(&self.handle);
            let placeholders = self
                .handle
                .speculation
                .try_borrow()
                .map(|speculation| speculation.placeholders())
                .unwrap_or_default();
            // An unchecked tile binds the tab's scratch record; create it for a
            // placeholder before its first build (as the minting action does).
            for tab in &placeholders {
                if self.placeholder_records.insert(*tab) {
                    let _ = tilo_desktop::records::create_scratch_tab_record(&self.fallback, *tab);
                }
            }
            let mut tiles = Tiles {
                inner: &mut self.resolver,
            };
            let mut resolver = PlaceholderTiles {
                inner: &mut tiles,
                placeholders: &placeholders,
            };
            let result =
                ui::ImageStore::with_host_factory_with_max_dimension(factory.clone(), || {
                    self.surface.reconcile(&tree, &self.fallback, &mut resolver)
                });
            if result == ReconcileOutcome::RebuildRequired {
                ui::ImageStore::with_host_factory_with_max_dimension(factory, || {
                    self.surface.rebuild_retaining(
                        &tree,
                        &self.fallback,
                        &mut resolver,
                        Self::chrome(&tree, self.chrome.clone(), self.notify.clone()),
                    )
                });
                self.dirty = true;
            }
            if self.handle.local.is_none()
                && let Ok(bytes) = serde_json::to_vec(&LocalView::from_tree(&tree))
                && self
                    .storage
                    .get(&self.view_key)
                    .is_none_or(|saved| saved.bytes != bytes)
                && let Err(e) = self.storage.put(self.view_key.clone(), bytes, None)
            {
                self.handle.errors.borrow_mut().push(e);
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
    fn resources_for_close(
        &self,
        _doc: &AutoCommit,
        close_target: Option<tilo_desktop_model::commands::TabLocation>,
    ) -> Vec<tilo_actions::Resource> {
        use tilo_actions::{
            ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
        };
        use tilo_desktop_model::commands::*;
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
                "tilo.desktop.request",
                &BoundCommand {
                    source: source.clone(),
                    command,
                },
            ) {
                actions.push(action);
            }
        };
        if let Some(location) = close_target {
            add(
                "close-tab".into(),
                "Close application".into(),
                DesktopCommand::CloseTab { location },
            );
        }
        // The one open for people and agents (`desktop:app:<id>#open` is the
        // same command): placement-independent, placed by the owner's turn.
        for OpenableApp {
            app_id: app,
            title: label,
        } in desktop::Bundled.openable()
        {
            add(
                format!("open-{app}"),
                format!("Open {label}"),
                DesktopCommand::OpenApp { app: app.into() },
            );
        }
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
            if let Some(group) = active_group
                && let Some(tab) = group.tabs.iter().find(|t| t.id == group.active_tab)
            {
                let location = TabLocation {
                    strip: strip.id.0,
                    group: group.id,
                    tab: tab.id,
                };
                if close_target.is_none() {
                    add(
                        "close-tab".into(),
                        format!("Close {}", tab.title),
                        DesktopCommand::CloseTab { location },
                    );
                }
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
                            before: strip.window_groups.get(group_index + 2).map(|item| item.id),
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
        for strip in &workspace.strips {
            if strip.window_groups.is_empty() && workspace.strips.len() > 1 {
                add(
                    format!("remove-strip-{}", strip.id.0),
                    format!("Remove empty strip {}", strip.name),
                    DesktopCommand::RemoveStrip { strip: strip.id.0 },
                );
            }
        }
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
                "tilo.desktop.request",
                vec![ActionParam::required("name", ParamType::Text, "Name")],
                move |args| {
                    let name = args.text("name").unwrap_or_default().to_string();
                    EncodedAction::encode_operation(
                        "tilo.desktop.request",
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
            "tilo.desktop.request",
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "Workspace name",
            )],
            move |args| {
                EncodedAction::encode_operation(
                    "tilo.desktop.request",
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
}

impl Widget for DesktopRoot {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::component_lifecycle::ComponentLifecycle
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "tilo.desktop.request",
            activity::view_actions::OPERATION_TYPE,
        ]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .run_desktop_operation(request.kind, request.type_id, request.payload)?
            .map(ui::widget::ResourceOperationStart::Completed))
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        self.surface.sync(doc, path);
    }
    fn update(&mut self, doc: &mut AppModel, path: &ModelPath) -> bool {
        let (doc, path) = ui::doc::document(doc, path);
        // Process the current admission before validating a queued local view.
        let mut changed = self.reconcile();
        if let Some(request) = self.pending_view_request.take() {
            if let Err(error) = self.apply_view_request(request) {
                self.handle.errors.borrow_mut().push(error);
            }
            changed |= self.reconcile();
        }
        if let Some(address) = self.pending_switch.take() {
            changed |= self.focus_switcher_window(address);
        }
        let command = self.handle.navigation_command.borrow_mut().take();
        if let Some(command) = command {
            let result = self.navigate(command, doc, path);
            self.view_changed = result.as_ref().is_ok_and(|(changed, _)| *changed);
            *self.handle.navigation_result.borrow_mut() =
                Some(result.map(|(_, invocation)| invocation));
            changed |= self.reconcile();
        }
        if let Some(action) = tilo_desktop::launchpad::LaunchpadAction::take(doc, path) {
            self.launchpad_request(action);
        }
        let updated = self.surface.update(doc, path);
        if changed && let Ok(tree) = self.handle.tree.try_borrow() {
            self.window_switcher.observe(&tree);
        }
        let notices = self.notification_layer.tick();
        changed || updated || notices
    }
    fn needs_redraw(&self) -> bool {
        // Composition consumes structural dirtiness before a queued navigation
        // request reaches the next update. Keep that deferred work scheduled.
        self.pending_view_request.is_some()
            || self.dirty
            || self.overlays.needs_redraw()
            || (self.activity.is_some() && !self.handle.history_available.get())
            || self.handle.inbox.borrow().is_some()
            || self.activity.as_ref().is_some_and(Widget::needs_redraw)
            || self.notification_layer.needs_redraw()
            || self.surface.needs_redraw()
            || self.window_switcher.needs_frame(ui::time::Instant::now())
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
        let notifications = self.notification_layer.build_layout(tree, theme, map);
        self.notification_node = Some(notifications);
        let children: Vec<NodeId> = self
            .activity_node
            .into_iter()
            .chain([surface, notifications])
            .collect();
        tree.new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &children,
        )
        .unwrap()
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.surface
            .apply_layout(tree, self.surface_node.unwrap_or(node), origin);
        if let (Some(activity), Some(node)) = (&mut self.activity, self.activity_node) {
            activity.apply_layout(tree, node, origin);
        }
        if let Some(node) = self.notification_node {
            self.notification_layer.apply_layout(tree, node, origin);
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
        // Above the desktop, below the History modal.
        self.notification_layer
            .draw(scene, transform, text, theme, presence);
        if let Some(activity) = &mut self.activity {
            activity.draw(scene, transform, text, theme, presence);
        } else {
            let tree = self.handle.tree.borrow();
            self.overlays.draw(
                scene,
                transform,
                text,
                theme,
                self.surface.get_rect(),
                (&tree, &self.resolver.host.author),
            );
        }
        if self.window_switcher.is_active() {
            self.window_switcher.draw(
                scene,
                transform,
                text,
                theme,
                self.surface.get_rect(),
                ui::time::Instant::now(),
                &mut tilo_desktop::window_switcher::NoThumbnails,
            );
        }
        self.refresh_navigation(false);
        self.capture_live();
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if self.route_window_switcher_event(event, ctx.scale_factor, ctx.modifiers) {
            *ctx.handled = true;
            return;
        }
        if let Some(activity) = &mut self.activity {
            activity.handle_event(
                event,
                &mut ctx.reborrow(ctx.path, self.activity_node.unwrap_or(ctx.node_id)),
            );
            return;
        }
        let (consumed, request) = self.overlays.handle_event(event, ctx.scale_factor, || {
            self.handle
                .tree
                .borrow()
                .active_workspace()
                .map_or(0, |workspace| workspace.collaborators.len())
        });
        if consumed {
            *ctx.handled = true;
            if request == Some(crate::desktop_overlays::OverlayRequest::OpenActivity) {
                let workspace = self.handle.tree.borrow().active;
                if let Err(error) =
                    self.queue_view_request(activity::WorkspaceViewRequest::OpenActivity {
                        workspace,
                        view: activity::ActivityView::default(),
                    })
                {
                    self.handle.errors.borrow_mut().push(error);
                }
            }
            return;
        }
        self.notification_layer.handle_event(
            event,
            &mut ctx.reborrow(ctx.path, self.notification_node.unwrap_or(ctx.node_id)),
        );
        if *ctx.handled {
            return;
        }
        self.surface.handle_event(event, ctx);
        if matches!(
            event,
            winit::event::WindowEvent::CursorMoved { .. }
                | winit::event::WindowEvent::MouseInput { .. }
                | winit::event::WindowEvent::Touch(_)
        ) {
            self.capture_live();
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(activity) = &self.activity {
            vec![activity]
        } else {
            vec![&self.surface, &self.notification_layer]
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(activity) = &mut self.activity {
            vec![activity]
        } else {
            vec![&mut self.surface, &mut self.notification_layer]
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
}

impl ui::component_lifecycle::ComponentLifecycle for DesktopRoot {
    fn close_components(&mut self) -> Vec<ComponentCloseReport> {
        self.close_activity(crate::desktop_activity::ActivityCloseReason::Disposed);
        self.reports.extend(self.surface.close_components());
        std::mem::take(&mut self.reports)
    }
    fn take_component_close_reports(&mut self) -> Vec<ComponentCloseReport> {
        self.surface.take_component_close_reports()
    }
}

impl ui::resources::PublishesResources for DesktopRoot {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.resources_for_close(doc, None)
    }
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
}

#[cfg(test)]
mod tests;

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
            .find(|r| r.app == "so.tilo.word-processor")
            .unwrap()
            .clone();
        let body: String = std::iter::repeat_n(
            "The quick brown fox jumps over the lazy dog. ",
            4096 / 45 + 1,
        )
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
