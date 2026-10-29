//! The collaboration wire: postcard messages behind `wire_frame` length
//! prefixes, the same on an app's launcher-issued socket and on the relay.
//!
//! An endpoint sends [`Call`]s; the host answers each with a
//! [`HostFrame::Reply`] carrying the call's id, so calls run concurrently on
//! one connection. There are no capability objects: what a call opens (a
//! channel) is named by a [`Handle`] the host allocates and keeps for the
//! connection's lifetime or until it is closed, and a [`Request::Subscribe`]
//! call's id names the subscription its [`HostFrame::Event`]s carry until
//! [`HostFrame::Ended`] or [`Request::Unsubscribe`].
//!
//! The verbs are few: join a channel, publish into it, subscribe to one of
//! its topics, read from it, command one of the host's services on it, and
//! wait for a change. What travels in them are the domain payloads the host
//! and its peers already exchange, such as [`PeerPresence`], which
//! broker-to-broker replication carries unchanged.

use collab_types::history::RosterAgent;
use collab_types::inbound::OpAdmission;
use collab_types::presence::Collaborator;
use collab_types::{ArtifactId, PeerPresence, SignedOp};
use serde::{Deserialize, Serialize};

use crate::{CertifiedJoinIdentity, RoomStatus};

#[cfg(feature = "client")]
pub mod client;

/// Both directions' framing.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// Something the host opened for this connection, such as a joined channel.
/// Valid on the connection that opened it until it is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Handle(pub u64);

/// One call: the endpoint's id for it, and what it asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub request: Request,
}

/// What an endpoint asks the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Join a channel, answered with [`Reply::Opened`] once the room is
    /// ready for apps to open documents in.
    Join(Join),
    /// Where the room a channel key names stands, answered as
    /// [`Reply::Room`]: now, or once it is no longer `seen`.
    Room {
        channel: String,
        seen: Option<RoomStatus>,
    },
    /// Publish into a channel this connection holds.
    Publish {
        channel: Handle,
        publication: Publication,
    },
    /// Start receiving one of a channel's topics. The call's id names the
    /// subscription in every event it delivers.
    Subscribe { channel: Handle, topic: Topic },
    /// Ask for something a channel holds; it arrives on its topic.
    Read { channel: Handle, read: Read },
    /// Ask one of the host's services something about this channel,
    /// answered as [`Reply::Answer`]. The body and answer are the service's
    /// own encoding.
    Command {
        channel: Handle,
        service: Service,
        body: Vec<u8>,
        /// The original invocation this command serves, when an action
        /// runs it on another's behalf; empty otherwise.
        on_behalf_of: Vec<u8>,
    },
    /// Wait for `of`'s next change past the revision `after`, answered as
    /// [`Reply::Changed`] with the revision to wait from next.
    Changed {
        channel: Handle,
        of: Changes,
        after: u64,
    },
    /// Open the installed app `app_id`, from `package_root` (`tapp:<hex>`)
    /// if not empty, for the invocation `on_behalf_of` (UUID bytes, or none
    /// for the person's press); answered as [`Reply::Invoked`] once it
    /// started or failed to.
    Launch {
        channel: Handle,
        app_id: String,
        package_root: String,
        on_behalf_of: Vec<u8>,
    },
    /// What the app says, or asks, of an invocation it was handed.
    Invocation {
        channel: Handle,
        id: u64,
        call: InvocationCall,
    },
    /// A delegated agent's call about the task run `run_id` it holds under
    /// `lease`.
    Task {
        channel: Handle,
        run_id: u64,
        lease: String,
        call: TaskCall,
    },
    /// Stop a subscription this connection started.
    Unsubscribe { subscription: u64 },
    /// Close a handle, ending its subscriptions.
    Close { handle: Handle },
}

/// Who joins which channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    /// The channel key: the app, room and, for a record, the record.
    pub channel: String,
    /// The device's certified presentation for the room's workspace.
    pub identity: CertifiedJoinIdentity,
    pub participant: Participant,
    /// The resource kind of the record the channel names, if the app has one.
    pub record_kind: Option<String>,
}

/// Whom the joining endpoint acts as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Participant {
    /// The device's person, shown under `handle` when it is not empty.
    Person { handle: String },
    /// The workspace agent `id` the person delegated to this device.
    Agent { id: String },
}

/// What an app says, or asks, of an invocation it was handed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationCall {
    /// It ended: `code` empty for success, else the failure and its message.
    Complete { code: String, message: String },
    /// Where it got to, as `tilo_actions::InvocationUpdate` JSON.
    Update(String),
    /// Whether its cancellation was asked, answered as [`Reply::Cancelled`].
    Cancelled,
}

/// An invocation of one of the app's published actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    /// The id the app answers it by.
    pub id: u64,
    /// Its stable invocation id, as UUID bytes.
    pub invocation: [u8; 16],
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    pub actor: String,
    /// The `tilo_actions::ActionContext` it was invoked in, as JSON.
    pub context: Option<String>,
}

/// What an agent says or asks about a task run it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskCall {
    /// The run's progress, as `TaskRunUpdate` JSON.
    Report(String),
    /// Whether the run was cancelled, answered as [`Reply::TaskCancelled`].
    Cancelled,
    /// Run an action as a step of the run, answered as [`Reply::Invoked`].
    Invoke(TaskStep),
}

/// An action a task run takes as one of its steps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStep {
    /// The live peer that provides the action; 0 when `provider` names it.
    pub peer_id: u64,
    /// A retained or application provider; empty for the live peer.
    pub provider: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    /// The native invocation this step re-asks under; empty for none.
    pub invocation: String,
    /// How long a re-ask may wait for its person to decide, in
    /// milliseconds; 0 answers at once.
    pub wait_ms: u32,
    /// The update mark, as JSON, a waiting re-ask has already seen.
    pub seen: Option<String>,
}

/// What a [`Request::Changed`] waits on. Each wait says only that
/// something changed; the endpoint asks again for what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Changes {
    /// The person's speaker groups (the Settings app's).
    Speakers,
    /// The channel's encryption keys.
    ChannelKeys,
    /// Nearby transfers this app sees.
    NearbyTransfers,
    /// The person's drives (the Files app's).
    Drives,
    /// The person's apps (the Files app's).
    Applications,
}

/// A host service a [`Request::Command`] addresses. Their commands are
/// internally tagged JSON (`native_media::Command`, `audio_output::Command`,
/// ...) or their own postcard, which the wire carries as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Service {
    /// `audio_output::Command` -> `audio_output::Snapshot`, JSON.
    AudioOutput,
    /// `native_media::Request` -> `native_media::Snapshot`, JSON.
    NativeMedia,
    /// A person-chat or private-draft command and its reply.
    PersonChat,
    /// `SpeakerCommand` -> `SpeakerReply`, JSON; the Settings app's only.
    Speaker,
    /// A realtime command and its reply, JSON.
    Realtime,
    /// `NearbyTransferCommand` -> `NearbyTransferReply`, JSON.
    NearbyTransfer,
    /// `WifiContinuityCommand` -> `WifiContinuityReply`, JSON; the Settings
    /// app's only.
    WifiContinuity,
    /// `BrowserServiceSessionCommand` -> `BrowserServiceSessionReply`; the
    /// Browser's only.
    BrowserServiceSession,
    /// An `approved_checks::Request` -> its receipt.
    ApprovedCheck,
    /// A command about the person's local files: selected discovery and
    /// opening, write attribution, external moderation or the file
    /// clipboard, each JSON in its own envelope.
    LocalFile,
    /// A `committed_document::Request` -> its `Reply`, in their encoding.
    CommittedDocument,
    /// A `tilo_actions::remote::RemoteActionFrame` the app sends another
    /// app's action -> the frame that answers it.
    RemoteAction,
}

/// What an endpoint publishes into a channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Publication {
    /// A keyed presence update, fanned out to the room's other endpoints
    /// and replicated to its peers.
    Presence(PeerPresence),
    /// A signed change to the channel's document, answered once the host
    /// admitted it (and the room's owner committed it, where one must).
    Document(Box<SignedOp>),
    /// Content the channel's record references, which the endpoint stored:
    /// the host keeps it where the record's readers are served from.
    Content { hash: String, bytes: Vec<u8> },
    /// What the app reports about its own instance or record.
    Report(Report),
    /// A terminal effect the app ran itself on one of the actions it
    /// publishes, for workspace activity; answered once recorded. `update`
    /// is the `tilo_actions::InvocationUpdate` JSON (internally tagged).
    Effect {
        resource: String,
        action: String,
        update: String,
    },
    /// The app's whole action surface, as JSON `Vec<ExportedAction>`,
    /// replacing what it published.
    Actions(String),
    /// A change to the app's action surface, as `SurfaceDelta` JSON.
    Surface(String),
}

/// What an app reports about the window it runs in or the record it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Report {
    /// What reopens this instance where it left off: its metadata, in the
    /// schema version the app reads.
    Continuation {
        schema_version: u16,
        metadata: Vec<u8>,
    },
    /// This instance has nothing to continue.
    ClearContinuation,
    /// This instance's step of a handoff, as
    /// `collab_types::handoff::HandoffReport` JSON.
    Handoff(Vec<u8>),
    /// A change to the record's search entries, as JSON.
    SearchIndex(Vec<u8>),
}

/// What an endpoint reads from a channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Read {
    /// Content the channel's record references and the endpoint lacks. It
    /// arrives on [`Topic::Content`], from this device or the room's others.
    Content { hash: String },
    /// The document's current state, which another endpoint holding the
    /// channel is asked to publish; it arrives on [`Topic::Ops`].
    Document,
    /// Up to `max` admitted changes to `artifact` before `before_seq`,
    /// answered as [`Reply::History`].
    History {
        artifact: ArtifactId,
        before_seq: u64,
        max: u32,
    },
}

/// What a subscription delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Topic {
    /// The room's other participants' presence.
    Presence,
    /// Every signed change to the channel's document from now on.
    Ops,
    /// Content of the channel's record that arrived, or where a read of it
    /// has got to.
    Content,
    /// Who is in the channel and what this endpoint may do there, and the
    /// session's directed asks of it.
    Context,
    /// The task runs the workspace hands this delegated agent.
    TaskRuns,
    /// The invocations of the app's published actions, replacing any
    /// stream it held; `stable_ids` asks for stable invocation ids.
    Invocations { stable_ids: bool },
    /// The action catalogue this endpoint may see, as it stands and then
    /// each time it changes.
    Catalogue,
}

/// The host's answer to a call that succeeded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Done,
    /// The subscription is live under the call's id.
    Subscribed,
    /// What the call opened.
    Opened(Handle),
    Room(RoomStatus),
    /// Whether an invocation's cancellation was asked.
    Cancelled(bool),
    /// Whether a task run was cancelled, and the control it shows now as
    /// JSON, if any.
    TaskCancelled {
        cancelled: bool,
        control: Option<String>,
    },
    /// The invocation a task step or launch started, and its state now as
    /// `InvocationUpdate` JSON.
    Invoked {
        id: u64,
        update: String,
    },
    /// The revision a [`Request::Changed`] waits from next.
    Changed(u64),
    /// A [`Request::Command`]'s answer, in its service's encoding.
    Answer(Vec<u8>),
    /// A page of history, each change the postcard bytes of its
    /// [`SignedOp`]. Not `exhausted` while older changes remain here or
    /// were asked of the room's other devices, whose answer a later read
    /// finds.
    History {
        ops: Vec<Vec<u8>>,
        exhausted: bool,
    },
}

/// One event a subscription delivers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Another participant's presence, attributed by the host.
    Presence {
        user_id: String,
        presence: PeerPresence,
    },
    /// A signed document change, as the postcard bytes of its
    /// [`SignedOp`], with who sent it and the channel's verdict on it.
    Op {
        op: Vec<u8>,
        from_user_id: String,
        admission: OpAdmission,
    },
    /// Content of the channel's record: its bytes once `state` is
    /// resolved, else where it has got to.
    Content {
        hash: String,
        state: tilo_content::ContentState,
        bytes: Vec<u8>,
    },
    Context(ContextUpdate),
    /// An invocation handed to this app.
    Invocation(Invocation),
    /// The action catalogue as JSON.
    Catalogue(String),
    /// A task run handed to this agent, as `TaskRunEnvelope` JSON (its
    /// action context is free-form JSON, which postcard cannot carry).
    TaskRun(String),
}

/// One change of a channel's context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextUpdate {
    /// The roster and this endpoint's permissions, replacing the last.
    Snapshot {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    /// The person at this desktop started following `peer_id`, or stopped.
    FollowChanged { peer_id: Option<String> },
    /// Publish the document's current state.
    SnapshotRequested,
    /// The next step of a handoff addressed to this instance, as
    /// `collab_types::handoff::HandoffRequest` JSON.
    Handoff(Vec<u8>),
    /// The room's delegated agents the session verified, for history.
    HistoryAgents(Vec<RosterAgent>),
    /// Records of the application's trees a peer wrote and no window holds.
    RecordsArrived(Vec<String>),
}

/// What the host sends an endpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostFrame {
    /// The answer to the call `id`.
    Reply {
        id: u64,
        result: Result<Reply, String>,
    },
    /// One event of the subscription started by the call `subscription`.
    Event { subscription: u64, event: Event },
    /// The host ended the subscription: its channel closed or the endpoint
    /// lost the right to it. No further events follow.
    Ended { subscription: u64, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::PeerPresencePayload;

    #[test]
    fn frames_round_trip_through_the_framing() {
        let call = Call {
            id: 7,
            request: Request::Publish {
                channel: Handle(3),
                publication: Publication::Presence(PeerPresence {
                    key: vec![1, 2],
                    payload: PeerPresencePayload::Caret { pos: 9 },
                }),
            },
        };
        let bytes = FRAMING.encode_value(&call).unwrap();
        let read: Call = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(read, call);

        let event = HostFrame::Event {
            subscription: 7,
            event: Event::Presence {
                user_id: "peer".into(),
                presence: PeerPresence {
                    key: vec![],
                    payload: PeerPresencePayload::Left,
                },
            },
        };
        let bytes = FRAMING.encode_value(&event).unwrap();
        let read: HostFrame = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(read, event);
    }
}
