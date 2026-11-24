//! Nearby Send as the shell publishes it: the devices this one can hand a file to, the offers
//! another device made back, and the actions that answer them.
//!
//! The offers and transfers are the device daemon's ([`tilo_runtime::device::nearby`]): it holds
//! them, carries the content and answers another of the person's devices over the action service
//! it already serves. This session keeps what is the seat's alone: the share sheet, the files the
//! person hands over — which it stages on the device's one content shelf, where its Files grants
//! let it read them — and the receiver no device of theirs is. It reads the daemon's resource over
//! the shell link and invokes its actions there, so the resource's own shape and its actions live
//! in one place ([`tilo_workspace_service::apps::nearby_send`]) and a call is checked by the same
//! code whether Files, the panel or a script made it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use collab_types::caps::{Access, CapDecision, ResourceRef, Scope};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, ApiError, InvocationId, ResourceSpec};
use tilo_broker_core::services::WorkspaceAuthority;
use tilo_capture_studio::{Refusal, Sent};
use tilo_resource::Effect as _;
use tilo_service::{Service, Started, Wake, reading};
use tilo_session_client::daemon_documents::{DaemonShell, whole};
use tilo_workspace_service::apps::nearby_send::{
    AirDrops, HELPER_TIMEOUT, Host, Invoked, NearbySendAction, Reading, TransferState,
};
use ui::widget::ScreenshotSendDevice;

use crate::platform::fabric::Continuity;
use crate::record_content::RecordContent;

use tilo_runtime::device::nearby as wire;

/// The session's Nearby Send resource: what the daemon shows, and the actions that change it.
#[derive(Clone)]
pub(crate) struct Served {
    inner: Rc<Inner>,
}

/// What one session's Nearby Send holds, shared by the service the shell calls and the studio's
/// sender.
struct Inner {
    daemon: DaemonShell,
    /// Where a file the person hands over is staged: the device's one content shelf, through the
    /// scope its Files grants let this session reach.
    content: Arc<RecordContent>,
    /// The workspaces' files as this session knows them: what Files grants a read or a write,
    /// attached once the app wire decides against the shell.
    layout: Arc<Mutex<Option<Arc<dyn WorkspaceAuthority>>>>,
    /// How a resource is shared: the session opens its invitation sheet.
    share: Box<dyn Fn(&str) -> Result<(), String>>,
    /// The receiver no device of the person's is, and the question its certificate puts to them.
    air_drop: Arc<AirDrops>,
    /// Whether the person lets files move to their other devices.
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    /// What wakes the session when a send moved.
    wake: Wake,
    /// What the last pass reported, so a pass that changed nothing is no revision.
    published: RefCell<Option<Value>>,
    /// The last reading, as the studio's picker and a send read the devices from.
    last: RefCell<Reading>,
}

impl Served {
    /// The session's Nearby Send, reading and invoking the daemon's over `daemon`.
    pub(crate) fn new(
        daemon: DaemonShell,
        content: Arc<RecordContent>,
        continuity: Continuity,
        share: Box<dyn Fn(&str) -> Result<(), String>>,
        wake: Wake,
    ) -> Self {
        Self {
            inner: Rc::new(Inner::new(daemon, content, continuity, share, wake)),
        }
    }

    /// Decide the grants against `authority`, this session's workspaces as its shell shows them.
    pub(crate) fn attach_authority(&self, authority: Arc<dyn WorkspaceAuthority>) {
        self.inner.attach_authority(authority);
    }

    /// The receiver no device of the person's is, for its question to reach their sheets
    /// ([`AirDrops::ask_with`]).
    pub(crate) fn air_drop(&self) -> &Arc<AirDrops> {
        self.inner.air_drop()
    }

    /// Whether the daemon this session reads Nearby Send from is linked.
    pub(crate) fn has_link(&self) -> bool {
        self.inner.has_link()
    }

    /// The person's other devices that are here now, as the capture studio's picker lists them.
    pub(crate) fn devices(&self) -> Vec<ScreenshotSendDevice> {
        self.inner.devices()
    }

    /// Send the file at `path` to the device `id` for the capture studio, answering `reply` once
    /// it is on its way or refused. Returns the refusal it can make here.
    pub(crate) fn studio_send(
        &self,
        id: &str,
        label: String,
        path: PathBuf,
        reply: Box<dyn FnOnce(Sent) + Send>,
    ) -> Option<Sent> {
        self.inner.studio_send(id, label, path, reply)
    }
}

impl Inner {
    /// The session's Nearby Send, reading and invoking the daemon's over `daemon`.
    fn new(
        daemon: DaemonShell,
        content: Arc<RecordContent>,
        continuity: Continuity,
        share: Box<dyn Fn(&str) -> Result<(), String>>,
        wake: Wake,
    ) -> Self {
        let layout: Arc<Mutex<Option<Arc<dyn WorkspaceAuthority>>>> = Arc::default();
        let air_drop = air_drops(&content, &layout, &continuity, &wake);
        let permitting = continuity.clone();
        Self {
            daemon,
            content,
            layout,
            share,
            air_drop,
            permitted: Arc::new(move || permitting.file_transfer_permitted()),
            wake,
            published: RefCell::new(None),
            last: RefCell::new(Reading::default()),
        }
    }

    /// Decide the grants against `authority`, this session's workspaces as its shell shows them.
    fn attach_authority(&self, authority: Arc<dyn WorkspaceAuthority>) {
        *self
            .layout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(authority);
    }

    /// The workspace in front, as the shell shows it: where a send is made and a received file
    /// lands.
    fn workspace(&self) -> Option<String> {
        let layout = self.workspace_grant()?;
        let mut active = String::new();
        layout
            .with_tree(&mut |tree| active = tree.active.to_string())
            .ok()?;
        (!active.is_empty()).then_some(active)
    }

    /// The workspaces' grants, once they are attached.
    fn workspace_grant(&self) -> Option<Arc<dyn WorkspaceAuthority>> {
        self.layout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The receiver no device of the person's is, for its question to reach their sheets
    /// ([`AirDrops::ask_with`]).
    fn air_drop(&self) -> &Arc<AirDrops> {
        &self.air_drop
    }

    /// Whether the daemon this session reads Nearby Send from is linked.
    fn has_link(&self) -> bool {
        self.daemon.link().is_ok()
    }

    /// The person's other devices that are here now, as the capture studio's picker lists them.
    fn devices(&self) -> Vec<ScreenshotSendDevice> {
        let mut devices: Vec<ScreenshotSendDevice> = self
            .last
            .borrow()
            .devices
            .iter()
            .filter(|device| device.reachable)
            .map(|device| ScreenshotSendDevice {
                id: device.handle.clone(),
                label: device.name.clone(),
            })
            .collect();
        devices.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
        devices
    }

    /// What the daemon shows now, with the receivers no device of the person's is added: an
    /// AirDrop attempt is one of this session's sends.
    fn now(&self) -> Reading {
        let Some(workspace) = self.workspace() else {
            *self.last.borrow_mut() = Reading::default();
            return Reading::default();
        };
        let mut reading = match self.read_daemon(&workspace) {
            Ok(reading) => reading,
            Err(error) => {
                tracing::warn!(%error, "Nearby Send could not be read");
                Reading::default()
            }
        };
        for attempt in self.air_drop.attempts() {
            let state = match attempt.phase {
                runtime_collab::NearbyTransferPhase::Completed => TransferState::Done,
                runtime_collab::NearbyTransferPhase::Cancelled => TransferState::Declined,
                runtime_collab::NearbyTransferPhase::Failed => TransferState::Failed,
                _ => TransferState::Sending,
            };
            reading.transfers.push(
                tilo_workspace_service::apps::nearby_send::NearbyTransferRow {
                    state,
                    progress: if state == TransferState::Done { 100 } else { 0 },
                    peer: attempt.endpoint.clone(),
                    destination: String::new(),
                    time: 0,
                    incoming: false,
                    name: attempt.name.clone(),
                    handle: hex(&attempt.id),
                },
            );
        }
        *self.last.borrow_mut() = reading.clone();
        reading
    }

    /// The daemon's reading of Nearby Send in `workspace`, as the shell link carries it.
    fn read_daemon(&self, workspace: &str) -> Result<Reading, String> {
        let read = whole(workspace, "session", wire::RESOURCE);
        let answered = self.daemon.read_blocking(read)?;
        let value = answered["records"]
            .as_array()
            .and_then(|records| records.first())
            .map_or(Value::Null, |record| record["value"].clone());
        if value.is_null() {
            return Ok(Reading::default());
        }
        serde_json::from_value(value).map_err(|error| format!("the reading does not read: {error}"))
    }

    /// Ask the daemon for one of the resource's actions, and answer what it said: a call that
    /// waits for the person's yes is [`Started::Accepted`].
    fn forward(&self, action: &str, arguments: Value, waits: bool) -> Result<Started, ApiError> {
        let request = request(action, arguments);
        let Some(workspace) = self.workspace() else {
            return Err(refused("this session shows no workspace".to_owned()));
        };
        if waits {
            let daemon = self.daemon.clone();
            let wake = self.wake.clone();
            std::thread::spawn(move || {
                if let Err(error) = daemon.invoke_receipt_blocking(Some(&workspace), request) {
                    tracing::warn!(%error, "a Nearby Send action was not taken");
                }
                wake();
            });
            return Ok(Started::Accepted);
        }
        let receipt = self
            .daemon
            .invoke_receipt_blocking(Some(&workspace), request)
            .map_err(refused)?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(Started::Done(output)),
            ReceiptStatus::Accepted => Ok(Started::Accepted),
            ReceiptStatus::Rejected { error } => Err(refused(error.to_string())),
            ReceiptStatus::Failed { error } => Err(refused(error.message)),
            ReceiptStatus::Cancelled { message } => Err(refused(
                message.unwrap_or_else(|| "the transfer was stopped".to_owned()),
            )),
        }
    }

    /// Stage the files `items` name on the device's shelf, under their Files grants, and offer
    /// them to `device`: on a thread of its own, since reading a file is not the pass's.
    fn send(&self, items: &[String], device: &str) -> Result<Started, ApiError> {
        let Some(workspace) = self.workspace() else {
            return Err(refused("this session shows no workspace".to_owned()));
        };
        let daemon = self.daemon.clone();
        let content = Arc::clone(&self.content);
        let layout = Arc::clone(&self.layout);
        let wake = self.wake.clone();
        let items = items.to_vec();
        let device = device.to_owned();
        std::thread::spawn(move || {
            match stage(&content, &layout, &workspace, &items) {
                Err(reason) => tracing::warn!(%reason, "a nearby send is refused"),
                Ok(staged) => {
                    let arguments = json!({"items": staged, "device": device});
                    let request = request(wire::SEND, arguments);
                    match daemon.invoke_receipt_blocking(Some(&workspace), request) {
                        Ok(receipt) if !receipt.status.is_terminal() => {}
                        Ok(_) => {}
                        Err(reason) => tracing::warn!(%reason, "a nearby offer is refused"),
                    }
                }
            }
            wake();
        });
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    /// Send the file at `path` to the device `id` for the capture studio, answering `reply` once
    /// it is on its way or refused. Returns the refusal it can make here.
    fn studio_send(
        &self,
        id: &str,
        label: String,
        path: PathBuf,
        reply: Box<dyn FnOnce(Sent) + Send>,
    ) -> Option<Sent> {
        if !(self.permitted)() {
            return Some(Sent::refused(label, Refusal::SyncOff));
        }
        if !self
            .last
            .borrow()
            .devices
            .iter()
            .any(|device| device.handle == id)
        {
            return Some(Sent::refused(label, Refusal::Offline));
        }
        let Some(workspace) = self.workspace() else {
            return Some(Sent::refused(label, Refusal::NoWorkspace));
        };
        let daemon = self.daemon.clone();
        let content = Arc::clone(&self.content);
        let layout = Arc::clone(&self.layout);
        let wake = self.wake.clone();
        let id = id.to_owned();
        std::thread::spawn(move || {
            let outcome = (|| {
                let name = path
                    .file_name()
                    .ok_or("the file has no name")?
                    .to_string_lossy()
                    .into_owned();
                let bytes = read_for(&content, &layout, &workspace, &path)?;
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                let hash = content.keep_under(&workspace, wire::APP, &bytes)?;
                let arguments = json!({
                    "items": [{"hash": hash, "name": name, "size": size}],
                    "device": id,
                });
                daemon
                    .invoke_receipt_blocking(Some(&workspace), request(wire::SEND, arguments))
                    .map_err(|error| error.to_string())?;
                Ok::<(), String>(())
            })();
            reply(match outcome {
                Ok(()) => Sent::offered(label),
                Err(why) => Sent::refused(label, Refusal::Refused(why)),
            });
            wake();
        });
        None
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![tilo_workspace_service::apps::nearby_send::resource(
            crate::i18n::t("session-nearby-send"),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == wire::RESOURCE).then(|| reading(&self.inner.now()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        self.inner.invoke(resource, action, arguments)
    }

    fn poll(&mut self) -> bool {
        self.inner.poll()
    }
}

impl Inner {
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != wire::RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        let decoded = NearbySendAction::decode(action, arguments)
            .map_err(|error| ApiError::InvalidArguments(format!("{action}: {error}")))?;
        match decoded {
            NearbySendAction::Send { items, device } => self.send(&items, &device),
            NearbySendAction::Accept {
                transfer,
                destination,
            } => self.forward(
                wire::ACCEPT,
                json!({"transfer": transfer, "destination": destination}),
                true,
            ),
            NearbySendAction::Decline { transfer } => {
                self.forward(wire::DECLINE, json!({"transfer": transfer}), false)
            }
            NearbySendAction::Cancel { transfer } => {
                self.forward(wire::CANCEL, json!({"transfer": transfer}), false)
            }
            NearbySendAction::RetryTransfer { transfer } => {
                self.forward(wire::RETRY, json!({"transfer": transfer}), false)
            }
            NearbySendAction::InvitePeople { resource } => {
                (self.share)(&resource).map_err(refused)?;
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
        }
    }

    fn poll(&self) -> bool {
        let now = reading(&self.now());
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }
}

/// The daemon's `action` of Nearby Send with `arguments`, as the person's own call.
fn request(action: &str, arguments: Value) -> InvokeRequest {
    let argument = match arguments {
        Value::Object(fields) => InvokeArgument::Json(fields),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: wire::PROVIDER.to_owned(),
            address: ActionAddress::new(wire::RESOURCE, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Stage each of `items` on the device's shelf under `workspace`'s Files grants, as the offer's
/// content: its address, name and size.
fn stage(
    content: &RecordContent,
    layout: &Arc<Mutex<Option<Arc<dyn WorkspaceAuthority>>>>,
    workspace: &str,
    items: &[String],
) -> Result<Vec<Value>, String> {
    let mut staged = Vec::with_capacity(items.len());
    for item in items {
        let path = Path::new(item);
        let bytes = read_for(content, layout, workspace, path)?;
        let name = path
            .file_name()
            .ok_or_else(|| format!("{item} is not a file"))?
            .to_string_lossy()
            .into_owned();
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let hash = content.keep_under(workspace, wire::APP, &bytes)?;
        staged.push(json!({"hash": hash, "name": name, "size": size}));
    }
    Ok(staged)
}

/// `path`'s bytes, read only beneath a folder `workspace`'s Files grants it.
fn read_for(
    content: &RecordContent,
    layout: &Arc<Mutex<Option<Arc<dyn WorkspaceAuthority>>>>,
    workspace: &str,
    path: &Path,
) -> Result<Vec<u8>, String> {
    let _ = content;
    let grants = grants(layout, workspace);
    let requested =
        std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let Some(grant) = grants
        .iter()
        .filter(|grant| requested.starts_with(grant))
        .max_by_key(|grant| grant.components().count())
    else {
        return Err(
            "that file is not one of the files this workspace shared with Files".to_owned(),
        );
    };
    let canonical = std::fs::canonicalize(grant).unwrap_or_else(|_| grant.clone());
    if !requested.starts_with(&canonical) || !requested.is_file() {
        return Err(
            "that file is not one of the files this workspace shared with Files".to_owned(),
        );
    }
    let _ = content;
    std::fs::read(&requested).map_err(|error| format!("{}: {error}", requested.display()))
}

/// The folders Files may read in `workspace`, as its sandbox decides it now.
fn grants(
    layout: &Arc<Mutex<Option<Arc<dyn WorkspaceAuthority>>>>,
    workspace: &str,
) -> Vec<PathBuf> {
    let Some(layout) = layout
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    else {
        return Vec::new();
    };
    let mut folders = Vec::new();
    let read = layout.with_workspace(workspace, &mut |workspace| {
        let Some(workspace) = workspace else {
            return;
        };
        folders = files_folders(workspace, Access::Read);
    });
    if let Err(error) = read {
        tracing::warn!(%error, "a workspace's files are not read for a send");
        return Vec::new();
    }
    folders
}

/// The folders the workspace lets Files reach with `access`, as its sandbox decides it now.
fn files_folders(workspace: &tilo_desktop_model::Workspace, access: Access) -> Vec<PathBuf> {
    let now = tilo_clock::unix_now();
    collab_types::caps::sandbox::of_app_at(&workspace.sandbox, wire::APP, now)
        .into_iter()
        .filter_map(|capability| match &capability.scope {
            Scope::FsPath(path) => Some(path),
            _ => None,
        })
        .filter(|path| {
            collab_types::caps::decide_app(
                wire::APP,
                &ResourceRef::FsPath((*path).clone()),
                access,
                &workspace.sandbox,
                Some(now.saturating_mul(1000)),
            ) == CapDecision::Allow
        })
        .map(PathBuf::from)
        .collect()
}

/// The AirDrop receivers this device hands files to, with the question their certificate puts to
/// the person: the seat's own, since it needs a sheet and an address no fabric reaches.
fn air_drops(
    content: &Arc<RecordContent>,
    layout: &Arc<Mutex<Option<Arc<dyn WorkspaceAuthority>>>>,
    continuity: &Continuity,
    wake: &Wake,
) -> Arc<AirDrops> {
    let _ = content;
    let grants_of = Arc::clone(layout);
    let permitting = continuity.clone();
    let changing = wake.clone();
    Arc::new(AirDrops::new(Host {
        enabled: tilo_workspace_service::apps::nearby_send::lab(),
        open: Box::new(move |workspace, requested| {
            let grants = grants(&grants_of, workspace);
            let path = std::fs::canonicalize(requested)
                .map_err(|error| format!("{}: {error}", requested.display()))?;
            if !grants.iter().any(|grant| path.starts_with(grant)) || !path.is_file() {
                return Err("that file is not one of the files Files may hand over".to_owned());
            }
            let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
            let name = requested
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            Ok((file, name))
        }),
        permitted: Box::new(move || permitting.file_transfer_permitted()),
        changed: Box::new(move || changing()),
        language: Box::new(|| Some(ui::i18n::window_language(&["en-US", "de-DE"]))),
        helper_timeout: HELPER_TIMEOUT,
    }))
}

/// A 16-byte id as the reading writes its handle.
fn hex(bytes: &[u8; 16]) -> String {
    hex::encode(bytes)
}

fn refused(reason: String) -> ApiError {
    ApiError::Refused {
        code: "nearby-send".to_owned(),
        reason,
    }
}
