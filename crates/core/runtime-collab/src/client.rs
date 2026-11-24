//! The typed endpoint of the collaboration wire: a connection to the host
//! ([`CollabClient`]), the records its calls are about ([`RecordHandle`]) and what
//! they deliver. Every call is one [`wire::Operation`] on the connection the
//! launcher handed this process; nothing here keeps state the host does not
//! name by a handle or a subscription.

use std::sync::Arc;

use runtime_portal::NotAvailable;

use crate::wire::{
    Changes, ContextUpdate, Event, Handle, InvocationCall, LiveActivityCall, Operation,
    Publication, Read, Receipt, Reply, Report, Service, SettingsCall, Topic,
    client::{Answerer, Client, Failure, Subscription},
};
use crate::{ChannelContext, Invocation, PresenceEvent};
use crate::{
    ExportedAction, NearbyTransferCommand, NearbyTransferReply, PresencePayload, SpeakerCommand,
    SpeakerReply, SurfaceDelta, WifiContinuityCommand, WifiContinuityReply, WorkspaceControlEvent,
    WorkspaceControlInput, audio_output, committed_document, file_clipboard, native_media,
};
use crate::{
    MAX_NEARBY_TRANSFER_WIRE_BYTES, MAX_SPEAKER_WIRE_BYTES, MAX_WIFI_CONTINUITY_WIRE_BYTES,
};

/// Where a connection's calls hand the host descriptors, if it can.
#[cfg(unix)]
type Descriptors = Option<runtime_portal::descriptors::DescriptorSender>;
#[cfg(not(unix))]
type Descriptors = Option<std::convert::Infallible>;

#[cfg(unix)]
#[path = "shell_dispatch.rs"]
mod shell_dispatch;
#[cfg(unix)]
pub use shell_dispatch::{Provided, ShellDispatch};

pub(crate) fn failure(failure: Failure) -> NotAvailable {
    match failure {
        Failure::Refused(reason) => NotAvailable::Refused(reason),
        Failure::Closed => NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            failure.to_string(),
        )),
        Failure::Unreadable(_) => NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            failure.to_string(),
        )),
    }
}

pub(crate) fn refused(message: impl Into<String>) -> NotAvailable {
    NotAvailable::Refused(message.into())
}

fn unexpected(reply: &Reply) -> NotAvailable {
    refused(format!("the collaboration host answered {reply:?}"))
}

/// Connect to the collaboration host over the connection the launcher
/// handed this process. Every caller in the process shares it.
///
/// # Errors
///
/// [`NotAvailable::ServiceFdMissing`] outside a Tilo launch, or another
/// [`NotAvailable`] when the connection cannot be set up.
pub fn try_connect() -> Result<CollabClient, NotAvailable> {
    shared::connect()
}

/// Resolves once a connection has ended.
pub type ConnectionEnd = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

/// [`try_connect`], with what resolves once the connection has ended.
///
/// # Errors
///
/// As [`try_connect`].
pub fn try_connect_until_closed() -> Result<(CollabClient, ConnectionEnd), NotAvailable> {
    let client = try_connect()?;
    let closed = Box::pin(client.client.closed());
    Ok((client, closed))
}

#[cfg(unix)]
mod shared {
    //! The one connection a process holds to the collaboration host.

    use std::os::fd::BorrowedFd;
    use std::sync::{Mutex, OnceLock, PoisonError};

    use rustix::io::{FdFlags, fcntl_setfd};

    use super::{CollabClient, NotAvailable};

    pub(super) fn connect() -> Result<CollabClient, NotAvailable> {
        static CONNECTION: OnceLock<Mutex<Option<CollabClient>>> = OnceLock::new();
        let mut held = CONNECTION
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = held.as_ref() {
            return Ok(client.clone());
        }
        let fd = runtime_portal::service_fd("collab").ok_or(NotAvailable::ServiceFdMissing)?;
        // SAFETY: `service_fd` has just checked that the slot is open; the
        // borrow lasts only for these calls.
        let inherited = unsafe { BorrowedFd::borrow_raw(fd) };
        // The launcher clears CLOEXEC only for this process's exec; children
        // the app starts later must not hold the connection.
        fcntl_setfd(inherited, FdFlags::CLOEXEC).map_err(std::io::Error::from)?;
        let socket = std::os::unix::net::UnixStream::from(inherited.try_clone_to_owned()?);
        #[cfg(target_os = "linux")]
        let socket = {
            let mut socket = socket;
            if std::env::var(runtime_portal::process_binding::ENV).as_deref() == Ok("1") {
                runtime_portal::process_binding::complete(&mut socket)?;
            }
            socket
        };
        socket.set_nonblocking(true)?;
        let descriptors = runtime_portal::descriptors::DescriptorSender::new();
        let sender = descriptors.clone();
        let (ready, started) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("tilo-svc-collab".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let stream = match tokio::net::UnixStream::from_std(socket) {
                        Ok(stream) => stream,
                        Err(error) => {
                            let _ = ready.send(Err(error));
                            return;
                        }
                    };
                    let stream =
                        runtime_portal::descriptors::DescriptorStream::sending(stream, sender);
                    let (reader, writer) = tokio::io::split(stream);
                    let (client, carry) = crate::wire::client::Client::new(reader, writer);
                    let _ = ready.send(Ok(client));
                    carry.await;
                });
            })?;
        let client = started
            .recv()
            .map_err(|_| std::io::Error::other("the collaboration connection did not start"))??;
        let client = CollabClient::new(client).with_descriptors(descriptors);
        *held = Some(client.clone());
        drop(held);
        Ok(client)
    }
}

#[cfg(not(unix))]
mod shared {
    use super::{CollabClient, NotAvailable};

    pub(super) fn connect() -> Result<CollabClient, NotAvailable> {
        Err(NotAvailable::ServiceFdMissing)
    }
}

/// A connection to the collaboration host.
#[derive(Clone)]
pub struct CollabClient {
    client: Client,
    descriptors: Descriptors,
    records: Arc<OpenRecords>,
}

impl CollabClient {
    /// A client over an already connected wire client.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client,
            descriptors: None,
            records: Arc::default(),
        }
    }

    /// A client over `reader`/`writer`, and the future that carries its
    /// frames; spawn it.
    pub fn over<R, W>(reader: R, writer: W) -> (Self, impl Future<Output = ()> + use<R, W>)
    where
        R: tokio::io::AsyncRead + Unpin + 'static,
        W: tokio::io::AsyncWrite + Unpin + 'static,
    {
        let (client, carry) = Client::new(reader, writer);
        (Self::new(client), carry)
    }

    /// A client over `stream`, already connected to the host, and what
    /// resolves once the connection has ended. Its frames are carried by a
    /// task spawned on the current runtime.
    pub fn connected<S>(stream: S) -> (Self, ConnectionEnd)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
        let (reader, writer) = tokio::io::split(stream);
        let (client, carry) = Self::over(reader, writer);
        tokio::spawn(carry);
        let end = Box::pin(client.closed());
        (client, end)
    }

    /// [`Self::connected`] for a stream that stays on this thread: its
    /// frames are carried by a task spawned on the current `LocalSet`.
    pub fn connected_local<S>(stream: S) -> (Self, ConnectionEnd)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + 'static,
    {
        let (reader, writer) = tokio::io::split(stream);
        let (client, carry) = Self::over(reader, writer);
        tokio::task::spawn_local(carry);
        let end = Box::pin(client.closed());
        (client, end)
    }

    /// [`Self::connected`] over the connected Unix socket `fd`, which this
    /// takes ownership of.
    #[cfg(unix)]
    pub fn from_connected_fd(
        fd: std::os::fd::OwnedFd,
    ) -> Result<(Self, ConnectionEnd), NotAvailable> {
        let socket = std::os::unix::net::UnixStream::from(fd);
        socket.set_nonblocking(true)?;
        Ok(Self::connected(tokio::net::UnixStream::from_std(socket)?))
    }

    /// [`Self::connected`] over the connected pipe `pipe`, opened for overlapped I/O, which this
    /// takes ownership of.
    #[cfg(windows)]
    pub fn from_connected_pipe(
        pipe: std::os::windows::io::OwnedHandle,
    ) -> Result<(Self, ConnectionEnd), NotAvailable> {
        use std::os::windows::io::IntoRawHandle as _;
        // SAFETY: an overlapped pipe handle, owned here and handed over exactly once.
        let pipe = unsafe {
            tokio::net::windows::named_pipe::NamedPipeClient::from_raw_handle(
                pipe.into_raw_handle(),
            )
        }?;
        Ok(Self::connected(pipe))
    }

    /// The wire client underneath.
    #[must_use]
    pub const fn wire(&self) -> &Client {
        &self.client
    }

    /// Hand the host descriptors over `sender`, the descriptor stream of the
    /// very connection `self` calls over; see
    /// [`RecordHandle::local_file_clipboard_command`].
    #[cfg(unix)]
    #[must_use]
    pub fn with_descriptors(
        mut self,
        sender: runtime_portal::descriptors::DescriptorSender,
    ) -> Self {
        self.descriptors = Some(sender);
        self
    }

    /// [`Self::with_descriptors`] for a caller that only knows the descriptor stream
    /// [`runtime_portal::DescriptorStreamSource`] handed it: on a host
    /// without descriptors the descriptor stream is empty and the client stays as it was.
    #[must_use]
    pub fn with_optional_descriptors(self, sender: runtime_portal::Descriptors) -> Self {
        #[cfg(unix)]
        {
            Self {
                descriptors: sender,
                ..self
            }
        }
        #[cfg(not(unix))]
        {
            let _ = sender;
            self
        }
    }

    /// Resolves once the connection has ended.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        self.client.closed()
    }

    /// The record `key` names, of the resource kind `kind` (none when empty), as this
    /// connection's calls about it name it. Nothing goes to the host until a call does: the host
    /// decides each call about the record for this connection, the app, workspace and person its
    /// launch admitted. Handles of one record share what the host keeps for it, which it lets go
    /// once the last of them is dropped.
    #[must_use]
    pub fn record(&self, key: &str, kind: &str) -> RecordHandle {
        let record = crate::wire::Record::new(key, kind);
        let channel = {
            let mut open = self
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            open.retain(|_, record| record.strong_count() > 0);
            if let Some(channel) = open.get(key).and_then(std::sync::Weak::upgrade) {
                channel
            } else {
                let channel = Arc::new(Channel {
                    client: self.client.clone(),
                    record,
                });
                open.insert(key.to_owned(), Arc::downgrade(&channel));
                channel
            }
        };
        // Descriptors travel over the one connection, which each of its handles shares; a
        // page without descriptor streams has nothing to share, the empty value being `Copy`.
        #[cfg(unix)]
        let descriptors = self.descriptors.clone();
        #[cfg(not(unix))]
        let descriptors = self.descriptors;
        RecordHandle {
            channel,
            descriptors,
        }
    }

    /// The session's published resources: what its services read and the
    /// actions that change them, opened on this connection the first time
    /// one is used. No record is named for them — they are the device's,
    /// not a workspace's — so an app reads the sound, the displays or the
    /// settings whether or not it holds a workspace.
    #[must_use]
    pub fn services(&self) -> ServicesHandle {
        ServicesHandle {
            client: self.client.clone(),
            handle: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Where the workspace `channel_id` names stands for apps on this device.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection fails.
    pub async fn workspace_status(
        &self,
        channel_id: &str,
    ) -> Result<crate::WorkspaceStatus, NotAvailable> {
        self.workspace(channel_id, None).await
    }

    /// Where the workspace stands once it is no longer `seen`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection fails.
    pub async fn await_workspace(
        &self,
        channel_id: &str,
        seen: &crate::WorkspaceStatus,
    ) -> Result<crate::WorkspaceStatus, NotAvailable> {
        self.workspace(channel_id, Some(seen.clone())).await
    }

    async fn workspace(
        &self,
        channel_id: &str,
        seen: Option<crate::WorkspaceStatus>,
    ) -> Result<crate::WorkspaceStatus, NotAvailable> {
        match self
            .client
            .call(Operation::Workspace {
                record: channel_id.to_owned(),
                seen,
            })
            .await
            .map_err(failure)?
        {
            Reply::Workspace(status) => Ok(status),
            other => Err(unexpected(&other)),
        }
    }
}

/// A record this connection's calls name; the host lets go of what it kept for it once the
/// last clone of its handle, and every stream opened from it, is gone.
struct Channel {
    client: Client,
    record: crate::wire::Record,
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.client.leave(self.record.key.clone());
    }
}

/// A record this connection's calls are about. Cheap to clone.
#[derive(Clone)]
pub struct RecordHandle {
    channel: Arc<Channel>,
    descriptors: Descriptors,
}

/// The records a connection's calls name, by key: every handle of one shares it, the host
/// keeping one thing per record whatever kind a handle names it as.
type OpenRecords =
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<Channel>>>;

/// The most a resource command or its reply takes on the wire, as the store bounds them.
const MAX_RESOURCE_WIRE_BYTES: usize = 4 << 20;

/// Encode one `what` control command, refusing it when it is invalid or
/// larger than `max_bytes`.
fn encode_bounded_command<C: serde::Serialize>(
    what: &str,
    command: &C,
    valid: bool,
    max_bytes: usize,
) -> Result<Vec<u8>, NotAvailable> {
    if !valid {
        return Err(refused(format!("invalid {what} command")));
    }
    let command_json = serde_json::to_vec(command)
        .map_err(|error| refused(format!("encode {what} command: {error}")))?;
    if command_json.len() > max_bytes {
        return Err(refused(format!("{what} command exceeds wire bound")));
    }
    Ok(command_json)
}

/// Decode one `what` control reply, refusing it when it is larger than
/// `max_bytes` or fails its own validity check.
fn decode_bounded_reply<R: serde::de::DeserializeOwned>(
    what: &str,
    bytes: &[u8],
    max_bytes: usize,
    valid: impl FnOnce(&R) -> bool,
) -> Result<R, NotAvailable> {
    if bytes.len() > max_bytes {
        return Err(refused(format!("{what} reply exceeds wire bound")));
    }
    let decoded: R = serde_json::from_slice(bytes)
        .map_err(|error| refused(format!("decode {what} reply: {error}")))?;
    if !valid(&decoded) {
        return Err(refused(format!("invalid {what} reply")));
    }
    Ok(decoded)
}

impl RecordHandle {
    /// The record this handle's calls name.
    #[must_use]
    pub fn record(&self) -> &crate::wire::Record {
        &self.channel.record
    }

    /// The record's key: its app, workspace and record.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.channel.record.key
    }

    async fn call(&self, request: Operation) -> Result<Reply, NotAvailable> {
        self.channel.client.call(request).await.map_err(failure)
    }

    async fn done(&self, request: Operation) -> Result<(), NotAvailable> {
        match self.call(request).await? {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    async fn publish(&self, publication: Publication) -> Result<(), NotAvailable> {
        self.done(Operation::Publish {
            record: self.channel.record.clone(),
            publication,
        })
        .await
    }

    async fn report(&self, report: Report) -> Result<(), NotAvailable> {
        self.publish(Publication::Report(report)).await
    }

    async fn command(
        &self,
        service: Service,
        body: Vec<u8>,
        on_behalf_of: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        match self
            .call(Operation::Command {
                record: self.channel.record.clone(),
                service,
                body,
                on_behalf_of: on_behalf_of.to_vec(),
            })
            .await?
        {
            Reply::Answer(answer) => Ok(answer),
            other => Err(unexpected(&other)),
        }
    }

    /// Wait until `of` may have changed since `after` (0 at first, then what
    /// the previous call returned), and answer the value to wait from next.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route does not follow `of` or the
    /// host is unavailable.
    pub async fn changed(&self, of: Changes, after: u64) -> Result<u64, NotAvailable> {
        match self
            .call(Operation::Changed {
                record: self.channel.record.clone(),
                of,
                after,
            })
            .await?
        {
            Reply::Changed(change) => Ok(change),
            other => Err(unexpected(&other)),
        }
    }

    async fn feed(&self, topic: Topic) -> Result<Feed, NotAvailable> {
        let subscription = self
            .channel
            .client
            .subscribe(self.channel.record.clone(), topic)
            .await
            .map_err(failure)?;
        Ok(Feed(Arc::new(FeedInner {
            channel: Arc::clone(&self.channel),
            finish: Box::new(subscription.finisher()),
            subscription: tokio::sync::Mutex::new(subscription),
        })))
    }

    /// Send PCM or controls to this launched application's output-only service.
    pub async fn audio_output_command(
        &self,
        command: &audio_output::Command,
    ) -> Result<audio_output::Snapshot, NotAvailable> {
        let bytes = command.encode().map_err(refused)?;
        let reply = self.command(Service::AudioOutput, bytes, &[]).await?;
        if reply.len() > audio_output::MAX_WIRE_BYTES {
            return Err(refused("audio output reply too large"));
        }
        let snapshot: audio_output::Snapshot =
            serde_json::from_slice(&reply).map_err(|_| refused("invalid audio output reply"))?;
        if !snapshot.valid() {
            return Err(refused("invalid audio output state"));
        }
        Ok(snapshot)
    }

    /// Whether the workspace seals what it keeps of this record. Asked first, it is where the
    /// host admits this connection to the record, or refuses it with its reason.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host refuses the record or the connection fails.
    pub async fn sealed(&self) -> Result<bool, NotAvailable> {
        match self.committed_document(&committed_document::Request::Writes).await? {
            committed_document::Reply::Writes { sealed } => Ok(sealed),
            _ => Err(refused("the host did not say how it keeps the record")),
        }
    }

    /// Read this channel's current committed history.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection fails or the host answers
    /// with an invalid reply.
    pub async fn committed_document(
        &self,
        command: &committed_document::Request,
    ) -> Result<committed_document::Reply, NotAvailable> {
        let bytes = command.encode().map_err(refused)?;
        let reply = self.command(Service::CommittedDocument, bytes, &[]).await?;
        committed_document::Reply::decode(&reply).map_err(refused)
    }

    /// Control this launched application's local governed call. The host
    /// derives the exact caller and record from the authenticated connection.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection fails or the host answers
    /// with an invalid status.
    pub async fn native_media_command(
        &self,
        command: &native_media::Request,
    ) -> Result<native_media::Snapshot, NotAvailable> {
        let bytes = command.encode().map_err(refused)?;
        let reply = self.command(Service::NativeMedia, bytes, &[]).await?;
        native_media::Snapshot::decode(&reply).map_err(refused)
    }

    /// Submit one bounded realtime-session continuity command, answered with
    /// a versioned application-contract reply.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host rejects the command or the
    /// connection fails.
    pub async fn realtime_command(&self, command_json: &[u8]) -> Result<Vec<u8>, NotAvailable> {
        self.command(Service::Realtime, command_json.to_vec(), &[])
            .await
    }

    /// Ask the device's one store `command` for this app's resources, a write or a read as the
    /// person the app acts for may read, and answer its reply. Both are the store's encoding
    /// (`tilo_resource_store::wire`), which only the apps and services that read it build: a
    /// connection carries them as bytes, and the page's start never holds the store's types.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the device refuses the command, the connection fails, or
    /// either exceeds the wire bound.
    pub async fn resource_command(&self, command: &[u8]) -> Result<Vec<u8>, NotAvailable> {
        if command.len() > MAX_RESOURCE_WIRE_BYTES {
            return Err(refused("resource command exceeds wire bound"));
        }
        let reply = self
            .command(Service::Resources, command.to_vec(), &[])
            .await?;
        if reply.len() > MAX_RESOURCE_WIRE_BYTES {
            return Err(refused("resource reply exceeds wire bound"));
        }
        Ok(reply)
    }

    /// Hold `query` open in the device's one store (`tilo_resource_store::Query`, in the store's
    /// encoding): its first page, then each page a write here or a member's change makes
    /// different, until the stream is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the device refuses the query or the connection fails.
    pub async fn open_resources(&self, query: &[u8]) -> Result<ResourcePages, NotAvailable> {
        if query.len() > MAX_RESOURCE_WIRE_BYTES {
            return Err(refused("resource query exceeds wire bound"));
        }
        Ok(ResourcePages(
            self.feed(Topic::Resources(query.to_vec())).await?,
        ))
    }

    /// Hold open what the host says of the `actions` a window shows: all of them now, then all
    /// again whenever grants change one, until the stream is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host refuses or the connection fails.
    pub async fn shown_actions(
        &self,
        actions: Vec<crate::shown_actions::ShownAction>,
    ) -> Result<ActionVerdictFeed, NotAvailable> {
        Ok(ActionVerdictFeed(
            self.feed(Topic::ShownActions(actions)).await?,
        ))
    }

    /// Submit one bounded Nearby Send control command. File contents stay on
    /// the shell-owned resumable transport; this carries only
    /// picker/status/consent control data.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the command or answers
    /// with an oversized or invalid reply.
    pub async fn nearby_transfer_command(
        &self,
        command: &NearbyTransferCommand,
    ) -> Result<NearbyTransferReply, NotAvailable> {
        let command_json = encode_bounded_command(
            "nearby transfer",
            command,
            command.valid(),
            MAX_NEARBY_TRANSFER_WIRE_BYTES,
        )?;
        let reply = self
            .command(Service::NearbyTransfer, command_json, &[])
            .await?;
        decode_bounded_reply(
            "nearby transfer",
            &reply,
            MAX_NEARBY_TRANSFER_WIRE_BYTES,
            NearbyTransferReply::valid,
        )
    }

    async fn local_file(&self, bytes: Vec<u8>, parent: &[u8]) -> Result<Vec<u8>, NotAvailable> {
        self.command(Service::LocalFile, bytes, parent).await
    }

    /// Submit a bounded local Files command on this admitted channel, under
    /// its original invocation, if any.
    ///
    /// Each selected descriptor names a descriptor open in this process, which
    /// must stay open until this returns. A duplicate of it is handed to the
    /// host on this connection's descriptor stream, and the command sent names
    /// it by its descriptor number instead.
    pub async fn local_file_clipboard_command(
        &self,
        command: &file_clipboard::Command,
        parent: Option<[u8; 16]>,
    ) -> Result<file_clipboard::Reply, NotAvailable> {
        if !command.valid() {
            return Err(refused("invalid local clipboard command"));
        }
        // Measure with the widest descriptor number, so the bound holds whatever
        // numbers the descriptors receive.
        let mut widest = command.clone();
        for selected in widest.selected_mut() {
            selected.descriptor = file_clipboard::MAX_DESCRIPTOR;
        }
        let widest = serde_json::to_vec(&widest).map_err(|error| refused(error.to_string()))?;
        if widest.len() > file_clipboard::MAX_WIRE_BYTES {
            return Err(refused("clipboard command exceeds bound"));
        }
        let mut command = command.clone();
        self.hand_over_selected(&mut command).map_err(refused)?;
        let bytes = serde_json::to_vec(&command).map_err(|error| refused(error.to_string()))?;
        let reply = self
            .local_file(bytes, parent.as_ref().map_or(&[], |id| id.as_slice()))
            .await?;
        if reply.len() > file_clipboard::MAX_WIRE_BYTES {
            return Err(refused("clipboard reply exceeds bound"));
        }
        serde_json::from_slice(&reply).map_err(|error| refused(error.to_string()))
    }

    /// Submit an already encoded local Files envelope (write attribution,
    /// external moderation) and return the host's raw answer.
    pub(crate) async fn local_file_envelope(
        &self,
        bytes: Vec<u8>,
        parent: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        self.local_file(bytes, parent).await
    }

    /// Hand each selected descriptor of `command` to the host, renaming it
    /// by its descriptor number.
    fn hand_over_selected(&self, command: &mut file_clipboard::Command) -> Result<(), String> {
        let mut selected = command.selected_mut().peekable();
        if selected.peek().is_none() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let sender = self
                .descriptors
                .as_ref()
                .ok_or("this connection cannot hand the host descriptors")?;
            // Duplicate every descriptor before sending any, so a closed one
            // leaves nothing on the descriptor stream that no request names.
            let selected = selected
                .map(|selected| {
                    let raw = i32::try_from(selected.descriptor)
                        .map_err(|_| "invalid selected descriptor".to_string())?;
                    // SAFETY: the caller keeps every selected descriptor open
                    // until this call returns; the borrow ends with the duplicate.
                    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
                    let fd = fd
                        .try_clone_to_owned()
                        .map_err(|error| format!("selected descriptor unavailable: {error}"))?;
                    Ok((selected, fd))
                })
                .collect::<Result<Vec<_>, String>>()?;
            for (selected, fd) in selected {
                selected.descriptor = sender.send(fd).map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = &self.descriptors;
            Err("selected descriptors require Unix".into())
        }
    }

    /// Use the shell-owned sealed Wi-Fi secret bridge from the authenticated
    /// Settings route.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the command or reply violates its wire
    /// contract or the host refuses it.
    pub async fn wifi_continuity_command(
        &self,
        command: &WifiContinuityCommand,
    ) -> Result<WifiContinuityReply, NotAvailable> {
        let command_json = encode_bounded_command(
            "Wi-Fi continuity",
            command,
            command.valid(),
            MAX_WIFI_CONTINUITY_WIRE_BYTES,
        )?;
        let reply = self
            .command(Service::WifiContinuity, command_json, &[])
            .await?;
        decode_bounded_reply(
            "Wi-Fi continuity",
            &reply,
            MAX_WIFI_CONTINUITY_WIRE_BYTES,
            WifiContinuityReply::valid,
        )
    }

    /// Ask the shell's speaker-group authority from the authenticated
    /// Settings route.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the command or reply violates its wire
    /// contract or the host refuses it.
    pub async fn speaker_command(
        &self,
        command: &SpeakerCommand,
    ) -> Result<SpeakerReply, NotAvailable> {
        let command_json =
            encode_bounded_command("speaker", command, command.valid(), MAX_SPEAKER_WIRE_BYTES)?;
        let reply = self.command(Service::Speaker, command_json, &[]).await?;
        decode_bounded_reply(
            "speaker",
            &reply,
            MAX_SPEAKER_WIRE_BYTES,
            SpeakerReply::valid,
        )
    }

    /// Subscribe to lifecycle edges addressed to this exact application.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe_realtime(&self) -> Result<RealtimeStream, NotAvailable> {
        Ok(RealtimeStream(self.feed(Topic::Realtime).await?))
    }

    /// This application's settings: its own entries in the session's records
    /// and the shared sections every app reads.
    #[must_use]
    pub fn settings(&self) -> crate::settings::AppSettingsHandle {
        crate::settings::AppSettingsHandle {
            channel: self.clone(),
        }
    }

    pub(crate) async fn settings_call(&self, call: SettingsCall) -> Result<Reply, NotAvailable> {
        self.call(Operation::Settings {
            record: self.channel.record.clone(),
            call,
        })
        .await
    }

    /// This application's live activity: what it shows outside its windows
    /// and the controls pressed on it.
    #[must_use]
    pub fn live_activity(&self) -> crate::live_activity::LiveActivityHandle {
        crate::live_activity::LiveActivityHandle {
            channel: self.clone(),
        }
    }

    pub(crate) async fn live_activity_call(
        &self,
        call: LiveActivityCall,
    ) -> Result<Reply, NotAvailable> {
        self.call(Operation::LiveActivity {
            record: self.channel.record.clone(),
            call,
        })
        .await
    }

    /// Open a workspace control session with `target_device`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host refuses the session.
    pub async fn open_workspace_control(
        &self,
        target_device: [u8; 32],
    ) -> Result<WorkspaceControlHandle, NotAvailable> {
        Ok(WorkspaceControlHandle(
            self.feed(Topic::WorkspaceControl { target_device }).await?,
        ))
    }

    /// Report a bounded, inert continuation projection to the local shell.
    /// The host binds it to this certified channel and never relays it to
    /// peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the report.
    pub async fn report_continuation(
        &self,
        schema_version: u16,
        metadata: &[u8],
    ) -> Result<(), NotAvailable> {
        self.report(Report::Continuation {
            schema_version,
            metadata: metadata.to_vec(),
        })
        .await
    }

    /// Answer a handoff request, or report that the shown document arrived
    /// retired. Never relayed to peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the report.
    pub async fn handoff(&self, message: &[u8]) -> Result<(), NotAvailable> {
        self.report(Report::Handoff(message.to_vec())).await
    }

    /// Clear this application's previously reported continuation projection.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the report.
    pub async fn clear_continuation(&self) -> Result<(), NotAvailable> {
        self.report(Report::ClearContinuation).await
    }

    /// Change this application's entries in the session's persistent search
    /// index. `delta_json` is an encoded
    /// `ui::search::index_publication::IndexDelta`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the delta.
    pub async fn publish_search_index(&self, delta_json: &[u8]) -> Result<(), NotAvailable> {
        self.report(Report::SearchIndex(delta_json.to_vec())).await
    }

    /// The channel's presence.
    #[must_use]
    pub fn presence(&self) -> PresenceHandle {
        PresenceHandle {
            channel: self.clone(),
        }
    }

    /// Roster and permission changes, and the session's directed asks of
    /// this endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe_context(&self) -> Result<ContextStream, NotAvailable> {
        Ok(ContextStream(self.feed(Topic::Context).await?))
    }

    /// Hand the host content this app has just stored. A document holds a
    /// reference to its binaries, never the bytes, so this is the only way
    /// the content behind a reference leaves the process that produced it.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses it.
    pub async fn offer_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), NotAvailable> {
        self.publish(Publication::Content {
            hash: hash.to_owned(),
            bytes: bytes.to_vec(),
        })
        .await
    }

    /// Ask for content a document here references and this app lacks; the
    /// answer arrives on [`Self::subscribe_blobs`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses it.
    pub async fn want_blob(&self, hash: &str) -> Result<(), NotAvailable> {
        self.done(Operation::Read {
            record: self.channel.record.clone(),
            read: Read::Content {
                hash: hash.to_owned(),
            },
        })
        .await
    }

    /// Content of the channel's record that arrived, or where a read of it
    /// has got to.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe_blobs(&self) -> Result<BlobStream, NotAvailable> {
        Ok(BlobStream(self.feed(Topic::Content).await?))
    }

    /// Replace what this application has published as its whole action
    /// surface. What crosses is inert: ids and labels, never the closures
    /// that run them, which only an invocation back to this app reaches.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the surface.
    pub async fn publish_actions(&self, actions: &[ExportedAction]) -> Result<(), NotAvailable> {
        let whole = crate::SurfacePublication::next(None, actions).0;
        let json = serde_json::to_string(&whole).map_err(|error| refused(error.to_string()))?;
        self.publish(Publication::Surface(json)).await
    }

    /// A change to what this channel has already published.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the delta (it does not
    /// apply to what the host holds); the caller then sends one with
    /// `reset` set.
    pub async fn publish_action_surface(&self, delta: &SurfaceDelta) -> Result<(), NotAvailable> {
        let json = serde_json::to_string(delta).map_err(|error| refused(error.to_string()))?;
        self.publish(Publication::Surface(json)).await
    }

    /// Answer for this application's own paged action directory from now
    /// on: the host asks `answerer` for each page and resolution instead of
    /// keeping its rows. Publishing again replaces the previous answerer.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the directory.
    pub async fn publish_action_directory(&self, answerer: Answerer) -> Result<(), NotAvailable> {
        self.channel.client.answer(
            crate::wire::client::Answering::Record(self.channel.record.key.clone()),
            answerer,
        );
        self.publish(Publication::Directory).await
    }

    /// The invocations the session addresses to this application. Callers
    /// preserve each invocation's stable id through dispatch and every
    /// lifecycle update.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe_stable_invocations(&self) -> Result<InvocationStream, NotAvailable> {
        Ok(InvocationStream(
            self.feed(Topic::Invocations { stable_ids: true }).await?,
        ))
    }

    async fn invocation(&self, id: u64, call: InvocationCall) -> Result<Reply, NotAvailable> {
        self.call(Operation::Invocation {
            record: self.channel.record.clone(),
            id,
            call,
        })
        .await
    }

    /// Report what became of an invocation: an empty `code` means it ran,
    /// anything else is a refusal reported to whoever asked.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn complete_invocation(
        &self,
        id: u64,
        code: &str,
        message: &str,
    ) -> Result<(), NotAvailable> {
        match self
            .invocation(
                id,
                InvocationCall::Complete {
                    code: code.to_owned(),
                    message: message.to_owned(),
                },
            )
            .await?
        {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Publish an accepted, progress or terminal lifecycle update. The host
    /// checks ownership, invocation identity, ordering, and terminal-once.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn update_invocation(&self, id: u64, update_json: &str) -> Result<(), NotAvailable> {
        match self
            .invocation(id, InvocationCall::Update(update_json.to_owned()))
            .await?
        {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Whether the caller asked to cancel this invocation.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn invocation_cancellation(&self, id: u64) -> Result<bool, NotAvailable> {
        match self.invocation(id, InvocationCall::Cancelled).await? {
            Reply::Cancelled(cancelled) => Ok(cancelled),
            other => Err(unexpected(&other)),
        }
    }

    /// Record a terminal local-UI effect in shared workspace activity. The
    /// host checks the address is a currently published effect and reduces
    /// the update to a redacted receipt.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the receipt.
    pub async fn record_local_effect(
        &self,
        resource: &str,
        action: &str,
        update_json: &str,
    ) -> Result<(), NotAvailable> {
        self.publish(Publication::Effect {
            resource: resource.to_owned(),
            action: action.to_owned(),
            update: update_json.to_owned(),
        })
        .await
    }

    /// Send one `tilo_actions::remote::RemoteActionFrame` request and return
    /// the answering frame. The host derives the caller from this channel.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the frame.
    pub async fn remote_action(&self, frame: &[u8]) -> Result<Vec<u8>, NotAvailable> {
        self.command(Service::RemoteAction, frame.to_vec(), &[])
            .await
    }

    /// Launch an exact installed package through this authenticated channel,
    /// returning its observed startup outcome.
    pub async fn launch_installed_application(
        &self,
        app_id: &str,
        package_root: Option<&str>,
    ) -> Result<(u64, String), NotAvailable> {
        self.launch_installed_application_for(app_id, package_root, None)
            .await
    }

    /// Launch on a live invocation's original caller's behalf.
    pub async fn launch_installed_application_for(
        &self,
        app_id: &str,
        package_root: Option<&str>,
        on_behalf_of: Option<[u8; 16]>,
    ) -> Result<(u64, String), NotAvailable> {
        match self
            .call(Operation::Launch {
                record: self.channel.record.clone(),
                app_id: app_id.to_owned(),
                package_root: package_root.unwrap_or_default().to_owned(),
                on_behalf_of: on_behalf_of.map(Vec::from).unwrap_or_default(),
            })
            .await?
        {
            Reply::Invoked { id, update } => Ok((id, update)),
            other => Err(unexpected(&other)),
        }
    }

    /// Admit a local document action the app already performed.
    ///
    /// # Errors
    ///
    /// [`crate::LocalDocumentActionError::Rejected`] when the host proves it
    /// was not admitted; [`crate::LocalDocumentActionError::Uncertain`] for
    /// any other failure, which proves nothing.
    pub async fn admit_local_document_action(
        &self,
        report: &crate::LocalDocumentActionReport,
    ) -> Result<crate::PublishedInvocationObservation, crate::LocalDocumentActionError> {
        let bytes = report
            .encode()
            .map_err(crate::LocalDocumentActionError::Rejected)?;
        let reply = self
            .channel
            .client
            .call(Operation::Admit {
                record: self.channel.record.clone(),
                report: bytes,
            })
            .await
            .map_err(|error| match error {
                Failure::Refused(reason) => crate::local_document_rejection(&reason).map_or_else(
                    || crate::LocalDocumentActionError::Uncertain(reason.clone()),
                    |reason| crate::LocalDocumentActionError::Rejected(reason.to_owned()),
                ),
                Failure::Closed | Failure::Unreadable(_) => {
                    crate::LocalDocumentActionError::Uncertain(error.to_string())
                }
            })?;
        let Reply::Admitted(observation) = reply else {
            return Err(crate::LocalDocumentActionError::Uncertain(format!(
                "the collaboration host answered {reply:?}"
            )));
        };
        crate::checked_observation(observation, report)
            .map_err(crate::LocalDocumentActionError::Uncertain)
    }

    /// The approved check `request` starts, followed through its reported
    /// progress to its receipt.
    pub async fn approved_check_reported_progress(
        &self,
        request: &crate::approved_checks::Request,
    ) -> Result<crate::approved_checks::ApprovedCheckReportedProgressStream, NotAvailable> {
        let encoded = request.encode().map_err(refused)?;
        let feed = self.feed(Topic::ApprovedCheck(encoded)).await?;
        Ok(crate::approved_checks::ApprovedCheckReportedProgressStream::new(feed, request.clone()))
    }

    /// Run an approved check and wait for its receipt.
    pub(crate) async fn approved_check_command(
        &self,
        request: Vec<u8>,
    ) -> Result<Vec<u8>, NotAvailable> {
        self.command(Service::ApprovedCheck, request, &[]).await
    }

    /// Send a browser service-session command, the Browser's only.
    pub(crate) async fn browser_service_session(
        &self,
        command: Vec<u8>,
    ) -> Result<Vec<u8>, NotAvailable> {
        self.command(Service::BrowserServiceSession, command, &[])
            .await
    }
}

struct FeedInner {
    /// Keeps the channel open while its stream is read.
    channel: Arc<Channel>,
    /// Asks the broker to end the subscription once it handed over what it holds.
    finish: Box<dyn Fn() + Send + Sync>,
    subscription: tokio::sync::Mutex<Subscription>,
}

/// One subscription of a channel, read by one reader at a time.
#[derive(Clone)]
pub(crate) struct Feed(Arc<FeedInner>);

impl Feed {
    /// The next event, or why the subscription ended.
    pub(crate) async fn event(&self) -> Result<Event, NotAvailable> {
        self.0
            .subscription
            .lock()
            .await
            .event()
            .await
            .map_err(failure)
    }
}

fn unexpected_event(event: &Event) -> NotAvailable {
    refused(format!("the collaboration host delivered {event:?}"))
}

/// Content of a channel's record, or where a read of it has got to.
#[derive(Debug, Clone)]
pub struct BlobEvent {
    pub hash: String,
    pub bytes: Vec<u8>,
    pub state: tilo_content::ContentState,
}

/// Content that arrived for the channel's record.
#[derive(Clone)]
pub struct BlobStream(Feed);

impl BlobStream {
    /// The next arrival.
    pub async fn read(&self) -> Result<BlobEvent, NotAvailable> {
        match self.0.event().await? {
            Event::Content { hash, state, bytes } => Ok(BlobEvent { hash, bytes, state }),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// The invocations addressed to this app.
#[derive(Clone)]
pub struct InvocationStream(Feed);

impl InvocationStream {
    /// Take no more invocations: the broker hands over the ones it already
    /// addressed to this stream and then ends it, so a reader that reads on
    /// until [`Self::read`] fails has every invocation it was ever handed.
    pub fn finish(&self) {
        (self.0.0.finish)();
    }

    /// The next invocation.
    pub async fn read(&self) -> Result<Invocation, NotAvailable> {
        match self.0.event().await? {
            Event::Invocation(invocation) => Ok(Invocation {
                invocation_uuid: invocation.invocation,
                id: invocation.id,
                resource: invocation.resource,
                action: invocation.action,
                argument: invocation.argument,
                actor: invocation.actor,
                context_json: invocation.context.unwrap_or_default(),
                typed: invocation.typed,
            }),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// Realtime continuity events for this app, as JSON.
#[derive(Clone)]
pub struct RealtimeStream(Feed);

/// The pages of a query held open; dropping it lets the query go.
pub struct ResourcePages(Feed);

impl ResourcePages {
    /// The next page, in the store's encoding (`tilo_resource_store::Page`).
    pub async fn read(&self) -> Result<Vec<u8>, NotAvailable> {
        match self.0.event().await? {
            Event::Resources(page) => Ok(page),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// What the host says of the actions a [`RecordHandle::shown_actions`] names; dropping it ends
/// the subscription.
pub struct ActionVerdictFeed(Feed);

impl ActionVerdictFeed {
    /// The decisions as they are next.
    pub async fn read(&self) -> Result<Vec<crate::shown_actions::ActionDecision>, NotAvailable> {
        match self.0.event().await? {
            Event::ShownActions(decisions) => Ok(decisions),
            other => Err(unexpected_event(&other)),
        }
    }
}

impl RealtimeStream {
    /// The next event.
    pub async fn read(&self) -> Result<Vec<u8>, NotAvailable> {
        match self.0.event().await? {
            Event::Realtime(event) => Ok(event),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// A workspace control session; it ends when its last clone is dropped.
#[derive(Clone)]
pub struct WorkspaceControlHandle(Feed);

impl WorkspaceControlHandle {
    /// Wait for the session's next event.
    pub async fn read(&self) -> Result<WorkspaceControlEvent, NotAvailable> {
        match self.0.event().await? {
            Event::WorkspaceControl(event) => Ok(event),
            other => Err(unexpected_event(&other)),
        }
    }

    /// Send the controlled device one input event.
    pub async fn send_input(&self, event: &WorkspaceControlInput) -> Result<(), NotAvailable> {
        let subscription = self.0.0.subscription.lock().await.id();
        match self
            .0
            .0
            .channel
            .client
            .call(Operation::ControlInput {
                subscription,
                input: event.clone(),
            })
            .await
            .map_err(failure)?
        {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }
}

/// The session's published resources on this connection: what its services
/// read and the actions that change them. Cheap to clone; the handle it
/// opens lives as long as the connection.
#[derive(Clone)]
pub struct ServicesHandle {
    client: Client,
    handle: Arc<tokio::sync::OnceCell<Handle>>,
}

impl ServicesHandle {
    /// The session's handle on its published resources, opened on first use.
    async fn handle(&self) -> Result<Handle, NotAvailable> {
        let handle = self
            .handle
            .get_or_try_init(|| async {
                match self
                    .client
                    .call(Operation::Services)
                    .await
                    .map_err(failure)?
                {
                    Reply::Opened(handle) => Ok(handle),
                    other => Err(unexpected(&other)),
                }
            })
            .await?;
        Ok(*handle)
    }

    /// What `resource` reads now; `None` when no service publishes it.
    pub async fn snapshot(&self, resource: &str) -> Result<Snapshot, NotAvailable> {
        let services = self.handle().await?;
        match self
            .client
            .call(Operation::Snapshot {
                services,
                resource: resource.to_owned(),
            })
            .await
            .map_err(failure)?
        {
            Reply::Snapshot { revision, reading } => Ok(Snapshot {
                revision,
                reading: reading.map(json_text).transpose()?,
            }),
            other => Err(unexpected(&other)),
        }
    }

    /// `resource`'s reading now, and each time it changes.
    pub async fn watch(&self, resource: &str) -> Result<ReadingStream, NotAvailable> {
        let services = self.handle().await?;
        let subscription = self
            .client
            .watch(services, resource.to_owned())
            .await
            .map_err(failure)?;
        Ok(ReadingStream {
            subscription: Arc::new(tokio::sync::Mutex::new(subscription)),
        })
    }

    /// Invoke `action` of `resource` with `arguments` (none for an action
    /// that takes nothing).
    ///
    /// # Errors
    ///
    /// The host refuses the call or the connection fails. A refusal by the
    /// API is a [`Receipt::Refused`], not an error: the action exists and
    /// this is what it said.
    pub async fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Receipt, NotAvailable> {
        let services = self.handle().await?;
        let encoded = serde_json::to_vec(arguments)
            .map_err(|error| refused(format!("the action's arguments do not encode: {error}")))?;
        match self
            .client
            .call(Operation::Invoke {
                services,
                resource: resource.to_owned(),
                action: action.to_owned(),
                arguments: encoded,
            })
            .await
            .map_err(failure)?
        {
            Reply::Receipt(receipt) => Ok(receipt),
            other => Err(unexpected(&other)),
        }
    }
}

/// What a resource read, and the services' revision it was read at: equal
/// revisions publish the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: u64,
    pub reading: Option<String>,
}

/// One change of a watched resource's reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub resource: String,
    pub revision: u64,
    /// The reading as JSON; `None` once no service publishes it.
    pub reading: Option<String>,
}

/// A watched resource's readings, as it stands and then on each change.
#[derive(Clone)]
pub struct ReadingStream {
    subscription: Arc<tokio::sync::Mutex<Subscription>>,
}

impl ReadingStream {
    /// The next reading.
    pub async fn read(&self) -> Result<Reading, NotAvailable> {
        let event = self
            .subscription
            .lock()
            .await
            .event()
            .await
            .map_err(failure)?;
        match event {
            Event::Reading {
                resource,
                revision,
                reading,
            } => Ok(Reading {
                resource,
                revision,
                reading: reading.map(json_text).transpose()?,
            }),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// A reading as the text its JSON is.
fn json_text(bytes: Vec<u8>) -> Result<String, NotAvailable> {
    String::from_utf8(bytes)
        .map_err(|error| refused(format!("a reading is not JSON text: {error}")))
}

/// The channel's presence: what this endpoint shows, and everyone else's.
#[derive(Clone)]
pub struct PresenceHandle {
    channel: RecordHandle,
}
impl PresenceHandle {
    async fn publish(
        &self,
        key_bytes: &[u8],
        payload: collab_types::PeerPresencePayload,
    ) -> Result<(), NotAvailable> {
        self.channel
            .publish(Publication::Presence(collab_types::PeerPresence {
                key: key_bytes.to_vec(),
                payload,
            }))
            .await
    }

    /// Publish a pointer position under `key_bytes`.
    pub async fn publish_pointer(
        &self,
        key_bytes: &[u8],
        x: f32,
        y: f32,
    ) -> Result<(), NotAvailable> {
        self.publish(
            key_bytes,
            collab_types::PeerPresencePayload::Pointer { x, y },
        )
        .await
    }

    /// Publish a caret position under `key_bytes`.
    pub async fn publish_caret(&self, key_bytes: &[u8], pos: u64) -> Result<(), NotAvailable> {
        self.publish(key_bytes, collab_types::PeerPresencePayload::Caret { pos })
            .await
    }

    /// Publish a selection under `key_bytes`.
    pub async fn publish_selection(
        &self,
        key_bytes: &[u8],
        start: u64,
        end: u64,
    ) -> Result<(), NotAvailable> {
        self.publish(
            key_bytes,
            collab_types::PeerPresencePayload::Selection { start, end },
        )
        .await
    }

    /// Publish a typing span under `key_bytes`.
    pub async fn publish_typing(
        &self,
        key_bytes: &[u8],
        start: u64,
        end: u64,
    ) -> Result<(), NotAvailable> {
        self.publish(
            key_bytes,
            collab_types::PeerPresencePayload::Typing { start, end },
        )
        .await
    }

    /// Publish an action's interaction lifecycle, as JSON.
    pub async fn publish_interaction(
        &self,
        key_bytes: &[u8],
        interaction_json: &str,
    ) -> Result<(), NotAvailable> {
        self.publish(
            key_bytes,
            collab_types::PeerPresencePayload::InteractionJson(interaction_json.to_owned()),
        )
        .await
    }

    /// Publish a short status; empty text clears it.
    pub async fn publish_status(
        &self,
        key_bytes: &[u8],
        text: &str,
        automatic: bool,
    ) -> Result<(), NotAvailable> {
        self.publish(
            key_bytes,
            collab_types::PeerPresencePayload::Status {
                text: text.to_owned(),
                automatic,
            },
        )
        .await
    }

    /// Whether the local member is composing a message under `key_bytes` (a channel, a
    /// direct conversation): a draft is being typed, never what it says. `false` says they
    /// stopped, so no reader is left "typing" after the window closes.
    pub async fn publish_composing(
        &self,
        key_bytes: &[u8],
        composing: bool,
    ) -> Result<(), NotAvailable> {
        self.publish(
            key_bytes,
            collab_types::PeerPresencePayload::Composing { composing },
        )
        .await
    }

    /// Say `key_bytes` arrived.
    pub async fn publish_arrived(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        self.publish(key_bytes, collab_types::PeerPresencePayload::Arrived)
            .await
    }

    /// Say `key_bytes` left.
    pub async fn publish_left(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        self.publish(key_bytes, collab_types::PeerPresencePayload::Left)
            .await
    }

    /// Everyone else's presence.
    pub async fn subscribe_all(&self) -> Result<PresenceStream, NotAvailable> {
        Ok(PresenceStream(self.channel.feed(Topic::Presence).await?))
    }
}

/// Everyone else's presence on a channel.
#[derive(Clone)]
pub struct PresenceStream(Feed);

impl PresenceStream {
    /// The next presence change; a payload no app receives is skipped.
    pub async fn read(&self) -> Result<PresenceEvent, NotAvailable> {
        use collab_types::PeerPresencePayload as P;
        loop {
            let Event::Presence { member, presence } = self.0.event().await? else {
                continue;
            };
            let payload = match presence.payload {
                P::Pointer { x, y } => PresencePayload::Pointer { x, y },
                P::Caret { pos } => PresencePayload::Caret { pos },
                P::Selection { start, end } => PresencePayload::Selection { start, end },
                P::Typing { start, end } => PresencePayload::Typing { start, end },
                P::Arrived => PresencePayload::Arrived,
                P::Left => PresencePayload::Left,
                P::InteractionJson(json) => PresencePayload::InteractionJson(json),
                P::Status { text, automatic } => PresencePayload::Status { text, automatic },
                P::Composing { composing } => PresencePayload::Composing { composing },
                _ => continue,
            };
            return Ok(PresenceEvent {
                member,
                key_bytes: presence.key,
                payload,
            });
        }
    }
}

/// Changes of a channel's context: who is in it, what this endpoint may do
/// there, and the session's directed asks of it.
#[derive(Clone)]
pub struct ContextStream(Feed);

impl ContextStream {
    /// The next change.
    pub async fn read(&self) -> Result<ChannelContext, NotAvailable> {
        let update = match self.0.event().await? {
            Event::Context(update) => update,
            other => return Err(unexpected_event(&other)),
        };
        let mut context = ChannelContext::default();
        match update {
            ContextUpdate::Snapshot {
                members,
                permissions,
                apps,
            } => {
                context.members = members;
                context.permissions = permissions;
                context.apps = apps;
            }
            ContextUpdate::FollowChanged { peer_id } => context.follow_changed = Some(peer_id),
            ContextUpdate::Handoff(handoff) => context.handoff = Some(handoff),
            ContextUpdate::ResourcesArrived(records) => context.resources_arrived = records,
            ContextUpdate::RecordsMoved(records) => context.records_moved = records,
            ContextUpdate::RecordChanged => context.record_changed = true,
        }
        Ok(context)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A client wired to a host the test drives frame by frame.

    use super::CollabClient;
    use crate::wire::{Call, FRAMING, HostFrame};

    /// A client, and the host end of its connection.
    pub(crate) struct Host {
        pub(crate) reader: tokio::io::ReadHalf<tokio::io::DuplexStream>,
        pub(crate) writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    }

    impl Host {
        pub(crate) async fn call(&mut self) -> Call {
            FRAMING
                .read_value_async(&mut self.reader)
                .await
                .unwrap()
                .unwrap()
        }

        pub(crate) async fn send(&mut self, frame: &HostFrame) {
            use tokio::io::AsyncWriteExt as _;
            FRAMING
                .write_value_async(&mut self.writer, frame)
                .await
                .unwrap();
            self.writer.flush().await.unwrap();
        }
    }

    pub(crate) fn connected() -> (CollabClient, Host) {
        let (near, far) = tokio::io::duplex(1 << 20);
        let (reader, writer) = tokio::io::split(near);
        let (client, carry) = CollabClient::over(reader, writer);
        tokio::spawn(carry);
        let (reader, writer) = tokio::io::split(far);
        (client, Host { reader, writer })
    }
}

/// The person's shell's connection to the daemon's dispatch (`crate::wire::shell`): the device's
/// documents, its actions, and how its workspaces stand, asked as the person its account is
/// signed in as. Cheap to clone.
#[cfg(unix)]
#[derive(Clone)]
pub struct ShellClient {
    client: CollabClient,
    /// The descriptors the daemon hands the shell on its link: the open objects a call of the
    /// seat names (what Files selected for the clipboard), each by its number on the link.
    handed: runtime_portal::descriptors::ReceivedDescriptors,
    /// The runtime the link's frames are carried on, which its dispatch's waits run on too:
    /// whoever holds the client reaches the daemon from any thread, in a runtime or not.
    runtime: tokio::runtime::Handle,
}

/// The most descriptors the daemon hands the shell that no call took yet: one clipboard command's
/// selection, and as many again for one more on its way.
#[cfg(unix)]
const MAX_HANDED_TO_SHELL: usize = 2 * (crate::file_clipboard::MAX_ENTRIES + 1);

#[cfg(unix)]
impl ShellClient {
    /// Open the shell's connection over `link`, the shell link the daemon made and handed this
    /// process ([`Self::inherited`]), and what resolves once it ended. Its frames are carried by a
    /// task spawned on the current runtime.
    #[must_use]
    pub fn open(link: tokio::net::UnixStream) -> (Self, ConnectionEnd) {
        // The link carries descriptors to the daemon too: the connections of the apps the shell
        // launched, which it hands over as it vouches for them. And from it: what a call of the
        // seat names, which the daemon hands over as it forwards the call.
        let descriptors = runtime_portal::descriptors::DescriptorSender::new();
        let handed = runtime_portal::descriptors::ReceivedDescriptors::new(MAX_HANDED_TO_SHELL);
        let (client, end) =
            CollabClient::connected(runtime_portal::descriptors::DescriptorStream::duplex(
                link,
                descriptors.clone(),
                handed.clone(),
            ));
        (
            Self {
                client: client.with_descriptors(descriptors),
                handed,
                runtime: tokio::runtime::Handle::current(),
            },
            end,
        )
    }

    /// The descriptors the daemon handed over on this link, by their numbers on it: a call that
    /// names one takes it.
    #[must_use]
    pub const fn handed(&self) -> &runtime_portal::descriptors::ReceivedDescriptors {
        &self.handed
    }

    /// The runtime this link's frames are carried on.
    #[must_use]
    pub const fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    /// Hand the daemon `connection`, made on this shell's app socket, to serve: as the app `app`
    /// names when the shell attested it (its `process` and `launcher` numbers are filled here
    /// from `pinned`, the pidfds of the app's process and of its launcher's when the launcher
    /// handed the app over), or as a client admitted by what it presents. From here on the
    /// connection speaks to the daemon directly.
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon refused the vouch.
    pub async fn vouch(
        &self,
        connection: std::os::fd::OwnedFd,
        app: Option<(
            crate::wire::shell::VouchedApp,
            std::os::fd::OwnedFd,
            Option<std::os::fd::OwnedFd>,
        )>,
    ) -> Result<(), NotAvailable> {
        self.hand(crate::wire::shell::HandedTo::Collaboration, connection, app)
            .await
    }

    /// Hand the daemon's identity service `connection`, made on this shell's socket for it, to
    /// serve as a client that presents itself.
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon refused the connection.
    pub async fn hand_identity(
        &self,
        connection: std::os::fd::OwnedFd,
    ) -> Result<(), NotAvailable> {
        self.hand(crate::wire::shell::HandedTo::Identity, connection, None)
            .await
    }

    async fn hand(
        &self,
        to: crate::wire::shell::HandedTo,
        connection: std::os::fd::OwnedFd,
        app: Option<(
            crate::wire::shell::VouchedApp,
            std::os::fd::OwnedFd,
            Option<std::os::fd::OwnedFd>,
        )>,
    ) -> Result<(), NotAvailable> {
        let sender = self.client.descriptors.as_ref().ok_or_else(|| {
            NotAvailable::Refused("this shell link carries no descriptors".to_owned())
        })?;
        let connection = sender.send(connection)?;
        let app = match app {
            Some((mut app, process, launcher)) => {
                app.process = sender.send(process)?;
                app.launcher = launcher.map(|launcher| sender.send(launcher)).transpose()?;
                Some(app)
            }
            None => None,
        };
        let vouch = crate::wire::shell::Vouch {
            connection,
            app,
            to,
        };
        match self
            .call(crate::wire::shell::ShellCall::Vouch(vouch))
            .await?
        {
            crate::wire::shell::ShellReply::Vouched => Ok(()),
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered a vouch with {other:?}"
            ))),
        }
    }

    /// A packet socket to the device's tilolang processes, for one ask
    /// (`runtime_portal::device_processes`): admitted on the shell's own link, then opened on the
    /// daemon's socket with the secret it answered, which carries the daemon's end of the pair.
    ///
    /// # Errors
    /// The daemon refused it, or its socket does not answer.
    #[cfg(target_os = "linux")]
    pub async fn processes(&self) -> Result<std::os::fd::OwnedFd, NotAvailable> {
        let (socket, secret) = match self.call(crate::wire::shell::ShellCall::Processes).await? {
            crate::wire::shell::ShellReply::Processes { socket, secret } => (socket, secret),
            other => {
                return Err(NotAvailable::Refused(format!(
                    "the daemon answered a hand-over with {other:?}"
                )));
            }
        };
        let stream = tokio::net::UnixStream::connect(&socket).await?;
        let (ours, theirs) = runtime_portal::packet::pair()?;
        let mut opening = crate::wire::shell::PROCESSES_MAGIC.to_vec();
        opening.extend_from_slice(&secret);
        // The marker, the secret and the daemon's end in one message: the daemon reads them as
        // one, so the descriptor arrives with the bytes it reads.
        loop {
            stream.writable().await?;
            match stream.try_io(tokio::io::Interest::WRITABLE, || {
                runtime_portal::fd_transport::send_with_fd(&stream, &opening, &theirs)
            }) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                sent => break sent?,
            }
        }
        Ok(ours)
    }

    /// The shell link the daemon handed this process when it started it
    /// ([`crate::wire::shell::SHELL_FD_ENV`]), taken once; `None` in a process the daemon did not
    /// start as the shell.
    ///
    /// # Errors
    /// The handed descriptor is no socket this process can use.
    pub fn inherited() -> Option<std::io::Result<std::os::unix::net::UnixStream>> {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd: std::os::fd::RawFd = std::env::var(crate::wire::shell::SHELL_FD_ENV)
            .ok()?
            .parse()
            .ok()?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the daemon handed this descriptor to the process for the shell link alone, and
        // the flag above lets it be owned exactly once.
        let link =
            unsafe { <std::os::unix::net::UnixStream as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        // It was handed down for this process's exec alone: no program the shell starts keeps it.
        let taken = rustix::io::fcntl_setfd(&link, rustix::io::FdFlags::CLOEXEC)
            .map_err(std::io::Error::from)
            .and_then(|()| link.set_nonblocking(true));
        Some(taken.map(|()| link))
    }

    /// Ask the device `call`.
    ///
    /// # Errors
    /// The daemon refused it, or the connection failed.
    pub async fn call(
        &self,
        call: crate::wire::shell::ShellCall,
    ) -> Result<crate::wire::shell::ShellReply, NotAvailable> {
        match self
            .client
            .client
            .call(Operation::Shell(call))
            .await
            .map_err(failure)?
        {
            Reply::Shell(reply) => Ok(reply),
            other => Err(unexpected(&other)),
        }
    }

    /// How the person's workspaces stand: the resources of
    /// `tilo_workspace_service::workspace_view`, read and watched.
    #[must_use]
    pub fn standing(&self) -> ServicesHandle {
        self.client.services()
    }
}
