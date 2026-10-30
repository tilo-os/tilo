//! The typed endpoint of the collaboration wire: a connection to the host
//! ([`CollabClient`]), the channels it joins ([`ChannelHandle`]) and what
//! they deliver. Every call is one [`wire::Operation`] on the connection the
//! launcher handed this process; nothing here keeps state the host does not
//! name by a handle or a subscription.

use std::sync::Arc;

use runtime_portal::NotAvailable;

use crate::wire::{
    Changes, ContextUpdate, Event, Handle, InvocationCall, Join, LiveActivityCall, Operation,
    Participant, Publication, Read, Reply, Report, Service, SettingsCall, TaskCall, TaskStep,
    Topic,
    client::{Answerer, Client, Failure, Subscription},
};
use crate::{
    ChannelContext, HistoryAgent, Invocation, OpEvent, PresenceEvent, ScrollbackBatch, TaskAction,
    TaskChildWait,
};
use crate::{
    ExportedAction, NearbyTransferCommand, NearbyTransferReply, PresencePayload, SpeakerCommand,
    SpeakerReply, SurfaceDelta, TaskRunEnvelope, TaskRunUpdate, WifiContinuityCommand,
    WifiContinuityReply, WorkspaceControlEvent, WorkspaceControlInput, audio_output,
    committed_document, file_clipboard, native_media, open_selected, private_draft,
};
use crate::{
    MAX_NEARBY_TRANSFER_WIRE_BYTES, MAX_SPEAKER_WIRE_BYTES, MAX_WIFI_CONTINUITY_WIRE_BYTES,
    TaskRunControl, TaskRunStatus,
};

/// Where a connection's calls hand the host descriptors, if it can.
#[cfg(unix)]
type Descriptors = Option<runtime_portal::descriptor_lane::DescriptorSender>;
#[cfg(not(unix))]
type Descriptors = Option<std::convert::Infallible>;

pub(crate) fn failure(failure: Failure) -> NotAvailable {
    match failure {
        Failure::Refused(reason) => NotAvailable::Refused(reason),
        Failure::Closed => NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
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
        let descriptors = runtime_portal::descriptor_lane::DescriptorSender::new();
        let lane = descriptors.clone();
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
                        runtime_portal::descriptor_lane::DescriptorStream::sending(stream, lane);
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
}

impl CollabClient {
    /// A client over an already connected wire client.
    #[must_use]
    pub const fn new(client: Client) -> Self {
        Self {
            client,
            descriptors: None,
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
    ///
    /// # Errors
    ///
    /// The socket cannot be handed to the runtime.
    #[cfg(unix)]
    pub fn from_connected_fd(
        fd: std::os::fd::OwnedFd,
    ) -> Result<(Self, ConnectionEnd), NotAvailable> {
        let socket = std::os::unix::net::UnixStream::from(fd);
        socket.set_nonblocking(true)?;
        Ok(Self::connected(tokio::net::UnixStream::from_std(socket)?))
    }

    /// The wire client underneath.
    #[must_use]
    pub const fn wire(&self) -> &Client {
        &self.client
    }

    /// Hand the host descriptors over `lane`, the descriptor lane of the
    /// very connection `self` calls over; see
    /// [`ChannelHandle::local_file_clipboard_command`].
    #[cfg(unix)]
    #[must_use]
    pub fn with_descriptors(
        mut self,
        lane: runtime_portal::descriptor_lane::DescriptorSender,
    ) -> Self {
        self.descriptors = Some(lane);
        self
    }

    /// [`Self::with_descriptors`] for a caller that only knows the lane
    /// [`runtime_portal::DescriptorStreamSource`] handed it: on a host
    /// without descriptors the lane is empty and the client stays as it was.
    #[must_use]
    pub fn with_optional_descriptors(mut self, lane: runtime_portal::DescriptorLane) -> Self {
        #[cfg(unix)]
        {
            self.descriptors = lane;
        }
        #[cfg(not(unix))]
        {
            let _ = lane;
        }
        self
    }

    /// Resolves once the connection has ended.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        self.client.closed()
    }

    /// Join a channel, presenting `identity` (an encoded
    /// [`crate::CertifiedJoinIdentity`]).
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the join or the
    /// connection fails.
    pub async fn join_channel(
        &self,
        channel_id: &str,
        identity: &[u8],
    ) -> Result<ChannelHandle, NotAvailable> {
        self.join_channel_as(channel_id, identity, "").await
    }

    /// Join a channel while announcing the participant's display handle.
    ///
    /// # Errors
    ///
    /// As [`Self::join_channel`].
    pub async fn join_channel_as(
        &self,
        channel_id: &str,
        identity: &[u8],
        participant_handle: &str,
    ) -> Result<ChannelHandle, NotAvailable> {
        self.join_channel_as_kind(channel_id, identity, participant_handle, "person", "")
            .await
    }

    /// Join as an explicitly typed participant (`person`, or `agent` with
    /// the agent's id as the handle). `record_kind` names the resource kind
    /// of the record the channel carries, or is empty.
    ///
    /// # Errors
    ///
    /// As [`Self::join_channel`].
    pub async fn join_channel_as_kind(
        &self,
        channel_id: &str,
        identity: &[u8],
        participant_handle: &str,
        participant_kind: &str,
        record_kind: &str,
    ) -> Result<ChannelHandle, NotAvailable> {
        let participant = match participant_kind {
            "" | "person" => Participant::Person {
                handle: participant_handle.to_owned(),
            },
            "agent" => Participant::Agent {
                id: participant_handle.to_owned(),
            },
            other => {
                return Err(refused(format!(
                    "unknown collaboration participant kind `{other}`"
                )));
            }
        };
        let identity = crate::CertifiedJoinIdentity::decode(identity)
            .map_err(|error| refused(format!("certified join presentation is malformed: {error}")))?
            .ok_or_else(|| {
                refused("collaboration join requires a certified device presentation")
            })?;
        let join = Join {
            channel: channel_id.to_owned(),
            identity,
            participant,
            record_kind: Some(record_kind.to_owned()).filter(|kind| !kind.is_empty()),
        };
        match self
            .client
            .call(Operation::Join(join))
            .await
            .map_err(failure)?
        {
            Reply::Opened(handle) => Ok(self.channel(handle)),
            other => Err(unexpected(&other)),
        }
    }

    /// The channel `handle` this connection holds.
    #[must_use]
    pub fn channel(&self, handle: Handle) -> ChannelHandle {
        ChannelHandle {
            channel: Arc::new(Channel {
                client: self.client.clone(),
                handle,
            }),
            descriptors: self.descriptors.clone(),
        }
    }

    /// Where the room `channel_id` names stands for apps on this device.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection fails.
    pub async fn room_status(&self, channel_id: &str) -> Result<crate::RoomStatus, NotAvailable> {
        self.room(channel_id, None).await
    }

    /// Where the room stands once it is no longer `seen`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection fails.
    pub async fn await_room(
        &self,
        channel_id: &str,
        seen: &crate::RoomStatus,
    ) -> Result<crate::RoomStatus, NotAvailable> {
        self.room(channel_id, Some(seen.clone())).await
    }

    async fn room(
        &self,
        channel_id: &str,
        seen: Option<crate::RoomStatus>,
    ) -> Result<crate::RoomStatus, NotAvailable> {
        match self
            .client
            .call(Operation::Room {
                channel: channel_id.to_owned(),
                seen,
            })
            .await
            .map_err(failure)?
        {
            Reply::Room(status) => Ok(status),
            other => Err(unexpected(&other)),
        }
    }
}

/// A joined channel; the host closes it once the last clone of its handle,
/// and every stream opened from it, is gone.
struct Channel {
    client: Client,
    handle: Handle,
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.client.close(self.handle);
    }
}

/// A channel this connection joined. Cheap to clone.
#[derive(Clone)]
pub struct ChannelHandle {
    channel: Arc<Channel>,
    descriptors: Descriptors,
}

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

impl ChannelHandle {
    /// The handle the host names this channel by.
    #[must_use]
    pub fn handle(&self) -> Handle {
        self.channel.handle
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
            channel: self.channel.handle,
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
                channel: self.channel.handle,
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

    async fn changed(&self, of: Changes, after: u64) -> Result<u64, NotAvailable> {
        match self
            .call(Operation::Changed {
                channel: self.channel.handle,
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
            .subscribe(self.channel.handle, topic)
            .await
            .map_err(failure)?;
        Ok(Feed(Arc::new(FeedInner {
            channel: Arc::clone(&self.channel),
            subscription: tokio::sync::Mutex::new(subscription),
        })))
    }

    /// Open a body-free private change lane, authorized as the original person.
    ///
    /// # Errors
    /// Returns unavailable if certification or the private subscription fails.
    pub async fn subscribe_person_chat(
        &self,
        peer: &str,
        original: &[u8],
    ) -> Result<PersonChatStream, NotAvailable> {
        if peer.len() != 64 || !matches!(original.len(), 0 | 16) {
            return Err(refused("invalid private subscription"));
        }
        Ok(PersonChatStream(
            self.feed(Topic::PersonChat {
                peer: peer.to_owned(),
                on_behalf_of: original.to_vec(),
            })
            .await?,
        ))
    }

    /// Fetch or acknowledge an ephemeral editor on the certified native channel.
    /// The private command lane carries this separate closed envelope; no
    /// editor body enters the action protocol or person-message persistence.
    ///
    /// # Errors
    /// Refuses a missing original invocation, denied transport or oversized reply.
    pub async fn private_draft(
        &self,
        request: &private_draft::Request,
        original: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        if original.len() != 16 {
            return Err(refused("private draft requires original invocation"));
        }
        let command =
            serde_json::to_vec(request).map_err(|_| refused("invalid private draft request"))?;
        let reply = self.command(Service::PersonChat, command, original).await?;
        if reply.len() > 128 * 1024 {
            return Err(refused("private draft reply exceeds bound"));
        }
        Ok(reply)
    }

    /// Invoke the person-only private host door; unsupported hosts fail closed.
    ///
    /// # Errors
    /// Returns unavailable if certification or the private owner command fails.
    pub async fn person_chat(
        &self,
        command: &[u8],
        original: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        if command.len() > 24 * 1024 || !matches!(original.len(), 0 | 16) {
            return Err(refused("invalid private conversation request"));
        }
        let reply = self
            .command(Service::PersonChat, command.to_vec(), original)
            .await?;
        if reply.len() > 1024 * 1024 {
            return Err(refused("private conversation reply exceeds bound"));
        }
        Ok(reply)
    }

    /// Send PCM or controls to this launched application's output-only service.
    ///
    /// # Errors
    /// Fails closed when the authenticated output service is absent or refuses the command.
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

    /// Ask the host to direct one current holder of the channel to publish a
    /// complete document snapshot for this late joiner.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host cannot accept the request.
    pub async fn request_document_snapshot(&self) -> Result<(), NotAvailable> {
        self.done(Operation::Read {
            channel: self.channel.handle,
            read: Read::Document,
        })
        .await
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
    /// host on this connection's descriptor lane, and the command sent names
    /// it by its lane number instead.
    ///
    /// # Errors
    /// Rejects oversized, malformed or unavailable commands and replies, and
    /// selected descriptors on a connection without a descriptor lane.
    pub async fn local_file_clipboard_command(
        &self,
        command: &file_clipboard::Command,
        parent: Option<[u8; 16]>,
    ) -> Result<file_clipboard::Reply, NotAvailable> {
        if !command.valid() {
            return Err(refused("invalid local clipboard command"));
        }
        // Measure with the widest lane number, so the bound holds whatever
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

    /// Submit selected-open routing under its original semantic invocation.
    /// The receipt confirms admission only; it does not confirm app startup.
    ///
    /// # Errors
    /// Refuses missing action custody, malformed requests and mismatched receipts.
    pub async fn local_file_open_selected(
        &self,
        command: &open_selected::Request,
        parent: Option<[u8; 16]>,
    ) -> Result<open_selected::RootReceipt, NotAvailable> {
        let parent =
            parent.ok_or_else(|| refused("selected open requires its action invocation"))?;
        if parent == [0; 16] || !command.valid() {
            return Err(refused("invalid selected open request"));
        }
        let bytes = serde_json::to_vec(command).map_err(|error| refused(error.to_string()))?;
        if bytes.len() > open_selected::MAX_WIRE_BYTES {
            return Err(refused("selected open request exceeds bound"));
        }
        let reply = self.local_file(bytes, &parent).await?;
        if reply.len() > open_selected::MAX_WIRE_BYTES {
            return Err(refused("selected open receipt exceeds bound"));
        }
        let receipt: open_selected::RootReceipt =
            serde_json::from_slice(&reply).map_err(|error| refused(error.to_string()))?;
        if !receipt.valid() || receipt.request_id != command.request.request_id {
            return Err(refused("selected open receipt differs from its request"));
        }
        Ok(receipt)
    }

    /// Read current selected-file target presentation on the Files lane. No
    /// response conveys a file grant or an observed launch outcome.
    ///
    /// # Errors
    /// Refuses invalid requests, unavailable native hosts or mismatched responses.
    pub async fn local_file_selected_discovery(
        &self,
        command: &open_selected::discovery::Request,
        parent: Option<[u8; 16]>,
    ) -> Result<open_selected::discovery::Reply, NotAvailable> {
        if !command.valid()
            || parent.is_some() != command.invocation.is_some()
            || parent == Some([0; 16])
        {
            return Err(refused("invalid selected discovery request"));
        }
        let bytes = serde_json::to_vec(command).map_err(|e| refused(e.to_string()))?;
        if bytes.len() > open_selected::discovery::MAX_BYTES {
            return Err(refused("selected discovery exceeds limit"));
        }
        let reply = self
            .local_file(bytes, parent.as_ref().map_or(&[], |id| id.as_slice()))
            .await?;
        let value =
            open_selected::discovery::decode_reply(&reply).map_err(|e| refused(e.to_string()))?;
        if !value.matches(command) {
            return Err(refused("selected discovery response scope differs"));
        }
        Ok(value)
    }

    /// Open with explicit access through original semantic action custody.
    /// The observed result requires namespace admission and normal launch
    /// readiness.
    ///
    /// # Errors
    /// Refuses missing custody, malformed access or mismatched observed results.
    pub async fn local_file_open_selected_v2(
        &self,
        command: &open_selected::v2::Request,
        parent: Option<[u8; 16]>,
    ) -> Result<open_selected::v2::ObservedReply, NotAvailable> {
        let parent =
            parent.ok_or_else(|| refused("selected open requires its action invocation"))?;
        if parent == [0; 16] || !command.valid() {
            return Err(refused("invalid selected open v2 request"));
        }
        let bytes = serde_json::to_vec(command).map_err(|error| refused(error.to_string()))?;
        if bytes.len() > open_selected::MAX_WIRE_BYTES {
            return Err(refused("selected open request exceeds bound"));
        }
        let reply = self.local_file(bytes, &parent).await?;
        let observed = open_selected::v2::decode_observed(&reply)
            .map_err(|error| refused(error.to_string()))?;
        if observed.reply.request_id != command.request.request_id
            || observed.reply.target.app_id != command.request.app_id
            || observed.access != command.request.access
        {
            return Err(refused("selected open result differs from actual request"));
        }
        Ok(observed)
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
    /// by its lane number.
    fn hand_over_selected(&self, command: &mut file_clipboard::Command) -> Result<(), String> {
        let mut selected = command.selected_mut().peekable();
        if selected.peek().is_none() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let lane = self
                .descriptors
                .as_ref()
                .ok_or("this connection cannot hand the host descriptors")?;
            // Duplicate every descriptor before sending any, so a closed one
            // leaves nothing on the lane that no request names.
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
                selected.descriptor = lane.send(fd).map_err(|error| error.to_string())?;
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

    /// Wait until the person's speaker groups may have changed since `after`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host has nothing to follow.
    pub async fn speaker_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        self.changed(Changes::Speakers, after).await
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
            channel: self.channel.handle,
            call,
        })
        .await
    }

    /// Wait until this route's channel keys, or this device's access to
    /// them, may have changed since `after` (0 at first, then what the
    /// previous call returned), and return the value to wait from next.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host has no channel service to
    /// follow or is unavailable.
    pub async fn channel_keys_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        self.changed(Changes::ChannelKeys, after).await
    }

    /// Wait until what a Nearby Send snapshot would show this connection
    /// may have changed since `after`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route may not use Nearby Send or
    /// the host has none.
    pub async fn nearby_transfer_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        self.changed(Changes::NearbyTransfers, after).await
    }

    /// Wait until the drives the shell lists may have changed since `after`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route may not list drives.
    pub async fn drives_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        self.changed(Changes::Drives, after).await
    }

    /// Wait until the person's apps may have changed since `after`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route may not list the person's
    /// apps.
    pub async fn applications_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        self.changed(Changes::Applications, after).await
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
            channel: self.channel.handle,
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

    /// Submit a single signed op into the channel: the postcard encoding of
    /// a `collab_types::SignedOp`. Answered once the host admitted it.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the op does not decode, or the host
    /// refuses it.
    pub async fn send(&self, op: &[u8]) -> Result<(), NotAvailable> {
        let op: collab_types::SignedOp = postcard::from_bytes(op)
            .map_err(|error| refused(format!("signed op does not decode: {error}")))?;
        self.publish(Publication::Document(Box::new(op))).await
    }

    /// Every signed change to the channel's document from now on.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe(&self) -> Result<OpStream, NotAvailable> {
        Ok(OpStream(self.feed(Topic::Ops).await?))
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
            channel: self.channel.handle,
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
        let json = serde_json::to_string(actions).map_err(|error| refused(error.to_string()))?;
        self.publish(Publication::Actions(json)).await
    }

    /// [`Self::publish_actions`] as a delta against what this channel has
    /// already published.
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
        self.channel.client.answer(self.channel.handle, answerer);
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

    /// The task runs the workspace hands this delegated agent.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe_task_runs(&self) -> Result<TaskRunStream, NotAvailable> {
        Ok(TaskRunStream(self.feed(Topic::TaskRuns).await?))
    }

    async fn task(&self, run_id: u64, lease: &str, call: TaskCall) -> Result<Reply, NotAvailable> {
        self.call(Operation::Task {
            channel: self.channel.handle,
            run_id,
            lease: lease.to_owned(),
            call,
        })
        .await
    }

    /// Report an update for one exact host-minted task lease.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the host refuses it or the encoded
    /// update exceeds 64 KiB.
    pub async fn report_task_run(
        &self,
        run_id: u64,
        lease: &str,
        update: &TaskRunUpdate,
    ) -> Result<(), NotAvailable> {
        let update_json = serde_json::to_string(update)
            .map_err(|error| refused(format!("task update serialization failed: {error}")))?;
        if update_json.len() > 64 * 1024 {
            return Err(refused("task update too large"));
        }
        match self
            .task(run_id, lease, TaskCall::Report(update_json))
            .await?
        {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    async fn task_step(
        &self,
        run_id: u64,
        lease: &str,
        step: TaskStep,
    ) -> Result<(u64, String), NotAvailable> {
        match self.task(run_id, lease, TaskCall::Invoke(step)).await? {
            Reply::Invoked { id, update } => Ok((id, update)),
            other => Err(unexpected(&other)),
        }
    }

    /// Invoke a published application action as a step of a leased task.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses the step.
    pub async fn invoke_task_action(
        &self,
        run_id: u64,
        lease: &str,
        target: TaskAction<'_>,
        invocation: &str,
    ) -> Result<(u64, String), NotAvailable> {
        self.task_step(
            run_id,
            lease,
            TaskStep {
                peer_id: target.peer_id,
                provider: target.provider.to_owned(),
                resource: target.resource.to_owned(),
                action: target.action.to_owned(),
                argument: target.argument.map(str::to_owned),
                invocation: invocation.to_owned(),
                wait_ms: 0,
                seen: None,
            },
        )
        .await
    }

    /// Re-ask for a native task's child `invocation`, started earlier with
    /// the same request, and let the host hold the answer as `wait` says.
    ///
    /// # Errors
    ///
    /// As [`Self::invoke_task_action`].
    pub async fn observe_task_child(
        &self,
        run_id: u64,
        lease: &str,
        target: TaskAction<'_>,
        invocation: &str,
        wait: TaskChildWait,
    ) -> Result<(u64, String), NotAvailable> {
        let seen = serde_json::to_string(&wait.seen)
            .map_err(|error| refused(format!("invalid update mark: {error}")))?;
        self.task_step(
            run_id,
            lease,
            TaskStep {
                peer_id: target.peer_id,
                provider: target.provider.to_owned(),
                resource: target.resource.to_owned(),
                action: target.action.to_owned(),
                argument: target.argument.map(str::to_owned),
                invocation: invocation.to_owned(),
                wait_ms: wait.wait_ms,
                seen: Some(seen),
            },
        )
        .await
    }

    /// Whether Task Center asked this exact leased run to stop.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn task_run_cancelled(&self, run_id: u64, lease: &str) -> Result<bool, NotAvailable> {
        Ok(self.task_run_status(run_id, lease).await?.cancelled)
    }

    /// Observe cancellation and bounded steering on this authenticated run.
    ///
    /// # Errors
    /// The host is unavailable, refuses the run or returns invalid control data.
    pub async fn task_run_status(
        &self,
        run_id: u64,
        lease: &str,
    ) -> Result<TaskRunStatus, NotAvailable> {
        let Reply::TaskCancelled { cancelled, control } =
            self.task(run_id, lease, TaskCall::Cancelled).await?
        else {
            return Err(refused("unexpected task status answer"));
        };
        let control = match control.filter(|json| !json.is_empty()) {
            None => None,
            Some(json) => {
                if json.len() > TaskRunControl::MAX_BYTES {
                    return Err(refused("oversized task direction page"));
                }
                let control: TaskRunControl = serde_json::from_str(&json)
                    .map_err(|_| refused("invalid task direction page"))?;
                control.validate().map_err(refused)?;
                Some(control)
            }
        };
        Ok(TaskRunStatus { cancelled, control })
    }

    async fn invocation(&self, id: u64, call: InvocationCall) -> Result<Reply, NotAvailable> {
        self.call(Operation::Invocation {
            channel: self.channel.handle,
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

    /// The action catalogue this endpoint may see, as it stands and then
    /// each time it changes.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn subscribe_action_catalogue(&self) -> Result<ActionCatalogueStream, NotAvailable> {
        Ok(ActionCatalogueStream(self.feed(Topic::Catalogue).await?))
    }

    /// Launch an exact installed package through this authenticated channel,
    /// returning its observed startup outcome.
    ///
    /// # Errors
    /// Returns an unavailable or refused launch.
    pub async fn launch_installed_application(
        &self,
        app_id: &str,
        package_root: Option<&str>,
    ) -> Result<(u64, String), NotAvailable> {
        self.launch_installed_application_for(app_id, package_root, None)
            .await
    }

    /// Launch on a live invocation's original caller's behalf.
    ///
    /// # Errors
    /// Returns an unavailable or refused launch.
    pub async fn launch_installed_application_for(
        &self,
        app_id: &str,
        package_root: Option<&str>,
        on_behalf_of: Option<[u8; 16]>,
    ) -> Result<(u64, String), NotAvailable> {
        match self
            .call(Operation::Launch {
                channel: self.channel.handle,
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

    /// A page of earlier admitted changes to `artifact_id` (the artifact
    /// uuid's 16 bytes), before `before_seq`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the host refuses or is unavailable.
    pub async fn fetch_scrollback_for(
        &self,
        artifact_id: &[u8],
        before_seq: u64,
        max: u32,
    ) -> Result<ScrollbackBatch, NotAvailable> {
        let artifact = <[u8; 16]>::try_from(artifact_id)
            .map(|bytes| collab_types::ArtifactId(uuid::Uuid::from_bytes(bytes)))
            .map_err(|_| refused("artifact id is not 16 bytes"))?;
        match self
            .call(Operation::Read {
                channel: self.channel.handle,
                read: Read::History {
                    artifact,
                    before_seq,
                    max,
                },
            })
            .await?
        {
            Reply::History { ops, exhausted } => Ok(ScrollbackBatch {
                ops,
                done: exhausted,
            }),
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
                channel: self.channel.handle,
                report: bytes,
            })
            .await
            .map_err(|error| match error {
                Failure::Refused(reason) => crate::local_document_rejection(&reason).map_or_else(
                    || crate::LocalDocumentActionError::Uncertain(reason.clone()),
                    |reason| crate::LocalDocumentActionError::Rejected(reason.to_owned()),
                ),
                Failure::Closed => crate::LocalDocumentActionError::Uncertain(error.to_string()),
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
    ///
    /// # Errors
    /// Refuses invalid requests or hosts that cannot run the check.
    pub async fn approved_check_reported_progress(
        &self,
        request: &crate::approved_checks::Request,
    ) -> Result<crate::approved_checks::ApprovedCheckReportedProgressStream, NotAvailable> {
        let encoded = request.encode().map_err(refused)?;
        let feed = self.feed(Topic::ApprovedCheck(encoded)).await?;
        Ok(crate::approved_checks::ApprovedCheckReportedProgressStream::new(feed, request.clone()))
    }

    /// Run an approved check and wait for its receipt.
    ///
    /// # Errors
    /// Refuses invalid requests, refused checks or miscorrelated receipts.
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

/// Signed changes to the channel's document.
#[derive(Clone)]
pub struct OpStream(Feed);

impl OpStream {
    /// The next change.
    ///
    /// # Errors
    ///
    /// The subscription ended.
    pub async fn read(&self) -> Result<OpEvent, NotAvailable> {
        match self.0.event().await? {
            Event::Op {
                op,
                from_user_id,
                admission,
            } => Ok(OpEvent {
                op,
                from_user_id,
                admission,
            }),
            other => Err(unexpected_event(&other)),
        }
    }
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
    ///
    /// # Errors
    ///
    /// The subscription ended.
    pub async fn read(&self) -> Result<BlobEvent, NotAvailable> {
        match self.0.event().await? {
            Event::Content { hash, state, bytes } => Ok(BlobEvent { hash, bytes, state }),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// The action catalogue, as JSON, as it stands and then on each change.
#[derive(Clone)]
pub struct ActionCatalogueStream(Feed);

impl ActionCatalogueStream {
    /// The catalogue as it stands next.
    ///
    /// # Errors
    ///
    /// The subscription ended.
    pub async fn read(&self) -> Result<String, NotAvailable> {
        match self.0.event().await? {
            Event::Catalogue(catalogue) => Ok(catalogue),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// The invocations addressed to this app.
#[derive(Clone)]
pub struct InvocationStream(Feed);

impl InvocationStream {
    /// The next invocation.
    ///
    /// # Errors
    ///
    /// The subscription ended.
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
            }),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// Realtime continuity events for this app, as JSON.
#[derive(Clone)]
pub struct RealtimeStream(Feed);

impl RealtimeStream {
    /// The next event.
    ///
    /// # Errors
    ///
    /// The subscription ended.
    pub async fn read(&self) -> Result<Vec<u8>, NotAvailable> {
        match self.0.event().await? {
            Event::Realtime(event) => Ok(event),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// The app's private conversation with one person, as it stands and then
/// on each change.
#[derive(Clone)]
pub struct PersonChatStream(Feed);

impl PersonChatStream {
    /// The conversation's next generation and its body, in the person chat
    /// reply encoding.
    ///
    /// # Errors
    ///
    /// The subscription ended, or the body is oversized.
    pub async fn read(&self) -> Result<(u64, Vec<u8>), NotAvailable> {
        match self.0.event().await? {
            Event::PersonChat {
                generation,
                conversation,
            } => {
                if conversation.len() > 1024 * 1024 {
                    return Err(refused("private conversation reply exceeds bound"));
                }
                Ok((generation, conversation))
            }
            other => Err(unexpected_event(&other)),
        }
    }
}

/// The task runs the workspace hands this agent.
#[derive(Clone)]
pub struct TaskRunStream(Feed);

impl TaskRunStream {
    /// The next run.
    ///
    /// # Errors
    ///
    /// The subscription ended, or the run does not decode.
    pub async fn read(&self) -> Result<TaskRunEnvelope, NotAvailable> {
        match self.0.event().await? {
            Event::TaskRun(json) => serde_json::from_str(&json)
                .map_err(|error| refused(format!("invalid task envelope JSON: {error}"))),
            other => Err(unexpected_event(&other)),
        }
    }
}

/// A workspace control session; it ends when its last clone is dropped.
#[derive(Clone)]
pub struct WorkspaceControlHandle(Feed);

impl WorkspaceControlHandle {
    /// Wait for the session's next event.
    ///
    /// # Errors
    ///
    /// The session ended.
    pub async fn read(&self) -> Result<WorkspaceControlEvent, NotAvailable> {
        match self.0.event().await? {
            Event::WorkspaceControl(event) => Ok(event),
            other => Err(unexpected_event(&other)),
        }
    }

    /// Send the controlled device one input event.
    ///
    /// # Errors
    ///
    /// The host refuses the input.
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

/// The channel's presence: what this endpoint shows, and everyone else's.
#[derive(Clone)]
pub struct PresenceHandle {
    channel: ChannelHandle,
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
    ///
    /// # Errors
    ///
    /// The host refuses it.
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
    ///
    /// # Errors
    ///
    /// The host refuses it.
    pub async fn publish_caret(&self, key_bytes: &[u8], pos: u64) -> Result<(), NotAvailable> {
        self.publish(key_bytes, collab_types::PeerPresencePayload::Caret { pos })
            .await
    }

    /// Publish a selection under `key_bytes`.
    ///
    /// # Errors
    ///
    /// The host refuses it.
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
    ///
    /// # Errors
    ///
    /// The host refuses it.
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
    ///
    /// # Errors
    ///
    /// The host refuses it.
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
    ///
    /// # Errors
    ///
    /// The host refuses it.
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

    /// Say `key_bytes` arrived.
    ///
    /// # Errors
    ///
    /// The host refuses it.
    pub async fn publish_arrived(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        self.publish(key_bytes, collab_types::PeerPresencePayload::Arrived)
            .await
    }

    /// Say `key_bytes` left.
    ///
    /// # Errors
    ///
    /// The host refuses it.
    pub async fn publish_left(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        self.publish(key_bytes, collab_types::PeerPresencePayload::Left)
            .await
    }

    /// Everyone else's presence.
    ///
    /// # Errors
    ///
    /// The host refuses or is unavailable.
    pub async fn subscribe_all(&self) -> Result<PresenceStream, NotAvailable> {
        Ok(PresenceStream(self.channel.feed(Topic::Presence).await?))
    }
}

/// Everyone else's presence on a channel.
#[derive(Clone)]
pub struct PresenceStream(Feed);

impl PresenceStream {
    /// The next presence change; a payload no app receives is skipped.
    ///
    /// # Errors
    ///
    /// The subscription ended.
    pub async fn read(&self) -> Result<PresenceEvent, NotAvailable> {
        use collab_types::PeerPresencePayload as P;
        loop {
            let Event::Presence { user_id, presence } = self.0.event().await? else {
                continue;
            };
            let payload = match presence.payload {
                P::Pointer { x, y } => PresencePayload::Pointer { x, y },
                P::Caret { pos } => PresencePayload::Caret { pos },
                P::Selection { start, end } => PresencePayload::Selection { start, end },
                P::Typing { start, end } => PresencePayload::Typing { start, end },
                P::Arrived => PresencePayload::Arrived,
                P::Left => PresencePayload::Left,
                P::Following { target } => PresencePayload::Following { target },
                P::InteractionJson(json) => PresencePayload::InteractionJson(json),
                P::Status { text, automatic } => PresencePayload::Status { text, automatic },
                _ => continue,
            };
            return Ok(PresenceEvent {
                user_id,
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
    ///
    /// # Errors
    ///
    /// The subscription ended.
    pub async fn read(&self) -> Result<ChannelContext, NotAvailable> {
        let update = match self.0.event().await? {
            Event::Context(update) => update,
            other => return Err(unexpected_event(&other)),
        };
        let mut context = ChannelContext::default();
        match update {
            ContextUpdate::Snapshot {
                collaborators,
                permissions,
            } => {
                context.collaborators = collaborators;
                context.permissions = permissions;
            }
            ContextUpdate::FollowChanged { peer_id } => context.follow_changed = Some(peer_id),
            ContextUpdate::SnapshotRequested => context.snapshot_requested = true,
            ContextUpdate::Handoff(handoff) => context.handoff = Some(handoff),
            ContextUpdate::HistoryAgents(agents) => {
                context.history_agents = Some(
                    agents
                        .into_iter()
                        .map(|agent| {
                            let [r, g, b] = agent.color;
                            HistoryAgent {
                                device: agent.device,
                                person: agent.person,
                                person_name: agent.person_name,
                                agent_id: agent.agent_id,
                                label: agent.label,
                                color: u32::from_be_bytes([0, r, g, b]),
                            }
                        })
                        .collect(),
                );
            }
            ContextUpdate::RecordsArrived(records) => context.records_arrived = records,
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
