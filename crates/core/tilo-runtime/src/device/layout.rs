//! The layouts of the workspaces this device keeps, and the actions that read them and open and
//! close windows on them ([`LayoutActions`]): the one way a desktop page's shell, an agent's
//! "show this" and any other caller change a workspace's layout.
//!
//! A layout is read as the device's own read of the workspace's document, and a window opened or
//! closed on it is written as the device's own write: signed as the device and committed through
//! the workspace service's one commit path, so it is kept and carried to the workspace's other
//! devices as any of its writes is ([`tilo_workspace_service::device_documents`]). Whose window it
//! is, is the caller's: the person who asked, or the person an agent acts for.
//!
//! An agent's "show this" (its app's `open` on the application, or on a record of it) puts the
//! view on its person's layout through the same place, where every device of theirs shows it, and
//! starts nothing.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, ActionCallError, ActionProvider, Caller, CataloguePage,
    CatalogueQuery, InvocationLog, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, Reservation, application_provider,
    application_provider_resource,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate, ResourceSpec};
use tilo_desktop_model::WorkspaceId;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_desktop_model::reading::{LayoutPlaceReading, LayoutReading, Naming, SHELL};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::{
    DeviceDocuments as _, DeviceWrites as _, DocumentRead, DocumentWriting,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};

use super::workspaces::{ServiceWorkspaces, block_on_owned};

/// The provider the layout's actions are published under.
pub const PROVIDER: &str = "so.tilo.layout";

/// The resource they are published on: the layout of the workspace the call is made in.
pub const RESOURCE: &str = "workspace:layout";

/// The layout of a workspace, as its actions publish it.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.layout", label = "Layout")]
struct LayoutRow;

/// What a caller does with the layout of the workspace they call in.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.layout")]
pub enum LayoutAction {
    /// Read the layout: the workspaces, strips and windows on it, the app and record each window
    /// shows, and the other workspaces the caller reaches here.
    #[effect(returns, kind = "read")]
    #[effect(icon = "layout-grid", keywords = "windows, desktop")]
    Read,
    /// Open a window of an app on the layout, as the caller's: beside the window in front, on the
    /// record it names or the app's own. A record a window shows already stays where it is.
    #[effect(kind = "outside")]
    #[effect(icon = "app-window", keywords = "open window, show")]
    Place {
        #[param("The app the window shows, by its id")]
        app: String,
        #[param("The record the window shows; the app's own when empty")]
        record: Option<String>,
    },
    /// Close a window on the layout, everywhere it is shown. Its record stays in the workspace.
    #[effect(kind = "outside")]
    #[effect(icon = "x", keywords = "close window")]
    Close {
        #[param("The window, by its key on the layout")]
        window: String,
    },
    /// Stand a window at a point on its workspace's canvas, as a drag that let it go there
    /// leaves it: one arranging step for the whole gesture.
    #[effect(kind = "undoable")]
    #[effect(icon = "app-window", keywords = "move window, drag")]
    Position {
        #[param("The window, by its key on the layout")]
        window: String,
        #[param("Its canvas x")]
        x: i64,
        #[param("Its canvas y")]
        y: i64,
    },
    /// Give a window a size on its workspace's canvas, as dragging its edge does.
    #[effect(kind = "undoable")]
    #[effect(icon = "app-window", keywords = "resize window")]
    Size {
        #[param("The window, by its key on the layout")]
        window: String,
        #[param("Its width in canvas pixels")]
        width: i64,
        #[param("Its height in canvas pixels")]
        height: i64,
    },
    /// Bring a window in front of its strip, and its workspace forward, as a press on its bar
    /// does.
    #[effect(kind = "undoable")]
    #[effect(icon = "app-window", keywords = "focus window")]
    Activate {
        #[param("The window, by its key on the layout")]
        window: String,
    },
}

/// The workspace's layout as this device has it.
enum LayoutHere {
    /// The layout document.
    Here(Box<automerge::AutoCommit>),
    /// This device keeps the workspace and has no layout: none was written yet.
    Unwritten,
    /// This device does not have it (yet): another device keeps it.
    Elsewhere,
}

/// The layouts of the workspaces `workspaces` keeps, read and written as this device.
pub(crate) struct DeviceLayouts {
    workspaces: Arc<ServiceWorkspaces>,
}

impl DeviceLayouts {
    pub(crate) const fn new(workspaces: Arc<ServiceWorkspaces>) -> Self {
        Self { workspaces }
    }

    /// The person this device is.
    fn device_person(&self) -> String {
        hex::encode(self.workspaces.device_cert().user_public_key)
    }

    /// The person `caller` is, as a layout names whose a window is: the person who called, or the
    /// person an agent acts for. A caller acting for nobody acts as this device's person.
    fn person_of(&self, caller: &Caller) -> String {
        let acting = caller.acting();
        acting
            .person_id()
            .or_else(|| acting.acted_for_person())
            .map_or_else(|| self.device_person(), ToOwned::to_owned)
    }

    /// `workspace`'s shared layout as this device has it now.
    fn layout(&self, workspace: &str) -> LayoutHere {
        let copies = self.workspaces.copies().clone();
        let read = ServiceDocuments::of(copies.clone()).read(
            workspace,
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            None,
        );
        match read {
            Ok(DocumentRead::Whole { bytes, .. }) => automerge::AutoCommit::load(&bytes)
                .map_or(LayoutHere::Elsewhere, |layout| {
                    LayoutHere::Here(Box::new(layout))
                }),
            // A workspace this device keeps has every layout written in it: none here is none.
            Ok(DocumentRead::Absent) => {
                let Some(id) = WorkspaceId::parse(workspace) else {
                    return LayoutHere::Elsewhere;
                };
                if block_on_owned(async move { copies.hosted_copy(id).await.is_some() }) {
                    LayoutHere::Unwritten
                } else {
                    LayoutHere::Elsewhere
                }
            }
            _ => LayoutHere::Elsewhere,
        }
    }

    /// Write `changes` into `workspace`'s layout as this device: the whole document when
    /// `founding`, a change of the one here otherwise.
    fn write(&self, workspace: &str, changes: &[u8], founding: bool) -> Result<(), String> {
        let written = ServiceWrites::of(self.workspaces.copies().clone()).write(
            workspace,
            changes,
            DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding,
                snapshot: founding,
                sealed: None,
            },
        );
        block_on_owned(written)
            .map(drop)
            .map_err(|refusal| format!("the layout was not written: {refusal}"))
    }

    /// What `workspace` is called here, when it has a name.
    fn name(&self, workspace: &str) -> Option<String> {
        WorkspaceId::parse(workspace)
            .and_then(|id| self.workspaces.workspace(id))
            .map(|kept| kept.name)
            .filter(|name| !name.trim().is_empty())
    }

    /// Put a window of `app` (`(id, title)`) showing `document` (empty: the app's primary record)
    /// on `workspace`'s layout, as `person`'s, opened by the agent session `opened_by` names. A
    /// record a window shows already is left where the people put it. A workspace this device
    /// keeps with no layout yet gets one; where another device keeps the layout, the view is asked
    /// for in its document ([`tilo_desktop_model::asked`]) and that device places it.
    ///
    /// # Errors
    /// The layout has no place for the window, or the write was refused.
    pub(crate) fn place(
        &self,
        workspace: &str,
        person: &str,
        (app, title, document): (&str, &str, &str),
        opened_by: Option<tilo_desktop_model::ownership::OpenedBy>,
    ) -> Result<(), String> {
        let layout = match self.layout(workspace) {
            LayoutHere::Here(layout) => *layout,
            LayoutHere::Unwritten => {
                let name = self
                    .name(workspace)
                    .unwrap_or_else(|| "Workspace".to_owned());
                let founded = founded_layout(workspace, &name);
                self.write(workspace, &founded.clone().save(), true)?;
                founded
            }
            LayoutHere::Elsewhere => {
                let view = tilo_desktop_model::asked::AskedView {
                    app_id: app.to_owned(),
                    title: title.to_owned(),
                    document: document.to_owned(),
                    person: person.to_owned(),
                    opened_by,
                };
                let mut ask = automerge::AutoCommit::new();
                tilo_desktop_model::asked::ask(
                    &mut ask,
                    &format!("{person}:{app}:{document}"),
                    &view,
                )
                .map_err(|error| error.to_string())?;
                ask.commit();
                return self.write(workspace, &ask.save(), true);
            }
        };
        let view = tilo_desktop_model::place::View {
            app_id: app,
            title,
            document,
            beside: None,
        };
        let opener = tilo_desktop_model::place::Opener { person, opened_by };
        let change = tilo_desktop_model::place::place_view(&layout, &opener, &view, now())
            .map_err(|why| format!("{title}'s window is not on the person's layout: {why}"))?;
        match change {
            Some(change) => self.write(workspace, &change, false),
            None => Ok(()),
        }
    }

    /// Close the window `tab` on `workspace`'s layout as `person` closes it, everywhere the layout
    /// is shown. A window closed already, or a layout this device does not have, is left as it is.
    ///
    /// # Errors
    /// The layout without the window did not write.
    pub(crate) fn close(&self, workspace: &str, person: &str, tab: u64) -> Result<(), String> {
        let LayoutHere::Here(layout) = self.layout(workspace) else {
            return Ok(());
        };
        match tilo_desktop_model::place::close_window(&layout, person, tab, now())
            .map_err(|why| why.to_string())?
        {
            Some(change) => self.write(workspace, &change, false),
            None => Ok(()),
        }
    }

    /// Stand the window `tab` at `(x, y)` on `workspace`'s canvas as `person` drags it there,
    /// everywhere the layout is shown. A window already there, or a layout this device does not
    /// have, is left as it is.
    ///
    /// # Errors
    /// The layout with the window moved did not write.
    pub(crate) fn position(
        &self,
        workspace: &str,
        person: &str,
        tab: u64,
        x: i64,
        y: i64,
    ) -> Result<(), String> {
        let LayoutHere::Here(layout) = self.layout(workspace) else {
            return Ok(());
        };
        let x = i32::try_from(x).unwrap_or(if x < 0 { i32::MIN } else { i32::MAX });
        let y = i32::try_from(y).unwrap_or(if y < 0 { i32::MIN } else { i32::MAX });
        match tilo_desktop_model::place::move_window(&layout, person, tab, x, y, now())
            .map_err(|why| why.to_string())?
        {
            Some(change) => self.write(workspace, &change, false),
            None => Ok(()),
        }
    }

    /// Give the window `tab` the canvas size `width` × `height` as `person` drags its edge,
    /// everywhere the layout is shown. A window already that size, or a layout this device does
    /// not have, is left as it is.
    ///
    /// # Errors
    /// The layout with the window resized did not write.
    pub(crate) fn size(
        &self,
        workspace: &str,
        person: &str,
        tab: u64,
        width: i64,
        height: i64,
    ) -> Result<(), String> {
        let LayoutHere::Here(layout) = self.layout(workspace) else {
            return Ok(());
        };
        let side = |value: i64| u32::try_from(value.max(1)).unwrap_or(u32::MAX);
        match tilo_desktop_model::place::size_window(
            &layout,
            person,
            tab,
            side(width),
            side(height),
            now(),
        )
        .map_err(|why| why.to_string())?
        {
            Some(change) => self.write(workspace, &change, false),
            None => Ok(()),
        }
    }

    /// Bring the window `tab` in front of its strip and its workspace forward, as `person`
    /// presses its bar. A window in front already, or a layout this device does not have, is
    /// left as it is.
    ///
    /// # Errors
    /// The layout with the window in front did not write.
    pub(crate) fn activate(&self, workspace: &str, person: &str, tab: u64) -> Result<(), String> {
        let LayoutHere::Here(layout) = self.layout(workspace) else {
            return Ok(());
        };
        match tilo_desktop_model::place::activate_window(&layout, person, tab, now())
            .map_err(|why| why.to_string())?
        {
            Some(change) => self.write(workspace, &change, false),
            None => Ok(()),
        }
    }

    /// `workspace`'s layout as `caller` reads it: the layout, each window by the app and record it
    /// shows (`windows`), and the other workspaces they reach here (`workspaces`, each with its
    /// key and name). `title` names an app by its id. A layout this device does not have reads as
    /// an empty one.
    pub(crate) fn read(
        &self,
        workspace: &str,
        caller: &Caller,
        title: &dyn Fn(&str) -> String,
    ) -> Value {
        let desktop = match self.layout(workspace) {
            LayoutHere::Here(layout) => tilo_desktop_model::shared_resource::read(&layout, None),
            LayoutHere::Unwritten | LayoutHere::Elsewhere => None,
        };
        let mut windows = Vec::new();
        let reading = desktop
            .as_ref()
            .map_or_else(LayoutReading::default, |desktop| {
                tilo_desktop_model::reading::of(
                    &desktop.tree,
                    String::new(),
                    &Naming {
                        workspace: &|name: &str| name.trim().to_owned(),
                        row: &|name: &str, at: usize| {
                            let name = name.trim();
                            if name.is_empty() {
                                format!("Strip {}", at.saturating_add(1))
                            } else {
                                name.to_owned()
                            }
                        },
                    },
                    |tab, group| {
                        let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .map(String::from)
                            .unwrap_or_default();
                        let app_title = if app.is_empty() {
                            String::new()
                        } else {
                            title(&app)
                        };
                        let shown = tab.title.trim().to_owned();
                        let name = match (shown.is_empty(), app_title.is_empty()) {
                            (false, false) if shown != app_title => {
                                format!("{shown} — {app_title}")
                            }
                            (false, _) => shown.clone(),
                            (true, _) => app_title.clone(),
                        };
                        let record = desktop
                            .records
                            .iter()
                            .find(|(on, _)| *on == tab.id)
                            .map(|(_, record)| record.clone())
                            .unwrap_or_default();
                        windows.push(json!({
                            "key": tab.id.to_string(),
                            "app": app,
                            "app_title": app_title,
                            "record": record,
                        }));
                        LayoutPlaceReading {
                            key: tab.id.to_string(),
                            strip: group.to_string(),
                            shows: app,
                            shown_as: tab.view.clone(),
                            // What the window shows is its app's to draw, where its app runs.
                            resource: String::new(),
                            title: shown,
                            name,
                            pinned: tab.pinned,
                            leaving: false,
                            shell: SHELL.to_owned(),
                        }
                    },
                )
            });
        json!({
            "layout": reading,
            "windows": windows,
            "workspaces": self.reached(workspace, caller),
        })
    }

    /// The workspaces besides `workspace` that `caller` reaches here, each as `{key, name}`: every
    /// workspace kept here is this device's person's, and another member reaches only the one
    /// they called in.
    fn reached(&self, workspace: &str, caller: &Caller) -> Vec<Value> {
        if self.person_of(caller) != self.device_person() {
            return Vec::new();
        }
        let here = WorkspaceId::parse(workspace);
        self.workspaces
            .workspaces()
            .into_iter()
            .filter(|kept| Some(kept.id()) != here)
            .map(|kept| json!({ "key": kept.id().to_string(), "name": kept.name }))
            .collect()
    }
}

/// The time a change of the layout is made: seconds since the epoch, as its history entry says.
fn now() -> i64 {
    i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX)
}

/// The layout a workspace's first window is placed on: the workspace by its own id and one strip,
/// in the shape a shared layout is kept in ([`tilo_desktop_model::desktop_doc`]).
fn founded_layout(workspace: &str, name: &str) -> automerge::AutoCommit {
    use tilo_desktop_model::{Layout, Strip, StripId, Workspace};
    let id = WorkspaceId::parse(workspace).unwrap_or_else(|| WorkspaceId::from_u128(1));
    let mut shown = Workspace::new(id, name);
    shown.strips = vec![Strip::new(StripId(1), "Main")];
    let tree = Layout {
        active: id,
        workspaces: vec![shown],
        ..Layout::default()
    };
    let mut doc = automerge::AutoCommit::new();
    tilo_desktop_model::desktop_doc::write_shared(
        &mut doc,
        &tree,
        &[],
        &[],
        &std::collections::BTreeMap::default(),
    );
    doc.commit();
    doc
}

/// How an app is named by its id: what a window of it is titled.
pub(crate) type Titles = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// The layout's actions on this device's dispatch ([`LayoutAction`]), run as each caller.
pub(crate) struct LayoutActions {
    layouts: Arc<DeviceLayouts>,
    titles: Titles,
    resources: Vec<ResourceSpec>,
    log: Mutex<InvocationLog<Arc<Mutex<InvocationState>>>>,
}

impl LayoutActions {
    /// The actions on the layouts of the workspaces `workspaces` keeps, naming apps by `titles`.
    pub(crate) fn new(workspaces: Arc<ServiceWorkspaces>, titles: Titles) -> Self {
        let mut resource = tilo_resource::offer::<LayoutAction>(tilo_resource::typed_resource::<
            LayoutRow,
        >(RESOURCE, "Layout"));
        resource.app_id = Some(PROVIDER.to_owned());
        Self {
            layouts: Arc::new(DeviceLayouts::new(workspaces)),
            titles,
            resources: vec![resource],
            log: Mutex::default(),
        }
    }

    /// What `request` asks, checked against the actions' parameters.
    fn admit(&self, request: &InvokeRequest) -> Result<LayoutAction, ActionCallError> {
        let address = &request.target.address;
        if request.target.provider != PROVIDER || address.resource != RESOURCE {
            return Err(ActionCallError::UnknownAction);
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None | InvokeArgument::Text(_) => Map::new(),
        };
        tilo_resource::decode::<LayoutAction>(
            &self.resources,
            RESOURCE,
            &address.action,
            &arguments,
        )
        .map_err(|error| ActionCallError::from_api(&error))
    }
}

/// Run `action` on `workspace`'s layout as `caller`: what it answers, or why it did not run.
fn run(
    layouts: &DeviceLayouts,
    titles: &Titles,
    (workspace, caller): (&str, &Caller),
    action: LayoutAction,
) -> Result<Option<Value>, String> {
    let person = layouts.person_of(caller);
    match action {
        LayoutAction::Read => Ok(Some(layouts.read(workspace, caller, titles.as_ref()))),
        LayoutAction::Place { app, record } => {
            let title = titles(&app);
            let record = record.unwrap_or_default();
            layouts
                .place(workspace, &person, (&app, &title, &record), None)
                .map(|()| None)
        }
        LayoutAction::Close { window } => {
            let tab = window
                .parse()
                .map_err(|_| format!("{window} is no window on the layout"))?;
            layouts.close(workspace, &person, tab).map(|()| None)
        }
        LayoutAction::Position { window, x, y } => {
            let tab = window
                .parse()
                .map_err(|_| format!("{window} is no window on the layout"))?;
            layouts
                .position(workspace, &person, tab, x, y)
                .map(|()| None)
        }
        LayoutAction::Size {
            window,
            width,
            height,
        } => {
            let tab = window
                .parse()
                .map_err(|_| format!("{window} is no window on the layout"))?;
            layouts
                .size(workspace, &person, tab, width, height)
                .map(|()| None)
        }
        LayoutAction::Activate { window } => {
            let tab = window
                .parse()
                .map_err(|_| format!("{window} is no window on the layout"))?;
            layouts.activate(workspace, &person, tab).map(|()| None)
        }
    }
}

impl ActionProvider for LayoutActions {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.resources.clone()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::published(
            PROVIDER,
            &self.resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let action = match self.admit(&request) {
            Ok(action) => action,
            Err(error) => return Pending::ready(Err(error)),
        };
        let Some(workspace) = caller.workspace().map(ToOwned::to_owned) else {
            return Pending::ready(Err(ActionCallError::InvalidRequest {
                reason: "a layout is a workspace's: call it in one".to_owned(),
            }));
        };
        let id = request.invocation;
        let reservation = self.log.lock_recover().reserve(caller, &request);
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                let state = self.log.lock_recover().get(caller, &id).cloned();
                return Pending::ready(
                    state
                        .map(|state| state.lock_recover().receipt())
                        .ok_or(ActionCallError::UnknownInvocation),
                );
            }
            Ok(Reservation::Reserved) => {}
        }
        let state = Arc::new(Mutex::new(InvocationState::new(
            id,
            InvocationUpdate::Accepted { invocation: id },
        )));
        self.log.lock_recover().complete(&id, Arc::clone(&state));
        let (layouts, titles, caller) = (
            Arc::clone(&self.layouts),
            Arc::clone(&self.titles),
            caller.clone(),
        );
        let (done, answer) = tilo_actions::remote::pending();
        // The layout is read and written as the device, which waits on the workspace service:
        // never on the caller's thread.
        std::thread::spawn(move || {
            let update = match run(&layouts, &titles, (&workspace, &caller), action) {
                Ok(value) => InvocationUpdate::Succeeded {
                    invocation: id,
                    output: ActionOutput {
                        value,
                        resources: Vec::new(),
                    },
                },
                Err(reason) => InvocationUpdate::Failed {
                    invocation: id,
                    error: tilo_actions::ActionFailure {
                        code: "layout_unwritten".to_owned(),
                        message: reason,
                        details: None,
                        retryable: true,
                    },
                },
            };
            let receipt = {
                let mut state = state.lock_recover();
                *state = InvocationState::new(id, update);
                state.receipt()
            };
            done.complete(Ok(receipt));
        });
        answer
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "a layout change ends as it is asked".to_owned(),
        }))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log
            .lock_recover()
            .get(caller, id)
            .map(|state| state.lock_recover().clone())
    }
}

/// The view an agent's `request` asks to be shown, when it is its app's `open` in `workspace`:
/// placed on the layout of the person the agent acts for, as the layout's `place` places it,
/// named as the agent session that opened it, answered once it is written. `None` for any other
/// request. `title` names an app by its id.
pub(super) fn show_on_layout(
    workspaces: &Arc<ServiceWorkspaces>,
    workspace: &str,
    (caller, request): (&Caller, &InvokeRequest),
    title: impl FnOnce(&str) -> String,
) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
    if request.target.address.action != APPLICATION_OPEN_ACTION {
        return None;
    }
    let (named, app) = application_provider(&request.target.provider)?;
    if named != workspace {
        return None;
    }
    let document = application_provider_resource(&request.target.provider)
        .unwrap_or_default()
        .to_owned();
    let (app, title) = (app.to_owned(), title(app));
    let opened_by = opened_by(caller, request);
    let layouts = DeviceLayouts::new(Arc::clone(workspaces));
    let person = layouts.person_of(caller);
    let (workspace, invocation) = (workspace.to_owned(), request.invocation);
    let (done, answer) = tilo_actions::remote::pending();
    // The layout is read and written as the device, which waits on the workspace service: never
    // on the caller's thread.
    std::thread::spawn(move || {
        let placed = layouts.place(&workspace, &person, (&app, &title, &document), opened_by);
        done.complete(Ok(InvocationReceipt {
            invocation,
            status: match placed {
                Ok(()) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(reason) => ReceiptStatus::Rejected {
                    error: ActionCallError::Unavailable { reason },
                },
            },
        }));
    });
    Some(answer)
}

/// The agent session that opens a window for `request`: what the window's mark says beside it.
fn opened_by(
    caller: &Caller,
    request: &InvokeRequest,
) -> Option<tilo_desktop_model::ownership::OpenedBy> {
    let session = caller.agent_session()?;
    // What the agent called the step that opened it, in its own words: never its session's
    // name, which the mark says already.
    let title = request
        .context
        .as_ref()
        .and_then(|context| context.group.as_ref())
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
        .and_then(|group| group.label.as_deref())
        .and_then(|label| {
            tilo_actions::context::one_line(label, tilo_actions::context::MAX_LABEL_CHARS)
        })
        .filter(|title| request.said_session_name().as_deref() != Some(title.as_str()))
        .unwrap_or_default();
    Some(tilo_desktop_model::ownership::OpenedBy {
        run: session.id,
        agent: caller
            .acting()
            .member_id()
            .filter(|member| member.is_agent())?
            .clone(),
        label: caller
            .clone()
            .in_session_of(request)
            .agent_called()
            .unwrap_or_default()
            .to_owned(),
        title,
    })
}
