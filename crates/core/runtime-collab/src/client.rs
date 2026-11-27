//! The typed endpoint of the collaboration wire: a connection to the host
//! ([`CollabClient`]), the records its calls are about ([`RecordHandle`]) and what
//! they deliver. Every call is one [`wire::Operation`] on the connection the
//! launcher handed this process; nothing here keeps state the host does not
//! name by a handle or a subscription.

use std::sync::Arc;

use runtime_portal::NotAvailable;
#[cfg(unix)]
use tilo_actions::remote::{ActionProvider, CataloguePage};

use crate::wire::{
    Changes, ContextUpdate, Event, Handle, InvocationCall, LiveActivityCall, Operation,
    Publication, Read, Receipt, Reply, Report, Service, SettingsCall, Topic,
    client::{Answerer, Client, Failure, Subscription},
};
use crate::{ChannelContext, Invocation};
use crate::{
    ExportedAction, SurfaceDelta, WorkspaceControlEvent, WorkspaceControlInput, committed_document,
    file_clipboard,
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
type OpenRecords = std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<Channel>>>;

/// The most a resource command or its reply takes on the wire, as the store bounds its largest
/// (an app's whole documents), under the connection's frame.
const MAX_RESOURCE_WIRE_BYTES: usize = 56 << 20;

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

    /// Whether the workspace seals what it keeps of this record. Asked first, it is where the
    /// host admits this connection to the record, or refuses it with its reason.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host refuses the record or the connection fails.
    pub async fn sealed(&self) -> Result<bool, NotAvailable> {
        match self
            .committed_document(&committed_document::Request::Writes)
            .await?
        {
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

    /// Hold open what the host says of the `actions` a window shows: all of them now, then all
    /// again whenever grants change one, until the stream is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host refuses or the connection fails.
    pub async fn shown_actions(
        &self,
        actions: Vec<tilo_permissions::shown_actions::ShownAction>,
    ) -> Result<ActionVerdictFeed, NotAvailable> {
        Ok(ActionVerdictFeed(
            self.feed(Topic::ShownActions(actions)).await?,
        ))
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

    /// Subscribe to lifecycle edges addressed to this exact application.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
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

/// What the host says of the actions a [`RecordHandle::shown_actions`] names; dropping it ends
/// the subscription.
pub struct ActionVerdictFeed(Feed);

impl ActionVerdictFeed {
    /// The decisions as they are next.
    pub async fn read(
        &self,
    ) -> Result<Vec<tilo_permissions::shown_actions::ActionDecision>, NotAvailable> {
        match self.0.event().await? {
            Event::ShownActions(decisions) => Ok(decisions),
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
            ContextUpdate::Snapshot { permissions, apps } => {
                context.permissions = permissions;
                context.apps = apps;
            }
            ContextUpdate::FollowChanged { peer_id } => context.follow_changed = Some(peer_id),
            ContextUpdate::Handoff(handoff) => context.handoff = Some(handoff),
            ContextUpdate::ResourcesArrived(records) => context.resources_arrived = records,
            ContextUpdate::RecordsMoved(records) => context.records_moved = records,
            ContextUpdate::RecordChanged => context.record_changed = true,
            ContextUpdate::Heard { member, frame } => context.heard = Some((member, *frame)),
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
    /// The link as it stands: a daemon that restarts makes a new one, and every holder of this
    /// client reaches the daemon on the new link from then on.
    shared: Arc<std::sync::RwLock<ShellShared>>,
    /// The providers this shell served on its link, each the shell's own: a second call cannot
    /// take one over, and what it offers changes through the handle it has. Each is offered again
    /// on a new link.
    pub(crate) served: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Served>>>,
    /// What the link is doing, and what asks for it back: the socket to redial, and what wakes
    /// the session when the link moved.
    link: Arc<LinkWatch>,
}

/// The connection a shell reaches its daemon on, and what came with it.
#[cfg(unix)]
struct ShellShared {
    client: CollabClient,
    /// The descriptors the daemon hands the shell on this link: the open objects a call of the
    /// seat names (what Files selected for the clipboard), each by its number on the link.
    handed: runtime_portal::descriptors::ReceivedDescriptors,
    /// The runtime the link's frames are carried on, which its dispatch's waits run on too:
    /// whoever holds the client reaches the daemon from any thread, in a runtime or not.
    runtime: tokio::runtime::Handle,
    /// Which link this is: what a follower watches to read its subscriptions again.
    generation: u64,
}

/// A provider the shell serves, kept so it can be offered again on a new link.
#[cfg(unix)]
pub(crate) struct Served {
    catalogue: CataloguePage,
    answers: Arc<dyn ActionProvider + Send + Sync>,
    /// How `answers` answers a request: the serving process's own ([`ShellClient::provide`]).
    serve: tilo_actions::remote::Serve,
    handle: Option<Handle>,
}

/// Whether the shell's link reaches its daemon now.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellLink {
    /// The daemon answers on the link.
    Linked,
    /// The link ended and the daemon is asked for a new one: what a person is told while the
    /// desktop keeps showing what it has.
    Reconnecting,
    /// The daemon did not answer the last ask for a link, and is asked again: the last refusal.
    Unreachable {
        /// What the ask was refused with.
        why: String,
    },
}

/// What the shell redials, and what to wake when the link moved.
#[cfg(unix)]
struct LinkWatch {
    status: std::sync::Mutex<ShellLink>,
    /// Each new link's generation: what follows subscriptions reads them again.
    generation: tokio::sync::watch::Sender<u64>,
    socket: std::sync::Mutex<Option<std::path::PathBuf>>,
    wake: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Whether the shell is done with its link for good: it asks its daemon for no other.
    stopped: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl LinkWatch {
    fn fresh() -> Self {
        Self {
            status: std::sync::Mutex::new(ShellLink::Linked),
            generation: tokio::sync::watch::channel(0).0,
            socket: std::sync::Mutex::default(),
            wake: std::sync::Mutex::default(),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The shell is done with its link: the follower asks for no other.
    fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Acquire)
    }

    fn set(&self, status: ShellLink) {
        let moved = {
            let mut held = self
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *held == status {
                false
            } else {
                *held = status;
                true
            }
        };
        if moved {
            self.wake_now();
        }
    }

    fn wake_now(&self) {
        let wake = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// How long the shell waits before it asks a daemon that did not answer again, and the longest it
/// waits: it never gives up on its own daemon.
#[cfg(unix)]
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(unix)]
const ASK_AGAIN_MOST: std::time::Duration = std::time::Duration::from_secs(5);

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
        let (shared, end) = opened(link);
        (
            Self {
                shared: Arc::new(std::sync::RwLock::new(shared)),
                served: Arc::default(),
                link: Arc::new(LinkWatch::fresh()),
            },
            end,
        )
    }

    /// Ask `socket`, this device's daemon, for a new link for this seat whenever the link ends,
    /// until this client is dropped: what keeps a desktop working while its daemon restarts.
    /// `wake` is called when the link moved, so the session shows the wait and hides it.
    pub fn following(
        &self,
        end: ConnectionEnd,
        socket: std::path::PathBuf,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self
            .link
            .socket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(socket);
        *self
            .link
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = wake;
        let shared = Arc::clone(&self.shared);
        let served = Arc::clone(&self.served);
        let watch = Arc::clone(&self.link);
        self.runtime().spawn(relinking(shared, served, watch, end));
    }

    /// End this shell's link: the daemon lets go of the seat it held for it, and no follower of
    /// this client asks for another. What a session does as it ends, so the session that starts on
    /// the same seat next finds it free, however many holders of this client are still about.
    pub fn end(&self) {
        self.link.stop();
        self.current().wire().hang_up();
    }

    /// The descriptors the daemon handed over on this link, by their numbers on it: a call that
    /// names one takes it.
    #[must_use]
    pub fn handed(&self) -> runtime_portal::descriptors::ReceivedDescriptors {
        self.shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .handed
            .clone()
    }

    /// The runtime this link's frames are carried on.
    #[must_use]
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .runtime
            .clone()
    }

    /// Which link this is: a follower that sees another reads its subscriptions again.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }

    /// Watch for a new link: reading the value says which link stands now.
    #[must_use]
    pub fn linking(&self) -> tokio::sync::watch::Receiver<u64> {
        self.link.generation.subscribe()
    }

    /// Whether the link reaches the daemon now, or what it is waiting for.
    #[must_use]
    pub fn link_status(&self) -> ShellLink {
        self.link
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The connection as it stands: what a call made now travels on.
    fn current(&self) -> CollabClient {
        self.shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .client
            .clone()
    }

    /// Hand the daemon `connection`, made on this shell's app socket, to serve: as the launch
    /// `app` names by its ticket when the shell attested it (its `process` number is filled here
    /// from the pidfd of the connecting process), or as a client admitted by what it presents.
    /// From here on the connection speaks to the daemon directly.
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon refused the vouch.
    pub async fn vouch(
        &self,
        connection: std::os::fd::OwnedFd,
        app: Option<(crate::wire::shell::VouchedApp, std::os::fd::OwnedFd)>,
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

    /// Hand the daemon `connection`, which `program`, one of the person's own programs the person
    /// said yes to, made at this login's API socket, to serve as Tilo's API for them
    /// ([`crate::wire::shell::HandedTo::Api`]).
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon refused the connection.
    pub async fn hand_api(
        &self,
        connection: std::os::fd::OwnedFd,
        program: crate::wire::shell::VouchedProgram,
    ) -> Result<(), NotAvailable> {
        self.hand_from(
            crate::wire::shell::HandedTo::Api,
            connection,
            None,
            Some(program),
        )
        .await
    }

    /// A connection the daemon carries as one link of this shell's live session
    /// (`tilo_live::link`): one end of a pair made here, the other handed over on this link.
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon runs no live session.
    pub async fn live(&self) -> Result<tokio::net::UnixStream, NotAvailable> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        self.hand(
            crate::wire::shell::HandedTo::Live,
            std::os::fd::OwnedFd::from(theirs),
            None,
        )
        .await?;
        ours.set_nonblocking(true)?;
        Ok(tokio::net::UnixStream::from_std(ours)?)
    }

    /// A connection the daemon carries as one link of this shell's device VPN
    /// (`device_vpn::link`): one end of a pair made here, the other handed over on this link.
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon serves no device VPN.
    pub async fn device_vpn(&self) -> Result<tokio::net::UnixStream, NotAvailable> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        self.hand(
            crate::wire::shell::HandedTo::DeviceVpn,
            std::os::fd::OwnedFd::from(theirs),
            None,
        )
        .await?;
        ours.set_nonblocking(true)?;
        Ok(tokio::net::UnixStream::from_std(ours)?)
    }

    /// A connection the daemon carries as one link of the person's speakers and remote audio
    /// ([`tilo_session_client::speakers`]): one end of a pair made here, the other handed over on
    /// this link.
    ///
    /// # Errors
    /// The link carries no descriptors, or the daemon serves no speakers link.
    pub async fn speakers(&self) -> Result<tokio::net::UnixStream, NotAvailable> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        self.hand(
            crate::wire::shell::HandedTo::Speakers,
            std::os::fd::OwnedFd::from(theirs),
            None,
        )
        .await?;
        ours.set_nonblocking(true)?;
        Ok(tokio::net::UnixStream::from_std(ours)?)
    }

    async fn hand(
        &self,
        to: crate::wire::shell::HandedTo,
        connection: std::os::fd::OwnedFd,
        app: Option<(crate::wire::shell::VouchedApp, std::os::fd::OwnedFd)>,
    ) -> Result<(), NotAvailable> {
        self.hand_from(to, connection, app, None).await
    }

    /// [`Self::hand`], naming the program that made `connection` where the shell knows it.
    async fn hand_from(
        &self,
        to: crate::wire::shell::HandedTo,
        connection: std::os::fd::OwnedFd,
        app: Option<(crate::wire::shell::VouchedApp, std::os::fd::OwnedFd)>,
        program: Option<crate::wire::shell::VouchedProgram>,
    ) -> Result<(), NotAvailable> {
        let connection_client = self.current();
        let sender = connection_client.descriptors.as_ref().ok_or_else(|| {
            NotAvailable::Refused("this shell link carries no descriptors".to_owned())
        })?;
        let connection = sender.send(connection)?;
        let app = match app {
            Some((mut app, process)) => {
                app.process = sender.send(process)?;
                Some(app)
            }
            None => None,
        };
        let vouch = crate::wire::shell::Vouch {
            connection,
            app,
            to,
            program,
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

    /// Ask the daemon to launch `ask` as this shell's person: the order this shell starts it by,
    /// once the daemon decided and recorded it.
    ///
    /// # Errors
    /// The daemon refused the launch (not installed, a workspace it does not serve, a kind that
    /// may not launch), or the link is down.
    pub async fn launch(
        &self,
        ask: crate::wire::shell::LaunchAsk,
    ) -> Result<crate::wire::shell::LaunchOrder, NotAvailable> {
        match self
            .call(crate::wire::shell::ShellCall::Launch(ask))
            .await?
        {
            crate::wire::shell::ShellReply::Launching(answer) => {
                answer.map_err(NotAvailable::Refused)
            }
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered a launch with {other:?}"
            ))),
        }
    }

    /// The launches the daemon ordered this shell to start, waited for until there is one.
    ///
    /// # Errors
    /// The link is down.
    pub async fn orders(&self) -> Result<Vec<crate::wire::shell::LaunchOrder>, NotAvailable> {
        match self.call(crate::wire::shell::ShellCall::Orders).await? {
            crate::wire::shell::ShellReply::Orders(orders) => Ok(orders),
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered a follow of its orders with {other:?}"
            ))),
        }
    }

    /// Tell the daemon the ordered launch `ticket` runs as `pid`, pinned by `process` (its pidfd),
    /// held at its launch hold: answered once the daemon bound the process to the launch, after
    /// which the shell lets it go on.
    ///
    /// # Errors
    /// The daemon refused the process (no such order, one already started), or the link is down.
    pub async fn started(
        &self,
        ticket: u64,
        pid: u32,
        process: std::os::fd::OwnedFd,
    ) -> Result<(), NotAvailable> {
        let connection_client = self.current();
        let sender = connection_client.descriptors.as_ref().ok_or_else(|| {
            NotAvailable::Refused("this shell link carries no descriptors".to_owned())
        })?;
        let process = sender.send(process)?;
        match self
            .call(crate::wire::shell::ShellCall::Started {
                ticket,
                pid,
                process,
            })
            .await?
        {
            crate::wire::shell::ShellReply::Started(answer) => {
                answer.map_err(NotAvailable::Refused)
            }
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered a started launch with {other:?}"
            ))),
        }
    }

    /// Tell the daemon what became of the launch `ticket` this shell started.
    ///
    /// # Errors
    /// The daemon knows no such launch, or the link is down.
    pub async fn launch_event(
        &self,
        ticket: u64,
        event: crate::wire::shell::LaunchEvent,
    ) -> Result<(), NotAvailable> {
        match self
            .call(crate::wire::shell::ShellCall::LaunchEvent { ticket, event })
            .await?
        {
            crate::wire::shell::ShellReply::Noted(answer) => answer.map_err(NotAvailable::Refused),
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered a launch's event with {other:?}"
            ))),
        }
    }

    /// Bind the process `process` pins (a pidfd), which `adopt` describes, to a launch the daemon
    /// decides now: its ticket.
    ///
    /// # Errors
    /// The daemon refused the launch, or the link is down.
    pub async fn adopt(
        &self,
        mut adopt: crate::wire::shell::Adopt,
        process: std::os::fd::OwnedFd,
    ) -> Result<u64, NotAvailable> {
        let connection_client = self.current();
        let sender = connection_client.descriptors.as_ref().ok_or_else(|| {
            NotAvailable::Refused("this shell link carries no descriptors".to_owned())
        })?;
        adopt.process = sender.send(process)?;
        match self
            .call(crate::wire::shell::ShellCall::Adopt(adopt))
            .await?
        {
            crate::wire::shell::ShellReply::Adopted(answer) => {
                answer.map_err(NotAvailable::Refused)
            }
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered an adopted launch with {other:?}"
            ))),
        }
    }

    /// A packet socket to the device's tilolang processes, for one ask
    /// (`runtime_portal::device_processes`): admitted on the shell's own link, then handed over on
    /// the daemon's native socket with the secret it answered ([`hand_processes`]).
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
        tokio::task::spawn_blocking(move || hand_processes(&socket, &secret))
            .await
            .map_err(|error| NotAvailable::Refused(format!("the hand-over did not run: {error}")))?
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
            .current()
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
        self.current().services()
    }
}

/// The connection over `link`, and what came with it: a shell's link as one link of its life.
#[cfg(unix)]
fn opened(link: tokio::net::UnixStream) -> (ShellShared, ConnectionEnd) {
    // The link carries descriptors to the daemon too: the connections of the apps the shell
    // launched, which it hands over as it vouches for them. And from it: what a call of the seat
    // names, which the daemon hands over as it forwards the call.
    let descriptors = runtime_portal::descriptors::DescriptorSender::new();
    let handed = runtime_portal::descriptors::ReceivedDescriptors::new(MAX_HANDED_TO_SHELL);
    let (client, end) =
        CollabClient::connected(runtime_portal::descriptors::DescriptorStream::duplex(
            link,
            descriptors.clone(),
            handed.clone(),
        ));
    (
        ShellShared {
            client: client.with_descriptors(descriptors),
            handed,
            runtime: tokio::runtime::Handle::current(),
            generation: 0,
        },
        end,
    )
}

/// Hand the device's tilolang processes one end of a packet socket pair on the daemon's native
/// socket at `socket`, with `secret`, the one-use secret the daemon answered this shell's
/// [`crate::wire::shell::ShellCall::Processes`] with ([`crate::wire::shell::SHELL_PROCESSES`]),
/// and answer the other end. The daemon admits the hand-over by the account the kernel names for
/// this connection and by the secret.
///
/// # Errors
/// The daemon's socket does not answer, or the daemon refused the hand-over.
#[cfg(target_os = "linux")]
pub fn hand_processes(
    socket: &std::path::Path,
    secret: &[u8; crate::wire::shell::SHELL_SECRET_BYTES],
) -> Result<std::os::fd::OwnedFd, NotAvailable> {
    use std::io::BufRead as _;
    let refused = |what: String| {
        NotAvailable::Refused(format!(
            "the device's processes at {}: {what}",
            socket.display()
        ))
    };
    let stream = tilo_socket::connect(socket)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    let (ours, theirs) = runtime_portal::packet::pair()?;
    let secret = secret.iter().fold(String::new(), |mut hex, byte| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    let request = serde_json::json!({
        "native": crate::wire::shell::SHELL_PROCESSES,
        "secret": secret,
    })
    .to_string()
        + "\n";
    runtime_portal::fd_transport::send_with_fd(&stream, request.as_bytes(), &theirs)?;
    drop(theirs);
    let mut answer = String::new();
    std::io::BufReader::new(&stream).read_line(&mut answer)?;
    let answer: serde_json::Value = serde_json::from_str(&answer)
        .map_err(|error| refused(format!("an answer that does not read: {error}")))?;
    if let Some(why) = answer.get("error").and_then(serde_json::Value::as_str) {
        return Err(refused(format!("the hand-over was refused: {why}")));
    }
    if answer.get("handed").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(refused(format!("no hand-over: {answer}")));
    }
    Ok(ours)
}

/// Ask this device's daemon at `socket` for a shell link for the seat this process is, and take
/// the link it answers with: the one typed request that establishes a shell link, made on the
/// daemon's own socket. It is what the daemon started this shell with, and what a shell asks for
/// again when its link ends.
#[cfg(unix)]
fn request_link(socket: &std::path::Path) -> Result<std::os::unix::net::UnixStream, String> {
    use std::io::Write as _;
    let complain = |what: &str| format!("{what} at {}: ", socket.display());
    let mut stream = tilo_socket::connect(socket)
        .map_err(|error| format!("{}{error}", complain("this device's daemon")))?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(10)));
    let request = b"{\"native\":\"shells#link\"}\n";
    stream
        .write_all(request)
        .map_err(|error| format!("{}{error}", complain("this device's daemon")))?;
    let (bytes, descriptors) = runtime_portal::fd_transport::recv_with_fds(&stream)
        .map_err(|error| format!("{}{error}", complain("this device's daemon")))?;
    let answer: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}an answer that does not read: {error}", complain("")))?;
    if let Some(why) = answer.get("error").and_then(serde_json::Value::as_str) {
        return Err(format!("{}a shell link was refused: {why}", complain("")));
    }
    let Some(descriptor) = descriptors.into_iter().next() else {
        return Err(format!("{}no shell link: {answer}", complain("")));
    };
    let link = std::os::unix::net::UnixStream::from(descriptor);
    link.set_nonblocking(true)
        .map_err(|error| format!("{}{error}", complain("the shell link")))?;
    Ok(link)
}

/// Keep asking `watch`'s socket for a new link until one answers, then offer every provider this
/// shell serves on it, and wait for the next link to end. It never gives up: the daemon may come
/// back, and a desktop that waited is one that works when it does.
#[cfg(unix)]
async fn relinking(
    shared: Arc<std::sync::RwLock<ShellShared>>,
    served: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Served>>>,
    watch: Arc<LinkWatch>,
    mut end: ConnectionEnd,
) {
    loop {
        end.await;
        if watch.stopped() {
            return;
        }
        watch.set(ShellLink::Reconnecting);
        let mut wait = ASK_AGAIN;
        loop {
            if watch.stopped() {
                return;
            }
            let socket = watch
                .socket
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let Some(socket) = socket else {
                return;
            };
            let asked = tokio::task::spawn_blocking(move || request_link(&socket)).await;
            let link = match asked {
                Ok(Ok(link)) => link,
                Ok(Err(why)) => {
                    tracing::warn!(%why, "this device's daemon did not answer for its shell link");
                    watch.set(ShellLink::Unreachable { why });
                    tokio::time::sleep(wait).await;
                    wait = wait.saturating_mul(2).min(ASK_AGAIN_MOST);
                    continue;
                }
                Err(error) => {
                    let why = format!("the shell link's ask stopped: {error}");
                    watch.set(ShellLink::Unreachable { why });
                    tokio::time::sleep(wait).await;
                    wait = wait.saturating_mul(2).min(ASK_AGAIN_MOST);
                    continue;
                }
            };
            // The link this shell was done with while the ask was on its way: it is let go, and
            // the seat it would take stays free.
            if watch.stopped() {
                return;
            }
            let Ok(link) = tokio::net::UnixStream::from_std(link) else {
                watch.set(ShellLink::Unreachable {
                    why: "the shell link is no socket".to_owned(),
                });
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2).min(ASK_AGAIN_MOST);
                continue;
            };
            let (fresh, next_end) = opened(link);
            let generation = {
                let mut held = shared
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let generation = held.generation.wrapping_add(1);
                *held = ShellShared {
                    generation,
                    ..fresh
                };
                generation
            };
            // The shell was done with its link while this one was being made: it is let go
            // rather than held open, and the seat stays free for the shell that follows.
            if watch.stopped() {
                shared
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .client
                    .wire()
                    .hang_up();
                return;
            }
            replay(&shared, &served).await;
            let _ = watch.generation.send(generation);
            watch.set(ShellLink::Linked);
            end = next_end;
            break;
        }
    }
}

/// Offer every provider this shell serves on the link that stands now, each under a handle of the
/// new link: what the daemon knew of the old one went with it.
#[cfg(unix)]
async fn replay(
    shared: &Arc<std::sync::RwLock<ShellShared>>,
    served: &Arc<tokio::sync::Mutex<std::collections::HashMap<String, Served>>>,
) {
    let mut held = served.lock().await;
    for (provider, entry) in held.iter_mut() {
        let client = shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .client
            .clone();
        match shell_dispatch::offer_on(&client, provider, &entry.catalogue).await {
            Ok(handle) => {
                shell_dispatch::answer_provider(
                    &client,
                    handle,
                    Arc::clone(&entry.answers),
                    entry.serve,
                );
                entry.handle = Some(handle);
            }
            Err(error) => {
                tracing::warn!(%error, provider, "the daemon is handed this provider's steps no more");
            }
        }
    }
}
