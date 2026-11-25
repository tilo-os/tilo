//! The seat: what a desktop session holds for this device and nothing else can (the screen and
//! its windows, the sound the speakers play, capture, the person's own settings, the board of
//! live activities, the session's published resources, the roles that follow records),
//! served to the apps this device's broker admitted.
//!
//! The device's daemon admits every app and answers every service it keeps itself. A service of
//! the seat is the session's: the session serves one provider for all of them over its shell
//! link ([`PROVIDER`]), and the daemon's broker reaches it through the forwarders here, each the
//! broker's own service of that kind. A forwarder carries the facts the broker admitted the app
//! on (its connection, app, workspace, window) as the call's arguments; the session answers with
//! the same code it ran when it admitted apps itself.
//!
//! The seat's provider is held apart from every provider a call names
//! ([`Providers::serve_seat`]): no app, agent or other device can call it, so those facts are
//! always the broker's, never something an app says about itself. With no session serving the
//! seat, a call is answered that no desktop session serves this computer's seat now.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_collab::wire::{Event, Receipt};
use serde_json::{Map, Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionProvider, Caller as ActionCaller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_lock::MutexRecover as _;
use tokio::sync::{mpsc, oneshot};

use crate::apps::collab_app_broker::{
    AudioOutputFuture, AudioOutputServices, NativeMediaFuture, NativeMediaRequestContext,
    NativeMediaServices,
};
use crate::apps::services::{
    BrowserServiceSessionRequestContext, BrowserServiceSessionServices, Caller, ClipboardAnswering,
    FileClipboardContext, FileClipboardServices, ItemAnswering, LaunchPlace, LauncherServices,
    PublishedResources, Reading, ResourceWatch,
};
use crate::workspaces::providers::Providers;

/// The provider a person's shell serves the seat as: one of the shell's own
/// (`runtime_collab::wire::shell::serves`).
pub const PROVIDER: &str = "session.seat";

/// What a call of the seat is answered while no desktop session serves it.
pub const NO_SEAT: &str =
    "no desktop session serves this computer's screen, sound and settings now";

/// How long a wait for the next change of something the seat keeps is held before it is asked
/// again, after the seat could not be asked: no session serves it, or it closed.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// The services of the seat (the `resource` of a call of [`PROVIDER`]) and their calls (its
/// `action`).
pub mod calls {
    pub const AUDIO_OUTPUT: &str = "audio_output";
    pub const NATIVE_MEDIA: &str = "native_media";
    pub const CONTINUATIONS: &str = "continuations";
    pub const PUBLISHED: &str = "published";
    pub const WORKSPACE_NODES: &str = "workspace_nodes";
    pub const RECORDS: &str = "records";
    pub const SETTINGS: &str = "settings";
    pub const DISPLAY_CONTROL: &str = "display_control";
    pub const LIVE_ACTIVITIES: &str = "live_activities";
    pub const LAUNCHES: &str = "launches";
    pub const KEYBOARD_SHORTCUTS: &str = "keyboard_shortcuts";
    pub const BROWSER_SERVICE_SESSIONS: &str = "browser_service_sessions";
    pub const FILE_CLIPBOARD: &str = "file_clipboard";
    pub const AGENTS: &str = "agents";

    pub const COMMAND: &str = "command";
    pub const DISCONNECT: &str = "disconnect";
    pub const CHANGED: &str = "changed";
    pub const REPORT: &str = "report";
    pub const CLEAR: &str = "clear";
    pub const HANDOFF: &str = "handoff";
    pub const SNAPSHOT: &str = "snapshot";
    pub const READ: &str = "read";
    pub const INVOKE: &str = "invoke";
    pub const ASK: &str = "ask";
    pub const UPDATE: &str = "update";
    pub const SHOW: &str = "show";
    pub const CONTROLS: &str = "controls";
    pub const LAUNCH: &str = "launch";
    pub const ITEM_USE: &str = "item_use";
    pub const ANSWERED_LATER: &str = "answered_later";
    pub const KEEP_ANSWER: &str = "keep_answer";
}

/// The broker's way to the seat: the registry the shell serves it in, and the runtime its calls
/// are carried on.
#[derive(Clone)]
pub struct Seat {
    providers: Providers,
    runtime: tokio::runtime::Handle,
}

impl Seat {
    /// The seat as `providers` holds it while a shell serves it, its calls carried on `runtime`.
    #[must_use]
    pub const fn new(providers: Providers, runtime: tokio::runtime::Handle) -> Self {
        Self { providers, runtime }
    }

    /// Whether a shell serves the seat now.
    #[must_use]
    pub fn served(&self) -> bool {
        self.providers.seat().is_some()
    }

    /// Call `call` of the seat's `service` with `arguments` (a JSON object): what it answers.
    pub fn call(
        &self,
        service: &str,
        call: &str,
        arguments: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send + 'static {
        Self::invoked(
            self.providers.seat(),
            Self::request(service, call, arguments),
        )
    }

    /// [`Self::call`], handing the session `files` on its shell link ahead of the call: each named
    /// by the number the caller knows it by, and told to the session as `handed`, the pairs of
    /// that number and the one it arrives under on the link.
    pub fn call_handing(
        &self,
        service: &str,
        call: &str,
        arguments: Value,
        files: Vec<(u32, std::fs::File)>,
    ) -> impl Future<Output = Result<Value, String>> + Send + 'static {
        let served = self.providers.served_seat();
        let handed = served
            .as_ref()
            .ok_or_else(|| NO_SEAT.to_owned())
            .and_then(|seat| {
                let hand_over = seat
                    .hand_over
                    .as_ref()
                    .ok_or("the desktop session's link carries no files")?;
                files
                    .into_iter()
                    .map(|(named, file)| {
                        hand_over(file)
                            .map(|number| (named, number))
                            .map_err(|error| format!("a file could not be handed over: {error}"))
                    })
                    .collect::<Result<Vec<_>, String>>()
            });
        let request = handed.map(|handed| {
            let mut arguments = match arguments {
                Value::Object(arguments) => arguments,
                other => Map::from_iter([("value".to_owned(), other)]),
            };
            arguments.insert("handed".to_owned(), json!(handed));
            Self::request(service, call, Value::Object(arguments))
        });
        let seat = served.map(|seat| seat.answers);
        async move { Self::invoked(seat, request?).await }
    }

    /// The invocation of `call` of `service` with `arguments`.
    fn request(service: &str, call: &str, arguments: Value) -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress::new(service, call),
            },
            argument: InvokeArgument::Json(match arguments {
                Value::Object(arguments) => arguments,
                other => Map::from_iter([("value".to_owned(), other)]),
            }),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// What `seat` answers `request`.
    async fn invoked(
        seat: Option<Arc<dyn ActionProvider + Send + Sync>>,
        request: InvokeRequest,
    ) -> Result<Value, String> {
        let seat = seat.ok_or_else(|| NO_SEAT.to_owned())?;
        let receipt = seat
            .invoke(&ActionCaller::local_system(), request)
            .await
            .ok_or("the desktop session dropped the call")?
            .map_err(|error| error.to_string())?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            ReceiptStatus::Failed { error } => Err(error.message),
            ReceiptStatus::Rejected { error } => Err(error.to_string()),
            ReceiptStatus::Cancelled { message } => {
                Err(message.unwrap_or_else(|| "the desktop session cancelled the call".into()))
            }
            ReceiptStatus::Accepted => {
                Err("the desktop session took the call without answering it".to_owned())
            }
        }
    }

    /// Tell the seat `call` of `service` with `arguments`, not waiting for its answer: what needs
    /// none (a window's notes, an index's change, a connection gone).
    pub fn tell(&self, service: &str, call: &str, arguments: Value) {
        let asked = self.call(service, call, arguments);
        let (service, call) = (service.to_owned(), call.to_owned());
        self.runtime.spawn(async move {
            if let Err(error) = asked.await {
                tracing::warn!(%service, %call, %error, "the seat did not take what it was told");
            }
        });
    }

    /// `future` run on the seat's runtime, its answer sent on the channel returned.
    fn answered<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let (reply, answered) = oneshot::channel();
        self.runtime.spawn(async move {
            let _ = reply.send(future.await);
        });
        answered
    }
}

/// `value` read as a `T`, or why not.
fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("the seat answered unreadably: {error}"))
}

/// A connection's facts as the seat is told them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MediaContext {
    pub peer_id: u64,
    pub app_id: String,
    pub workspace_id: String,
    pub document_id: String,
}

impl From<&NativeMediaRequestContext> for MediaContext {
    fn from(context: &NativeMediaRequestContext) -> Self {
        Self {
            peer_id: context.peer_id(),
            app_id: context.app_id().to_owned(),
            workspace_id: context.workspace_id().to_owned(),
            document_id: context.document_id().to_owned(),
        }
    }
}

impl MediaContext {
    /// The broker's context of the connection these facts are of, as the seat answers it: the
    /// broker checked the connection before it forwarded the call, and checks it again for each.
    #[must_use]
    pub fn context(self) -> NativeMediaRequestContext {
        NativeMediaRequestContext::seat_forwarded(
            self.peer_id,
            self.app_id,
            self.workspace_id,
            self.document_id,
        )
    }
}

/// The seat's sound output, as an app's audio is played there.
pub struct SeatAudioOutput(pub Seat);

impl AudioOutputServices for SeatAudioOutput {
    fn output(
        &self,
        context: NativeMediaRequestContext,
        command: runtime_collab::audio_output::Command,
    ) -> AudioOutputFuture {
        let asked = self.0.call(
            calls::AUDIO_OUTPUT,
            calls::COMMAND,
            json!({"context": MediaContext::from(&context), "command": command}),
        );
        Box::pin(async move {
            context.revalidate()?;
            read(asked.await?)
        })
    }

    fn disconnect(&self, peer_id: u64) {
        self.0.tell(
            calls::AUDIO_OUTPUT,
            calls::DISCONNECT,
            json!({"peer": peer_id}),
        );
    }
}

/// The seat's calls: the microphone, camera and screen an app's call captures.
pub struct SeatNativeMedia(pub Seat);

impl NativeMediaServices for SeatNativeMedia {
    fn command(
        &self,
        context: NativeMediaRequestContext,
        request: runtime_collab::native_media::Request,
    ) -> NativeMediaFuture {
        let asked = self.0.call(
            calls::NATIVE_MEDIA,
            calls::COMMAND,
            json!({"context": MediaContext::from(&context), "request": request}),
        );
        Box::pin(async move {
            context.revalidate()?;
            read(asked.await?)
        })
    }

    fn disconnect(&self, peer_id: u64) {
        self.0.tell(
            calls::NATIVE_MEDIA,
            calls::DISCONNECT,
            json!({"peer": peer_id}),
        );
    }
}

/// What each window of the seat shows, as its app reports it.
pub struct SeatContinuations(pub Seat);

impl tilo_broker_core::steps::ContinuationServices for SeatContinuations {
    fn report(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String> {
        self.0.tell(
            calls::CONTINUATIONS,
            calls::REPORT,
            json!({
                "workspace": workspace.to_string(),
                "app": app,
                "instance": instance,
                "schema": schema,
                "metadata": metadata,
            }),
        );
        Ok(())
    }

    fn clear(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: &str,
        instance: u64,
    ) -> Result<(), String> {
        self.0.tell(
            calls::CONTINUATIONS,
            calls::CLEAR,
            json!({"workspace": workspace.to_string(), "app": app, "instance": instance}),
        );
        Ok(())
    }

    fn handoff(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: String,
        instance: u64,
        message: Vec<u8>,
    ) -> Result<(), String> {
        self.0.tell(
            calls::CONTINUATIONS,
            calls::HANDOFF,
            json!({
                "workspace": workspace.to_string(),
                "app": app,
                "instance": instance,
                "message": message,
            }),
        );
        Ok(())
    }
}

/// Tell the roles of the session serving `seat` that follow the workspaces' records (the
/// person's mentions, the activity) of every record `writes` says the workspaces took a change
/// of: this device's own write, an app's, or a member's, each by the document it is read as. A
/// write of the store's index of a kind changes no record's content, and is no news to them.
pub fn tell_record_changes(
    seat: Seat,
    mut writes: tokio::sync::broadcast::Receiver<crate::workspaces::ResourceWrite>,
) {
    let runtime = seat.runtime.clone();
    runtime.spawn(async move {
        loop {
            let write = match writes.recv().await {
                Ok(write) => write,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the session was not told of some records changing");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            if collab_types::is_store_document(&write.record)
                && tilo_resource_store::content_resource(&write.record).is_none()
            {
                continue;
            }
            if seat.served() {
                seat.tell(
                    calls::RECORDS,
                    calls::CHANGED,
                    json!({"workspace": write.workspace, "app": write.app, "record": write.record}),
                );
            }
        }
    });
}

/// A resource's reading as the seat answers it.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SeatReading {
    pub revision: u64,
    /// The reading, `None` when nothing publishes the resource.
    pub reading: Option<Value>,
}

impl SeatReading {
    /// The reading as the wire carries it.
    fn wire(self) -> Reading {
        (
            self.revision,
            self.reading
                .and_then(|reading| serde_json::to_vec(&reading).ok()),
        )
    }
}

/// The resources the seat's services publish (the sound, the network, the displays Quick
/// Settings changes), as the apps here read, follow and change them.
pub struct SeatPublished {
    seat: Seat,
    /// The resources followed, each by the task that asks the seat for its next reading.
    watches: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next: std::sync::atomic::AtomicU64,
}

impl SeatPublished {
    /// The resources `seat`'s services publish.
    #[must_use]
    pub fn new(seat: Seat) -> Self {
        Self {
            seat,
            watches: Mutex::default(),
            next: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn mint(&self, watching: tokio::task::AbortHandle) -> ResourceWatch {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.watches.lock_recover().insert(id, watching);
        ResourceWatch::new(id)
    }
}

/// `caller` as the seat is told it.
fn caller_json(caller: &Caller) -> Value {
    json!({"app_id": caller.app_id, "workspace": caller.workspace})
}

impl PublishedResources for SeatPublished {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let asked = self.seat.call(
            calls::PUBLISHED,
            calls::SNAPSHOT,
            json!({"caller": caller_json(caller), "resource": resource}),
        );
        self.seat.answered(async move {
            asked
                .await
                .and_then(read::<SeatReading>)
                .map(SeatReading::wire)
        })
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> Result<ResourceWatch, String> {
        let (seat, caller, resource) =
            (self.seat.clone(), caller_json(caller), resource.to_owned());
        let following = self.seat.runtime.spawn(async move {
            let mut after = 0_u64;
            loop {
                let asked = seat
                    .call(
                        calls::PUBLISHED,
                        calls::READ,
                        json!({"caller": caller, "resource": resource, "after": after}),
                    )
                    .await
                    .and_then(read::<SeatReading>);
                let reading = match asked {
                    Ok(reading) => reading,
                    Err(error) => {
                        if seat.served() {
                            tracing::warn!(%resource, %error, "a published resource could not be followed");
                        }
                        tokio::time::sleep(ASK_AGAIN).await;
                        continue;
                    }
                };
                if reading.revision == after {
                    continue;
                }
                after = reading.revision;
                let (revision, reading) = reading.wire();
                if sink
                    .send(Event::Reading {
                        resource: resource.clone(),
                        revision,
                        reading,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(self.mint(following.abort_handle()))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        if let Some(following) = self.watches.lock_recover().remove(&watch.id()) {
            following.abort();
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        // The broker hands the arguments as the app sent them: what is not JSON is refused here,
        // never handed to the seat as nothing.
        let asked = serde_json::from_slice::<Value>(&arguments)
            .map_err(|error| format!("the call's arguments are not JSON: {error}"))
            .map(|arguments| {
                self.seat.call(
                    calls::PUBLISHED,
                    calls::INVOKE,
                    json!({
                        "caller": caller_json(caller),
                        "resource": resource,
                        "action": action,
                        "arguments": arguments,
                    }),
                )
            });
        self.seat.answered(async move {
            let asked = match asked {
                Ok(asked) => asked.await,
                Err(why) => Err(why),
            };
            asked
                .and_then(read::<Receipt>)
                .unwrap_or_else(|message| Receipt::Refused {
                    code: "unavailable".to_owned(),
                    message,
                })
        })
    }
}

/// An app's asks about a node of a workspace's tree, as the seat's person answers them.
pub struct SeatWorkspaceNodes(pub Seat);

impl tilo_broker_core::services::WorkspaceNodeActions for SeatWorkspaceNodes {
    fn ask(
        &self,
        workspace: &str,
        node: &str,
        ask: tilo_broker_core::services::WorkspaceNodeAsk,
    ) -> Result<
        tilo_actions::remote::Pending<tilo_broker_core::services::WorkspaceNodeOutcome>,
        String,
    > {
        use tilo_broker_core::services::WorkspaceNodeAsk;
        let ask = match ask {
            WorkspaceNodeAsk::Share { share, access } => {
                json!({"share": {"share": share, "access": access}})
            }
            WorkspaceNodeAsk::Unshare { share } => json!({"unshare": {"share": share}}),
            WorkspaceNodeAsk::Permits => json!("permits"),
        };
        let asked = self.0.call(
            calls::WORKSPACE_NODES,
            calls::ASK,
            json!({"workspace": workspace, "node": node, "ask": ask}),
        );
        let (answer, answered) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            answer.complete(asked.await);
        });
        Ok(answered)
    }
}

/// `ask` as [`SeatWorkspaceNodes`] tells the seat it, read back.
///
/// # Errors
/// It names no ask.
pub fn workspace_node_ask(
    ask: &Value,
) -> Result<tilo_broker_core::services::WorkspaceNodeAsk, String> {
    use tilo_broker_core::services::WorkspaceNodeAsk;
    let text = |value: &Value, field: &str| {
        value[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("the ask names no {field}"))
    };
    if ask.as_str() == Some("permits") {
        return Ok(WorkspaceNodeAsk::Permits);
    }
    if let Some(share) = ask.get("share") {
        return Ok(WorkspaceNodeAsk::Share {
            share: text(share, "share")?,
            access: text(share, "access")?,
        });
    }
    if let Some(unshare) = ask.get("unshare") {
        return Ok(WorkspaceNodeAsk::Unshare {
            share: text(unshare, "share")?,
        });
    }
    Err("the ask is none the seat answers".to_owned())
}

/// One app's settings, kept by the seat in the person's own settings: what the broker answers an
/// app's settings calls with when no settings source is its own.
pub struct SeatSettings {
    pub(crate) seat: Seat,
    pub(crate) app_id: String,
    /// The setters of the person's and the device's settings the app may invoke
    /// (`settings_grants`): none for an app that keeps only its own preferences.
    pub(crate) grants: Vec<String>,
    /// Whether the app administers the model providers, whose writes their own rules check.
    pub(crate) providers: bool,
    /// The context a display transaction of the device's settings app runs in, decided by the
    /// broker for each: `None` for an app with no display control.
    pub(crate) display: Option<SeatDisplay>,
}

/// The broker's decision on a display transaction: its context, or why it is refused.
pub(crate) type SeatDisplay = Arc<
    dyn Fn(
            &[u8],
            runtime_collab::DisplayOrigin,
        ) -> Result<crate::apps::services::DisplayControlContext, String>
        + Send
        + Sync,
>;

/// A settings view as the seat answers it.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SeatView {
    pub revision: u64,
    pub document: Value,
}

impl SeatView {
    fn wire(self) -> Result<(u64, Vec<u8>), String> {
        let document = serde_json::to_vec(&self.document).map_err(|error| error.to_string())?;
        if document.len() > runtime_collab::settings::MAX_SETTINGS_WIRE_BYTES {
            return Err("settings view exceeds the wire bound".into());
        }
        Ok((self.revision, document))
    }
}

impl SeatSettings {
    /// The app's view once its revision differs from `after`; at once for 0.
    pub(crate) async fn read(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
        loop {
            let asked = self
                .seat
                .call(
                    calls::SETTINGS,
                    calls::READ,
                    json!({
                        "app": self.app_id,
                        "grants": self.grants,
                        "providers": self.providers,
                        "after": after,
                    }),
                )
                .await
                .and_then(read::<SeatView>);
            match asked {
                Ok(view) if view.revision != after => return view.wire(),
                Ok(_) => {}
                Err(error) if !self.seat.served() => {
                    tilo_failure::ignored!(
                        "no desktop session serves the seat now: it is asked again once one does",
                        drop(error)
                    );
                    tokio::time::sleep(ASK_AGAIN).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Merge `values` into the app's entry in `section`, answering the view after the write.
    pub(crate) async fn update(
        &self,
        section: &str,
        values: &[u8],
        action: Option<&str>,
    ) -> Result<(u64, Vec<u8>), String> {
        if values.len() > runtime_collab::settings::MAX_SETTINGS_WIRE_BYTES {
            return Err("settings write exceeds the wire bound".into());
        }
        let values = serde_json::from_slice::<Value>(values)
            .map_err(|_| "a settings write is a JSON object".to_owned())?;
        self.seat
            .call(
                calls::SETTINGS,
                calls::UPDATE,
                json!({
                    "app": self.app_id,
                    "grants": self.grants,
                    "providers": self.providers,
                    "section": section,
                    "values": values,
                    "action": action,
                }),
            )
            .await
            .and_then(read::<SeatView>)?
            .wire()
    }

    /// Run a display transaction for the device's settings app.
    pub(crate) async fn display(
        &self,
        command: &[u8],
        origin: runtime_collab::DisplayOrigin,
    ) -> Result<Vec<u8>, String> {
        if command.len() > 64 * 1024 {
            return Err("display request exceeds the wire bound".into());
        }
        let display = self
            .display
            .as_ref()
            .ok_or("display control is unavailable to this application")?;
        let context = display(command, origin)?;
        let reply = self
            .seat
            .call(
                calls::DISPLAY_CONTROL,
                calls::COMMAND,
                json!({"context": context, "command": command}),
            )
            .await
            .and_then(read::<Vec<u8>>)?;
        if reply.len() > 64 * 1024 {
            return Err("display reply exceeds the wire bound".into());
        }
        Ok(reply)
    }
}

/// One app's live activity on the seat's board: what the broker answers an app's live-activity
/// calls with when the board is not its own. The activity ends when the app's channel closes.
pub struct SeatLiveActivity {
    pub(crate) seat: Seat,
    pub(crate) app_id: String,
    pub(crate) shown: std::sync::atomic::AtomicBool,
}

impl SeatLiveActivity {
    /// Show `activity` (a JSON object) as the app's activity; empty ends it.
    pub(crate) fn show(&self, activity: &[u8]) -> Result<(), String> {
        let activity = runtime_collab::live_activity::read_activity(activity)?;
        self.shown
            .store(activity.is_some(), std::sync::atomic::Ordering::Release);
        self.seat.tell(
            calls::LIVE_ACTIVITIES,
            calls::SHOW,
            json!({"app": self.app_id, "activity": activity}),
        );
        Ok(())
    }

    /// The controls pressed after `after`, once there is one, as a JSON array, and the serial of
    /// the last.
    pub(crate) async fn controls(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
        loop {
            let asked = self
                .seat
                .call(
                    calls::LIVE_ACTIVITIES,
                    calls::CONTROLS,
                    json!({"app": self.app_id, "after": after}),
                )
                .await;
            match asked {
                Ok(Value::Null) => {}
                Ok(taken) => {
                    let (serial, controls) = read::<(u64, Vec<Value>)>(taken)?;
                    let bytes = serde_json::to_vec(&controls).map_err(|error| error.to_string())?;
                    return Ok((serial, bytes));
                }
                Err(error) if !self.seat.served() => {
                    tilo_failure::ignored!(
                        "no desktop session serves the seat now: it is asked again once one does",
                        drop(error)
                    );
                    tokio::time::sleep(ASK_AGAIN).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for SeatLiveActivity {
    fn drop(&mut self) {
        if self.shown.load(std::sync::atomic::Ordering::Acquire) {
            self.seat.tell(
                calls::LIVE_ACTIVITIES,
                calls::SHOW,
                json!({"app": self.app_id, "activity": Value::Null}),
            );
        }
    }
}

/// `place` as the seat is told it.
fn launch_place_json(place: &LaunchPlace) -> Value {
    match place {
        LaunchPlace::Focus => json!("focus"),
        LaunchPlace::Aside => json!("aside"),
        LaunchPlace::Beside { app_id } => json!({"beside": app_id}),
    }
}

/// A place as [`launch_place_json`] tells the seat it, read back.
///
/// # Errors
/// It names no place.
pub fn launch_place(place: &Value) -> Result<LaunchPlace, String> {
    match (place.as_str(), place["beside"].as_str()) {
        (Some("focus"), _) => Ok(LaunchPlace::Focus),
        (Some("aside"), _) => Ok(LaunchPlace::Aside),
        (_, Some(app_id)) => Ok(LaunchPlace::Beside {
            app_id: app_id.to_owned(),
        }),
        _ => Err("the launch names no place".to_owned()),
    }
}

/// This device's launcher, with an app's windows opened in the person's desktop session while one
/// serves the seat: an app asked to open, or to open a record, starts there, on the same path the
/// shell's own launches take. Every other part of launching is `own`'s: what is installed, the
/// instances that run with no window, and the processes this daemon started.
///
/// The broker asks for a launch on the thread that also carries the shell's link, so the launch
/// is told to the session without waiting for its answer: a launch the session refuses is logged
/// there, and the app sees its window open or not.
pub struct SeatLauncher {
    seat: Seat,
    own: Arc<dyn LauncherServices>,
}

impl SeatLauncher {
    /// `own`, its windows opened in the session that serves `seat` while one does.
    #[must_use]
    pub fn new(seat: Seat, own: Arc<dyn LauncherServices>) -> Self {
        Self { seat, own }
    }
}

impl LauncherServices for SeatLauncher {
    fn begin_executing_binding_observation(&self, launcher_pid: u32) -> Option<u64> {
        self.own.begin_executing_binding_observation(launcher_pid)
    }

    fn observe_executing_binding(
        &self,
        observation: u64,
        phase: tilo_broker_core::services::ExecutingBindingPhase,
    ) {
        self.own.observe_executing_binding(observation, phase);
    }

    fn applications(&self) -> Vec<tilo_broker_core::services::LaunchableApplication> {
        self.own.applications()
    }

    fn applications_for(
        &self,
        app_id: &str,
    ) -> Vec<tilo_broker_core::services::LaunchableApplication> {
        self.own.applications_for(app_id)
    }

    fn launch(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String> {
        if !self.seat.served() {
            return self.own.launch(app, workspace, record, place);
        }
        self.seat.tell(
            calls::LAUNCHES,
            calls::LAUNCH,
            json!({
                "app": app,
                "workspace": workspace.id().to_string(),
                "record": record,
                "place": launch_place_json(place),
            }),
        );
        Ok(())
    }

    fn launch_content(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        content: tilo_broker_core::services::LaunchContent<'_>,
        place: &LaunchPlace,
    ) -> Result<tilo_broker_core::services::Launched, String> {
        if !self.seat.served() {
            return self.own.launch_content(app, workspace, content, place);
        }
        if !content.hash.is_empty() {
            return Err("opening launch content is unsupported on this host".into());
        }
        self.launch(app, workspace, None, place)
            .map(|()| tilo_broker_core::services::Launched::Unconfirmed)
    }

    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.own.attest_process(pid, app, workspace)
    }

    fn windowless(
        &self,
        app: &str,
        workspace: &str,
        record: Option<&str>,
    ) -> Result<Option<tilo_broker_core::services::WindowlessStart>, String> {
        self.own.windowless(app, workspace, record)
    }

    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<tilo_broker_core::services::AuthenticatedApplicationLaunch>, String> {
        self.own
            .authenticated_launch(pid, app, workspace, window_instance)
    }

    #[cfg(target_os = "linux")]
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        app: &str,
        workspace: &str,
    ) -> bool {
        self.own
            .attest_executing_process(launcher_pid, peer, app, workspace)
    }

    fn note_acted_on(&self, app: &str, workspace: &str) {
        self.own.note_acted_on(app, workspace);
    }

    fn closes_applications(&self) -> bool {
        self.own.closes_applications()
    }

    fn close(&self, app: &str, workspace: &tilo_desktop_model::Workspace) -> Result<(), String> {
        self.own.close(app, workspace)
    }
}

/// The device's keyboard shortcuts as the seat's person reads them: the shortcut guide's
/// catalogue, its names in the person's language and its chords from the keymap the session runs.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ShortcutTable {
    /// Changes exactly when the table does; never 0.
    pub revision: u64,
    /// The app the catalogue's actions are checked against.
    pub app_id: String,
    /// The catalogue's rows, under no provider: the broker names the workspace's.
    pub rows: Vec<tilo_actions::remote::CatalogueRow>,
    /// What each action of the catalogue answers, called with no arguments.
    pub answers: Vec<ShortcutAnswer>,
}

/// What one action of the [`ShortcutTable`] answers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ShortcutAnswer {
    pub address: ActionAddress,
    pub answer: Result<tilo_actions::ActionOutput, tilo_actions::ActionFailure>,
}

/// The device's keyboard shortcuts, as the desktop session signed in here reads them.
///
/// The broker asks for them on its own thread (the catalogue it lists, an action it admits), the
/// thread that also carries the call to the session, so it is never kept waiting on the session:
/// this answers from the session's latest table, which it follows for as long as it lives. The
/// table is the session's whole answer: its names are in the person's language and its chords
/// those of the keymap the session runs, so the keymap the broker loads goes unread. With no
/// session serving the seat there is no table, and no shortcut is offered.
pub struct SeatKeyboardShortcuts {
    table: Arc<Mutex<Option<Arc<ShortcutTable>>>>,
    /// The app the catalogue is checked against, as the first table named it: the same for every
    /// table.
    app_id: Arc<std::sync::OnceLock<String>>,
}

impl SeatKeyboardShortcuts {
    /// The shortcuts of the session that serves `seat`, followed from now on.
    #[must_use]
    pub fn follow(seat: Seat) -> Self {
        let table = Arc::new(Mutex::new(None));
        let app_id = Arc::new(std::sync::OnceLock::new());
        let (kept, named) = (Arc::downgrade(&table), Arc::downgrade(&app_id));
        let runtime = seat.runtime.clone();
        runtime.spawn(async move {
            let mut after = 0_u64;
            loop {
                let asked = seat
                    .call(
                        calls::KEYBOARD_SHORTCUTS,
                        calls::READ,
                        json!({"after": after}),
                    )
                    .await
                    .and_then(read::<ShortcutTable>);
                let (Some(table), Some(app_id)) = (kept.upgrade(), named.upgrade()) else {
                    return;
                };
                match asked {
                    Ok(read) if read.revision != after => {
                        after = read.revision;
                        let _ = app_id.set(read.app_id.clone());
                        *table.lock_recover() = Some(Arc::new(read));
                    }
                    // The session held the wait as long as it holds one: ask again.
                    Ok(_) => {}
                    Err(error) => {
                        if seat.served() {
                            tracing::warn!(%error, "the seat's keyboard shortcuts could not be read");
                        } else {
                            after = 0;
                            *table.lock_recover() = None;
                        }
                        tokio::time::sleep(ASK_AGAIN).await;
                    }
                }
            }
        });
        Self { table, app_id }
    }

    fn table(&self) -> Option<Arc<ShortcutTable>> {
        self.table.lock_recover().clone()
    }

    fn descriptor_at(&self, address: &ActionAddress) -> Option<tilo_actions::ActionDescriptor> {
        self.table()?
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|action| action.address == *address)
            .cloned()
    }
}

impl tilo_broker_core::services::KeyboardShortcutServices for SeatKeyboardShortcuts {
    fn app_id(&self) -> &str {
        self.app_id.get().map_or("", String::as_str)
    }

    fn is_resource(&self, resource: &str) -> bool {
        self.table()
            .is_some_and(|table| table.rows.iter().any(|row| row.resource.id == resource))
    }

    fn catalogue_rows(
        &self,
        provider: &str,
        _keymap: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        self.table()
            .map(|table| {
                table
                    .rows
                    .iter()
                    .cloned()
                    .map(|mut row| {
                        provider.clone_into(&mut row.provider);
                        row
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn descriptor(
        &self,
        _keymap: &settings_store::hardware::ShortcutConfig,
        address: &ActionAddress,
    ) -> Option<tilo_actions::ActionDescriptor> {
        self.descriptor_at(address)
    }

    fn apply(
        &self,
        _keymap: &settings_store::hardware::ShortcutConfig,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>> {
        let failure = |code: &str, message: String| {
            Box::new(tilo_actions::ActionFailure {
                code: code.into(),
                message,
                details: None,
                retryable: false,
            })
        };
        let descriptor = self.descriptor_at(address).ok_or_else(|| {
            failure(
                "unknown_action",
                format!(
                    "no keyboard shortcut action `{}` on `{}`",
                    address.action, address.resource
                ),
            )
        })?;
        // The catalogue's actions are answered whole: what an argument names is only checked
        // against the action's parameters.
        if let Some(slot) = argument.filter(|slot| !slot.is_empty()) {
            let json = slot
                .strip_prefix(tilo_actions::params::ARGUMENTS_JSON_PREFIX)
                .ok_or_else(|| {
                    failure(
                        "invalid_arguments",
                        "keyboard shortcut actions take JSON arguments".into(),
                    )
                })?;
            let supplied: Map<String, Value> = serde_json::from_str(json)
                .map_err(|error| failure("invalid_arguments", error.to_string()))?;
            tilo_actions::params::ActionArgs::bind_json(&descriptor.params, &supplied)
                .map_err(|error| failure("invalid_arguments", error.to_string()))?;
        }
        self.table()
            .and_then(|table| {
                table
                    .answers
                    .iter()
                    .find(|answered| answered.address == *address)
                    .map(|answered| answered.answer.clone())
            })
            .ok_or_else(|| {
                failure(
                    "unavailable",
                    "the desktop session did not say what this shortcut action answers".into(),
                )
            })?
            .map_err(Box::new)
    }
}

/// A connection's facts as the seat is told them for one of its account credentials' requests.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ServiceSessionFacts {
    pub peer_id: u64,
    pub person: String,
    pub app_id: String,
    pub workspace_id: String,
    pub document_id: String,
    pub certificate: tilo_user_identity::DeviceCert,
    pub resource_type: Option<String>,
}

impl From<&BrowserServiceSessionRequestContext> for ServiceSessionFacts {
    fn from(context: &BrowserServiceSessionRequestContext) -> Self {
        let read = context.read_context();
        Self {
            peer_id: context.peer_id(),
            person: context.person().to_owned(),
            app_id: read.route().app_id.clone(),
            workspace_id: read.route().cluster_id.clone(),
            document_id: read.route().document_id.clone(),
            certificate: read.certificate().clone(),
            resource_type: read.resource_type().map(str::to_owned),
        }
    }
}

impl ServiceSessionFacts {
    /// The broker's context of the connection these facts are of, as the seat answers it: the
    /// broker checked the connection before it forwarded the call, and checks it again after.
    #[must_use]
    pub fn context(self) -> BrowserServiceSessionRequestContext {
        BrowserServiceSessionRequestContext::seat_forwarded(
            self.peer_id,
            self.person,
            crate::apps::collab_app_broker::NativeDocumentReadContext::seat_forwarded(
                tilo_broker_core::CrdtRoute::record(
                    self.app_id,
                    self.workspace_id,
                    self.document_id,
                ),
                self.certificate,
                self.resource_type,
            ),
        )
    }
}

/// The person's account credentials, kept in the vault the desktop session signed in here
/// opens.
///
/// Whether the connection asking may ask at all is decided here, where its live read lives: the
/// connection is still admitted and reads its app's record, and it is the certified person's own
/// Browser where only Browser asks. The session decides the rest against the workspace it keeps
/// (who administers it, whether the person is still in it) before it opens the vault.
pub struct SeatBrowserServiceSessions(pub Seat);

impl SeatBrowserServiceSessions {
    /// The certified person's own Browser connection, still admitted.
    fn persons_browser(context: &BrowserServiceSessionRequestContext) -> Result<(), String> {
        context.revalidate()?;
        if context.app_id() != tilo_app_id::apps::BROWSER
            || context.person() != hex::encode(context.identity_public_key())
        {
            return Err("Account credentials requires the certified person's own app".into());
        }
        context.revalidate()
    }

    fn asked(
        &self,
        call: &str,
        context: &BrowserServiceSessionRequestContext,
        command: &runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        let asked = self.0.call(
            calls::BROWSER_SERVICE_SESSIONS,
            call,
            json!({"context": ServiceSessionFacts::from(context), "command": command}),
        );
        Box::pin(async move { read(asked.await?) })
    }
}

impl BrowserServiceSessionServices for SeatBrowserServiceSessions {
    fn authorize(&self, context: &BrowserServiceSessionRequestContext) -> Result<(), String> {
        Self::persons_browser(context)
    }

    fn authorize_command(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: &runtime_collab::BrowserServiceSessionCommand,
    ) -> Result<(), String> {
        if !command.browser_only() {
            // An account's password is decided once, by the workspace's vault, for the device
            // that asks.
            return context.revalidate();
        }
        Self::persons_browser(context)
    }

    fn command(
        self: Arc<Self>,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        self.asked(calls::COMMAND, context, &command)
    }

    fn item_use(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        self.asked(calls::ITEM_USE, context, &command)
    }

    fn answered_later(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        self.asked(calls::ANSWERED_LATER, context, &command)
    }

    fn disconnect(&self, peer_id: u64) {
        self.0.tell(
            calls::BROWSER_SERVICE_SESSIONS,
            calls::DISCONNECT,
            json!({"peer": peer_id}),
        );
    }
}

/// A Files window's facts as the seat is told them for one clipboard command.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClipboardFacts {
    pub peer: u64,
    pub window: String,
    pub app_id: String,
    pub workspace: String,
    pub owner: tilo_action_policy::MemberId,
}

impl From<&FileClipboardContext> for ClipboardFacts {
    fn from(context: &FileClipboardContext) -> Self {
        Self {
            peer: context.peer(),
            window: context.window().to_owned(),
            app_id: context.app_id().to_owned(),
            workspace: context.workspace().to_owned(),
            owner: context.owner().clone(),
        }
    }
}

impl ClipboardFacts {
    /// The broker's context of the window these facts are of, as the seat answers it, with the
    /// objects Files selected in `selected` by the numbers the command names them by: the broker
    /// checked the caller for what the command needs before it forwarded it, and checks again
    /// after.
    #[must_use]
    pub fn context(self, selected: HashMap<u32, std::fs::File>) -> FileClipboardContext {
        FileClipboardContext::seat_forwarded(
            (self.peer, self.window, self.app_id),
            (self.workspace, self.owner),
            selected,
        )
    }
}

/// What a clipboard command needs of its caller: a paste, a cut and a paste's cancel change the
/// person's files; everything else reads them.
fn clipboard_needs(command: &runtime_collab::file_clipboard::Command) -> tilo_actions::ActionKind {
    use runtime_collab::file_clipboard::Command;
    match command {
        Command::Paste { .. } | Command::Cancel { .. } | Command::Publish { cut: true, .. } => {
            tilo_actions::ActionKind::Undoable
        }
        _ => tilo_actions::ActionKind::Read,
    }
}

/// Files' clipboard, kept by the desktop session signed in here, whose loop the copies reach.
///
/// The objects a command selects are open in Files' process, which handed them to the broker; the
/// broker hands each on to the session on its shell link, with the call that names it. The
/// caller is checked here, where its live admission lives, before and after the session answers.
pub struct SeatFileClipboard(pub Seat);

impl FileClipboardServices for SeatFileClipboard {
    fn command(
        self: Arc<Self>,
        context: &FileClipboardContext,
        command: runtime_collab::file_clipboard::Command,
    ) -> ClipboardAnswering {
        let needs = clipboard_needs(&command);
        if let Err(refused) = context.authorize(needs) {
            return Box::pin(std::future::ready(Err(refused)));
        }
        let mut selected = Vec::new();
        for descriptor in command.selected().map(|selected| selected.descriptor) {
            if selected.iter().any(|(named, _)| *named == descriptor) {
                continue;
            }
            // One Files did not hand over is answered by the session as the command reaches it.
            if let Ok(file) = context.duplicate_selected(descriptor) {
                selected.push((descriptor, file));
            }
        }
        let asked = self.0.call_handing(
            calls::FILE_CLIPBOARD,
            calls::COMMAND,
            json!({"context": ClipboardFacts::from(context), "command": command}),
            selected,
        );
        let context = context.clone();
        Box::pin(async move {
            let reply = read(asked.await?)?;
            context.authorize(needs)?;
            Ok(reply)
        })
    }

    fn disconnect(&self, peer: u64) {
        self.0.tell(
            calls::FILE_CLIPBOARD,
            calls::DISCONNECT,
            json!({"peer": peer}),
        );
    }
}

/// The handed files a call of the seat names (its `handed` pairs: the number the caller knows each
/// by, and the one it arrived under on the link), taken from `received`, by the caller's numbers.
/// Every one named is taken, so none outlives the call.
///
/// # Errors
/// The call names its files unreadably.
#[cfg(unix)]
pub fn handed_files(
    received: &runtime_portal::descriptors::ReceivedDescriptors,
    arguments: &Value,
) -> Result<HashMap<u32, std::fs::File>, String> {
    let handed = match arguments.get("handed") {
        None | Some(Value::Null) => Vec::new(),
        Some(handed) => serde_json::from_value::<Vec<(u32, u32)>>(handed.clone())
            .map_err(|error| format!("handed: {error}"))?,
    };
    Ok(handed
        .into_iter()
        .filter_map(|(named, number)| {
            received
                .take(number)
                .map(|fd| (named, std::fs::File::from(fd)))
        })
        .collect())
}

/// What this computer's networks wake and which networks it saved, answered by the desktop
/// session signed in here, which reads its `NetworkManager`; the passphrases are the daemon's.

/// One agent a workspace's record names active, as the session that reads the record lists it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeatAgent {
    pub workspace: String,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    /// The person the agent acts for.
    pub acts_for: String,
    /// What it may spend today and has, as the record keeps it.
    pub spend: Option<runtime_collab::AgentSpend>,
}

/// One agent session the person admitted at the session's MCP server, while it goes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeatAgentSession {
    pub session: String,
    pub workspace: String,
    pub agent: String,
    /// What the person allowed it for every later step like the one it asked about.
    pub answers: Vec<tilo_action_policy::Grant>,
}

/// The workspaces' agents and the agent sessions the desktop session signed in here holds: the
/// records it reads every pass, and the MCP connections the daemon hands it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTable {
    /// Changes exactly when the table does; 0 only for the table of no session.
    pub revision: u64,
    pub agents: Vec<SeatAgent>,
    pub sessions: Vec<SeatAgentSession>,
}

impl AgentTable {
    /// The table of `agents` and `sessions`, with the revision that names exactly it.
    #[must_use]
    pub fn of(agents: Vec<SeatAgent>, sessions: Vec<SeatAgentSession>) -> Self {
        use std::hash::{Hash as _, Hasher as _};
        let mut table = Self {
            revision: 0,
            agents,
            sessions,
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(&table)
            .unwrap_or_default()
            .hash(&mut hasher);
        table.revision = hasher.finish().max(1);
        table
    }
}

/// The workspaces' agents and the agent sessions, as the desktop session signed in here holds
/// them: whom the broker admits to a channel as an agent, and whether the turn an agent's step
/// runs in still goes.
///
/// The broker asks on its own thread, the one that carries the call to the session, so it is
/// never kept waiting on the session: this answers from the session's latest table, which it
/// follows for as long as it lives. With no session serving the seat there is no table: no agent
/// is listed and no session goes.
pub struct SeatAgents {
    seat: Seat,
    table: Arc<Mutex<Arc<AgentTable>>>,
}

impl SeatAgents {
    /// The agents of the session that serves `seat`, followed from now on.
    #[must_use]
    pub fn follow(seat: Seat) -> Arc<Self> {
        let table = Arc::new(Mutex::new(Arc::new(AgentTable::default())));
        let kept = Arc::downgrade(&table);
        let follower = seat.clone();
        seat.runtime.spawn(async move {
            let mut after = 0_u64;
            loop {
                let asked = follower
                    .call(calls::AGENTS, calls::READ, json!({"after": after}))
                    .await
                    .and_then(read::<AgentTable>);
                let Some(table) = kept.upgrade() else {
                    return;
                };
                match asked {
                    Ok(read) if read.revision != after => {
                        after = read.revision;
                        *table.lock_recover() = Arc::new(read);
                    }
                    // The session held the wait as long as it holds one: ask again.
                    Ok(_) => {}
                    Err(error) => {
                        if follower.served() {
                            tracing::warn!(%error, "the seat's agents could not be read");
                        } else {
                            after = 0;
                            *table.lock_recover() = Arc::default();
                        }
                        tokio::time::sleep(ASK_AGAIN).await;
                    }
                }
            }
        });
        Arc::new(Self { seat, table })
    }

    fn table(&self) -> Arc<AgentTable> {
        Arc::clone(&self.table.lock_recover())
    }
}

impl tilo_broker_core::services::AgentRoster for SeatAgents {
    fn listed(&self, workspace: &str) -> Vec<tilo_broker_core::services::ListedAgent> {
        self.table()
            .agents
            .iter()
            .filter(|agent| agent.workspace == workspace)
            .map(|agent| tilo_broker_core::services::ListedAgent {
                id: agent.id.clone(),
                name: agent.name.clone(),
                avatar: agent.avatar.clone(),
                acts_for: agent.acts_for.clone(),
            })
            .collect()
    }

    fn spend(&self, workspace: &str, agent: &str) -> Option<runtime_collab::AgentSpend> {
        self.table()
            .agents
            .iter()
            .find(|listed| listed.workspace == workspace && listed.id == agent)?
            .spend
            .clone()
    }
}

impl tilo_broker_core::services::AgentSessions for SeatAgents {
    fn going(
        &self,
        workspace: &str,
        agent: &str,
        session: &str,
    ) -> Option<tilo_broker_core::services::GoingSession> {
        self.table()
            .sessions
            .iter()
            .find(|going| {
                going.session == session && going.workspace == workspace && going.agent == agent
            })
            .map(|going| tilo_broker_core::services::GoingSession {
                origin: None,
                answers: going.answers.clone(),
            })
    }

    fn keep_answer(&self, session: &str, grant: tilo_action_policy::Grant) {
        // Kept here at once as well, so the session's next step is admitted on it before the
        // session's table says so.
        {
            let mut table = self.table.lock_recover();
            let mut kept = AgentTable::clone(&table);
            if let Some(going) = kept
                .sessions
                .iter_mut()
                .find(|going| going.session == session)
            {
                going.answers.retain(|held| held.id != grant.id);
                going.answers.push(grant.clone());
                *table = Arc::new(kept);
            }
        }
        self.seat.tell(
            calls::AGENTS,
            calls::KEEP_ANSWER,
            json!({"session": session, "grant": grant}),
        );
    }
}

/// The seat's services, each a forwarder to it, for a broker's [`crate::apps::services::ProcessServices`].
pub struct Forwarders {
    pub audio_output: Arc<dyn AudioOutputServices>,
    pub native_media: Arc<dyn NativeMediaServices>,
    pub continuations: Arc<dyn tilo_broker_core::steps::ContinuationServices>,
    pub workspace_nodes: Arc<dyn tilo_broker_core::services::WorkspaceNodeActions>,
    pub keyboard_shortcuts: Arc<dyn tilo_broker_core::services::KeyboardShortcutServices>,
    pub browser_service_sessions: Arc<dyn BrowserServiceSessionServices>,
    pub file_clipboard: Arc<dyn FileClipboardServices>,
    /// The workspaces' agents and the agent sessions: see [`SeatAgents`].
    pub agents: Arc<SeatAgents>,
}

impl Seat {
    /// A forwarder to the seat for each of its services.
    #[must_use]
    pub fn forwarders(&self) -> Forwarders {
        Forwarders {
            audio_output: Arc::new(SeatAudioOutput(self.clone())),
            native_media: Arc::new(SeatNativeMedia(self.clone())),
            continuations: Arc::new(SeatContinuations(self.clone())),
            workspace_nodes: Arc::new(SeatWorkspaceNodes(self.clone())),
            keyboard_shortcuts: Arc::new(SeatKeyboardShortcuts::follow(self.clone())),
            browser_service_sessions: Arc::new(SeatBrowserServiceSessions(self.clone())),
            file_clipboard: Arc::new(SeatFileClipboard(self.clone())),
            agents: SeatAgents::follow(self.clone()),
        }
    }
}
