//! Nearby Send as the shell publishes it: the devices this one can hand a file to, the offers
//! another device made back, and the actions that answer them.
//!
//! The offers and transfers are the device daemon's ([`tilo_runtime::device::nearby`]): it holds
//! them, carries the content and answers another of the person's devices over the action service
//! it already serves. This session keeps what is the seat's alone: the share sheet, the files the
//! person hands over — which it stages on the device's one content shelf, where its Files grants
//! let it read them — and the receiver no device of theirs is. It reads the daemon's resource over
//! the shell link and invokes its actions there, so the resource's own shape and its actions live
//! in one place ([`tilo_app_host::nearby_send`]) and a call is checked by the same
//! code whether Files, the panel or a script made it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use collab_types::caps::{Access, CapDecision, CapOverride, ResourceRef, Scope};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, ApiError, InvocationId, ResourceSpec};
use tilo_app_host::nearby_send::{
    AirDrops, HELPER_TIMEOUT, Host, NearbySendAction, Reading, TransferState,
};
use tilo_capture_studio::{Refusal, Sent};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect as _;
use tilo_service::{Service, Started, Wake, reading};
use tilo_session_client::daemon_documents::{DaemonShell, whole};
use tilo_session_client::workspace_view::WorkspaceView;
use tilo_session_core::desktop::state::DesktopHandle;
use ui::widget::ScreenshotSendDevice;

use crate::platform::fabric::Continuity;
use crate::record_content::RecordContent;

use tilo_runtime::device::nearby as wire;

/// The session's Nearby Send resource: what the daemon shows, and the actions that change it.
/// What the session does with a resource it shares: opens the invitation sheet for it.
pub(crate) type Share = Box<dyn Fn(&str) -> Result<(), String>>;

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
    /// The seat's shell on the loop: which workspace is in front, where a send is made and a
    /// received file lands.
    handle: DesktopHandle,
    /// The folders a workspace's Files grants, asked of the loop from the threads that read the
    /// files a send hands over.
    grants: Arc<FilesGrants>,
    /// How a resource is shared: the session opens its invitation sheet.
    share: Share,
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
    /// The daemon's reading of the workspace in front, followed off the loop: what a pass reads,
    /// never waiting on the daemon.
    followed: RefCell<Option<Followed>>,
}

/// The daemon's Nearby Send reading of one workspace, followed by a task of the daemon link's
/// runtime: each answer waits for the next change.
struct Followed {
    workspace: String,
    latest: Arc<Mutex<Result<Reading, String>>>,
    follow: tokio::task::AbortHandle,
}

impl Drop for Followed {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

/// How long a follow waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

impl Served {
    /// The session's Nearby Send, reading and invoking the daemon's over `daemon`.
    pub(crate) fn new(
        daemon: DaemonShell,
        content: Arc<RecordContent>,
        handle: DesktopHandle,
        continuity: Continuity,
        share: Share,
        wake: Wake,
    ) -> Self {
        Self {
            inner: Rc::new(Inner::new(daemon, content, handle, continuity, share, wake)),
        }
    }

    /// Answer the Files grants a send asked for since the last pass, from the shell `handle` shows
    /// or, for a workspace the shell does not show (the person's own), from the daemon's `view`.
    /// On the loop's pass only.
    pub(crate) fn answer(&self, handle: &DesktopHandle, view: Option<&WorkspaceView>) {
        self.inner.grants.answer(handle, view);
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
        handle: DesktopHandle,
        continuity: Continuity,
        share: Share,
        wake: Wake,
    ) -> Self {
        let grants = Arc::new(FilesGrants {
            asks: Mutex::new(Vec::new()),
            wake: wake.clone(),
        });
        let air_drop = air_drops(&content, &grants, &continuity, &wake);
        let permitting = continuity;
        Self {
            daemon,
            content,
            handle,
            grants,
            share,
            air_drop,
            permitted: Arc::new(move || permitting.file_transfer_permitted()),
            wake,
            published: RefCell::new(None),
            last: RefCell::new(Reading::default()),
            followed: RefCell::new(None),
        }
    }

    /// The workspace in front, as the shell shows it: where a send is made and a received file
    /// lands. `None` while this session reads no daemon of its own, which serves neither.
    fn workspace(&self) -> Option<String> {
        if !self.has_link() {
            return None;
        }
        let active = self.handle.read(|tree| tree.active.to_string());
        (!active.is_empty()).then_some(active)
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
    /// `AirDrop` attempt is one of this session's sends.
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
            reading
                .transfers
                .push(tilo_app_host::nearby_send::NearbyTransferRow {
                    state,
                    progress: if state == TransferState::Done { 100 } else { 0 },
                    peer: attempt.endpoint.clone(),
                    destination: String::new(),
                    time: 0,
                    incoming: false,
                    name: attempt.name.clone(),
                    handle: hex(&attempt.id),
                });
        }
        *self.last.borrow_mut() = reading.clone();
        reading
    }

    /// The daemon's reading of Nearby Send in `workspace`, as the shell link carries it.
    /// The daemon's reading of `workspace` as its follow last heard it: the follow starts on the
    /// first ask, and a pass never waits for the daemon.
    fn read_daemon(&self, workspace: &str) -> Result<Reading, String> {
        let mut followed = self.followed.borrow_mut();
        if followed
            .as_ref()
            .is_none_or(|followed| followed.workspace != workspace)
        {
            *followed = Some(self.follow(workspace));
        }
        followed.as_ref().map_or_else(
            || Ok(Reading::default()),
            |followed| followed.latest.lock_recover().clone(),
        )
    }

    /// Follow the daemon's reading of `workspace`, waking the session each time it moved.
    fn follow(&self, workspace: &str) -> Followed {
        let latest = Arc::new(Mutex::new(Ok(Reading::default())));
        let (daemon, wake, kept) = (
            self.daemon.clone(),
            Arc::clone(&self.wake),
            Arc::clone(&latest),
        );
        let at = workspace.to_owned();
        let follow = self.daemon.runtime().spawn(async move {
            let mut after: Option<String> = None;
            loop {
                let mut read = whole(&at, "session", wire::RESOURCE);
                read.wait_ms = after
                    .is_some()
                    .then_some(tilo_actions::remote::MAX_REMOTE_WAIT_MS);
                read.after.clone_from(&after);
                match daemon.read(read).await {
                    Ok(answered) => {
                        after = answered
                            .get("revision")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned);
                        *kept.lock_recover() = reading_of(&answered);
                        wake();
                    }
                    Err(error) => {
                        *kept.lock_recover() = Err(error);
                        tokio::time::sleep(ASK_AGAIN).await;
                    }
                }
            }
        });
        Followed {
            workspace: workspace.to_owned(),
            latest,
            follow: follow.abort_handle(),
        }
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
        let grants = Arc::clone(&self.grants);
        let wake = self.wake.clone();
        let items = items.to_vec();
        let device = device.to_owned();
        std::thread::spawn(move || {
            match stage(&content, &grants, &workspace, &items) {
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
        let grants = Arc::clone(&self.grants);
        let wake = self.wake.clone();
        let id = id.to_owned();
        std::thread::spawn(move || {
            let outcome = (|| {
                let name = path
                    .file_name()
                    .ok_or("the file has no name")?
                    .to_string_lossy()
                    .into_owned();
                let path = read_for(&grants, &workspace, &path)?;
                let size = std::fs::metadata(&path)
                    .map_err(|error| error.to_string())?
                    .len();
                let hash = content.keep_file_under(&workspace, wire::APP, &path)?;
                let arguments = json!({
                    "items": [{"hash": hash, "name": name, "size": size}],
                    "device": id,
                });
                daemon.invoke_receipt_blocking(Some(&workspace), request(wire::SEND, arguments))?;
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
        vec![tilo_app_host::nearby_send::resource(crate::i18n::t(
            "session-nearby-send",
        ))]
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

/// The reading `answered` carries: none while the daemon publishes nothing.
fn reading_of(answered: &Value) -> Result<Reading, String> {
    let value = answered["records"]
        .as_array()
        .and_then(|records| records.first())
        .map_or(Value::Null, |record| record["value"].clone());
    if value.is_null() {
        return Ok(Reading::default());
    }
    serde_json::from_value(value).map_err(|error| format!("the reading does not read: {error}"))
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
    grants: &Arc<FilesGrants>,
    workspace: &str,
    items: &[String],
) -> Result<Vec<Value>, String> {
    let mut staged = Vec::with_capacity(items.len());
    for item in items {
        let path = read_for(grants, workspace, Path::new(item))?;
        let name = path
            .file_name()
            .ok_or_else(|| format!("{item} is not a file"))?
            .to_string_lossy()
            .into_owned();
        let size = std::fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .len();
        let hash = content.keep_file_under(workspace, wire::APP, &path)?;
        staged.push(json!({"hash": hash, "name": name, "size": size}));
    }
    Ok(staged)
}

/// `path`, canonical, once a folder `workspace`'s Files grants it: what a send stages.
fn read_for(grants: &Arc<FilesGrants>, workspace: &str, path: &Path) -> Result<PathBuf, String> {
    let granted = grants.folders(workspace);
    let requested =
        std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let Some(grant) = granted
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
    Ok(requested)
}

/// How long a send waits for the loop to read a workspace's Files grants: the loop answers on its
/// next pass, so this only bounds a frozen session.
const GRANTS_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A workspace whose Files folders a send asked the loop for, and where the answer goes.
type Ask = (
    String,
    tilo_actions::remote::Completer<Option<Vec<PathBuf>>>,
);

/// The folders a workspace's Files grants, asked of the loop from the threads that read the files
/// a send hands over ([`Self::answer`]).
struct FilesGrants {
    asks: Mutex<Vec<Ask>>,
    wake: Wake,
}

impl FilesGrants {
    /// Answer what the sends asked since the last pass: the folders the seat's shell shows for
    /// `workspace`, or for one the shell does not show (the person's own) the folders the daemon's
    /// `view` keeps for it. On the loop's pass only.
    fn answer(&self, handle: &DesktopHandle, view: Option<&WorkspaceView>) {
        let asks = std::mem::take(
            &mut *self
                .asks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (workspace, done) in asks {
            let shown = handle.read(|tree| {
                tilo_broker_core::services::workspace_in(tree, &workspace)
                    .map(|shown| folders_in(&shown.sandbox, Access::Read))
            });
            done.complete(shown.or_else(|| view.and_then(|view| via_daemon(view, &workspace))));
        }
    }

    /// The folders Files may read in `workspace`, as the loop answers on its next pass.
    fn folders(&self, workspace: &str) -> Vec<PathBuf> {
        let (done, answered) = tilo_actions::remote::pending();
        self.asks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((workspace.to_owned(), done));
        (self.wake)();
        match answered.wait_timeout(GRANTS_WAIT) {
            Ok(Some(Some(folders))) => folders,
            Ok(Some(None)) => Vec::new(),
            Ok(None) => {
                tracing::warn!("a workspace's files are not read for a send");
                Vec::new()
            }
            Err(_) => {
                tracing::warn!("the session did not answer a workspace's files for a send");
                Vec::new()
            }
        }
    }
}

/// The folders the daemon's `view` keeps for `workspace`, a workspace the shell does not show.
/// `None` for one this device does not hold, or whose governance has not arrived.
fn via_daemon(view: &WorkspaceView, workspace: &str) -> Option<Vec<PathBuf>> {
    let id = tilo_desktop_model::WorkspaceId::parse(workspace)?;
    let sandbox = view
        .reading(id, |reading| reading.sandbox.clone())
        .flatten()?;
    Some(folders_in(&sandbox, Access::Read))
}

/// The folders `sandbox` lets Files reach with `access`, as it decides it now.
fn folders_in(sandbox: &[CapOverride], access: Access) -> Vec<PathBuf> {
    let now = tilo_clock::unix_now();
    collab_types::caps::sandbox::of_app_at(sandbox, wire::APP, now)
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
                sandbox,
                Some(now.saturating_mul(1000)),
            ) == CapDecision::Allow
        })
        .map(PathBuf::from)
        .collect()
}

/// The `AirDrop` receivers this device hands files to, with the question their certificate puts to
/// the person: the seat's own, since it needs a sheet and an address no fabric reaches.
fn air_drops(
    content: &Arc<RecordContent>,
    grants: &Arc<FilesGrants>,
    continuity: &Continuity,
    wake: &Wake,
) -> Arc<AirDrops> {
    let _ = content;
    let grants_of = Arc::clone(grants);
    let permitting = continuity.clone();
    let changing = wake.clone();
    Arc::new(AirDrops::new(Host {
        enabled: tilo_app_host::nearby_send::lab(),
        open: Box::new(move |workspace, requested| {
            let granted = grants_of.folders(workspace);
            let path = std::fs::canonicalize(requested)
                .map_err(|error| format!("{}: {error}", requested.display()))?;
            if !granted.iter().any(|grant| path.starts_with(grant)) || !path.is_file() {
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
        language: Box::new(|| Some(tilo_ui_base::i18n::window_language(&["en-US", "de-DE"]))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use collab_types::caps::Effect;
    use tilo_action_policy::Grantee;

    /// A send stages the files it hands over only beneath a folder its workspace's Files grants:
    /// its thread asks the loop, which answers from the seat's shell, and a file outside the grant
    /// is refused.
    #[test]
    fn a_send_reads_a_granted_folder_and_refuses_another() {
        let home = tempfile::tempdir().expect("a directory for the files");
        let root = std::fs::canonicalize(home.path()).expect("the directory's real path");
        let granted = root.join("shared");
        std::fs::create_dir_all(&granted).expect("the folder the workspace shares");
        let staged = granted.join("note.txt");
        std::fs::write(&staged, b"what the send carries").expect("a file in the shared folder");
        let elsewhere = root.join("private");
        std::fs::create_dir_all(&elsewhere).expect("a folder the workspace does not share");
        let refused = elsewhere.join("secret.txt");
        std::fs::write(&refused, b"not for a send").expect("a file outside the shared folder");

        let id = tilo_desktop_model::WorkspaceId::new();
        let mut workspace = tilo_desktop_model::Workspace::new(id, "Probe".to_owned());
        workspace.sandbox = vec![CapOverride {
            app: Some(tilo_member_types::AppId::new(wire::APP)),
            ..CapOverride::new(
                Grantee::App(wire::APP.to_owned()),
                Scope::FsPath(granted.to_string_lossy().into_owned()),
                Effect::Grant,
                Access::Read,
            )
        }];
        let mut tree = tilo_desktop_model::Layout {
            active: id,
            ..tilo_desktop_model::Layout::default()
        };
        tree.workspaces.push(workspace);
        let handle = DesktopHandle::new(tree, 1);
        let workspace = id.to_string();

        let (woken, woke) = mpsc::channel();
        let grants = Arc::new(FilesGrants {
            asks: Mutex::new(Vec::new()),
            wake: Arc::new(move || {
                let _ = woken.send(());
            }),
        });

        // The send reads on a thread of its own, as staging does; the loop answers here.
        let read = |path: &Path| {
            let asking = Arc::clone(&grants);
            let name = workspace.clone();
            let path = path.to_owned();
            let reading = std::thread::spawn(move || read_for(&asking, &name, &path));
            woke.recv().expect("the send asks the loop");
            grants.answer(&handle, None);
            reading.join().expect("the send's thread")
        };

        let kept = read(&staged).expect("a file the workspace shares is read");
        assert_eq!(
            std::fs::read(&kept).expect("the file the send keeps"),
            b"what the send carries"
        );
        let refusal = read(&refused).expect_err("a file the workspace does not share is refused");
        assert!(refusal.contains("shared with Files"), "{refusal}");
    }
}
