//! The collaboration wire: postcard messages behind `wire_frame` length
//! prefixes, the same on an app's launcher-issued socket and on the relay.
//!
//! An endpoint sends [`Call`]s; the host answers each with a
//! [`HostFrame::Reply`] carrying the call's id, so calls run concurrently on
//! one connection. The connection itself is what the host admitted: the app,
//! workspace and person its launch named. A call about one of the app's
//! records names it ([`Record`]) and is decided for that connection as it
//! comes; what the host keeps for a record lasts until the endpoint leaves it
//! ([`Operation::Leave`]) or the connection ends. A [`Operation::Subscribe`]
//! call's id names the subscription its [`HostFrame::Event`]s carry until
//! [`HostFrame::Ended`] or [`Operation::Unsubscribe`].
//!
//! The verbs are few: publish about a record, subscribe to one of its
//! topics, read from it, command one of the host's services about it, and
//! wait for a change. What travels in them are the domain payloads the host
//! and its peers already exchange, such as [`PeerPresence`], which
//! broker-to-broker replication carries unchanged.

use collab_types::PeerPresence;
use collab_types::presence::Member;
use serde::{Deserialize, Serialize};

use crate::{
    ActionDirectoryQuery, DisplayOrigin, PublishedInvocationObservation, WorkspaceControlEvent,
    WorkspaceControlInput, WorkspaceStatus,
};

#[cfg(feature = "client")]
pub mod client;
pub mod shell;

/// Both directions' framing.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// Something the host opened for this connection: the session's published resources, or a
/// provider the person's shell serves. Valid on the connection that opened it until it is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Handle(pub u64);

/// A record a call is about: its key (the app, workspace and, for a record, the record, as
/// [`crate::route`] writes them), and the kind of resource it is, when the app names one. The host
/// decides each call about it for the connection that makes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub kind: Option<String>,
}

impl Record {
    /// The record `key` names, of the kind `kind` (none when empty).
    #[must_use]
    pub fn new(key: impl Into<String>, kind: &str) -> Self {
        Self {
            key: key.into(),
            kind: Some(kind.to_owned()).filter(|kind| !kind.is_empty()),
        }
    }
}

/// One call: the endpoint's id for it, and what it asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub request: Operation,
}

/// What an endpoint asks the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    /// Where the workspace a record's key names stands, answered
    /// as [`Reply::Workspace`]: now, or once it is no longer `seen`.
    Workspace {
        record: String,
        seen: Option<WorkspaceStatus>,
    },
    /// Publish about a record.
    Publish {
        record: Record,
        publication: Publication,
    },
    /// Start receiving one of a record's topics. The call's id names the
    /// subscription in every event it delivers.
    Subscribe { record: Record, topic: Topic },
    /// Ask for something a record holds; it arrives on its topic.
    Read { record: Record, read: Read },
    /// Ask one of the host's services something about a record,
    /// answered as [`Reply::Answer`]. The body and answer are the service's
    /// own encoding.
    Command {
        record: Record,
        service: Service,
        body: Vec<u8>,
        /// The original invocation this command serves, when an action
        /// runs it on another's behalf; empty otherwise.
        on_behalf_of: Vec<u8>,
    },
    /// Wait for `of`'s next change past the revision `after`, answered as
    /// [`Reply::Changed`] with the revision to wait from next.
    Changed {
        record: Record,
        of: Changes,
        after: u64,
    },
    /// Open the installed app `app_id`, from `package_root` (`tapp:<hex>`)
    /// if not empty, for the invocation `on_behalf_of` (UUID bytes, or none
    /// for the person's press); answered as [`Reply::Invoked`] once it
    /// started or failed to.
    Launch {
        record: Record,
        app_id: String,
        package_root: String,
        on_behalf_of: Vec<u8>,
    },
    /// What the app says, or asks, of an invocation it was handed.
    Invocation {
        record: Record,
        id: u64,
        call: InvocationCall,
    },
    /// A call on the app's own settings.
    Settings { record: Record, call: SettingsCall },
    /// A call on the app's live activity: what it shows outside its
    /// windows, and the controls pressed on it.
    LiveActivity {
        record: Record,
        call: LiveActivityCall,
    },
    /// Admit a local document action the app already performed, as an
    /// encoded `LocalDocumentActionReport`, answered as [`Reply::Admitted`].
    /// A refusal starting with `LOCAL_DOCUMENT_REJECTED` proves it was not
    /// admitted; any other failure leaves it uncertain.
    Admit { record: Record, report: Vec<u8> },
    /// Input for the workspace control session the subscription
    /// `subscription` ([`Topic::WorkspaceControl`]) holds.
    ControlInput {
        subscription: u64,
        input: WorkspaceControlInput,
    },
    /// The endpoint's answer to the host's [`HostFrame::Ask`] `ask`.
    Answer {
        ask: u64,
        answer: Result<Answer, String>,
    },
    /// Stop a subscription this connection started.
    Unsubscribe { subscription: u64 },
    /// Close a handle, ending its subscriptions.
    Close { handle: Handle },
    /// The endpoint is done with the record `record` (its key): what the host kept for it on this
    /// connection, and the subscriptions about it, end.
    Leave { record: String },
    /// The session's published resources: what its services read and the
    /// actions that change them, answered as [`Reply::Opened`] with the
    /// handle the other session calls name. The session is the device's,
    /// not a workspace's, so no record is named for it.
    Services,
    /// `resource`'s reading now, answered as [`Reply::Snapshot`].
    Snapshot { services: Handle, resource: String },
    /// Receive `resource`'s reading now and each time it changes. The
    /// call's id names the subscription in every [`Event::Reading`] it
    /// delivers.
    Watch { services: Handle, resource: String },
    /// Invoke `action` of `resource` with `arguments` (a JSON object, empty
    /// for none), answered as [`Reply::Receipt`].
    Invoke {
        services: Handle,
        resource: String,
        action: String,
        arguments: Vec<u8>,
    },
    /// What the person's shell asks of the device, on a connection the host admitted as that
    /// shell by its account ([`shell`]), answered as [`Reply::Shell`].
    Shell(shell::ShellCall),
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
    pub actor: tilo_actions::Actor,
    /// The `tilo_actions::ActionContext` it was invoked in, as JSON.
    pub context: Option<String>,
    /// What the person typed answering the asked record this call waited in, one entry per
    /// take, secrets included (`tilo_actions::remote::asked::Approval::typed`, which the
    /// context's JSON leaves out). Handed to the app the call is delivered to, on the device whose
    /// store took the answer, and nowhere else: never written into the context, a record or a
    /// log.
    #[serde(default)]
    pub typed: Typed,
}

/// What a person typed answering an ask, wiped when dropped and never printed.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Typed(pub Vec<zeroize::Zeroizing<String>>);

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Typed({} withheld)", self.0.len())
    }
}

/// A call on the app's settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsCall {
    /// The app's view once its revision differs from `after` (at once for
    /// 0), answered as [`Reply::Settings`].
    Read { after: u64 },
    /// Merge `values` (a JSON object) into the app's entry in `section`,
    /// answered as [`Reply::Settings`] with the view after the write.
    /// `action` names the action that made the change, for history.
    Update {
        section: String,
        values: Vec<u8>,
        action: Option<String>,
    },
    /// A display transaction, the Settings app's only, answered as
    /// [`Reply::Answer`].
    Display {
        command: Vec<u8>,
        origin: DisplayOrigin,
    },
}

/// A call on the app's live activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveActivityCall {
    /// Show this JSON object as the app's activity; empty ends it.
    Show(Vec<u8>),
    /// The controls pressed after `after`, once there is one, answered as
    /// [`Reply::Controls`].
    Controls { after: u64 },
}

/// What the host asks an endpoint that published something it answers for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    /// A page of the action directory the endpoint published about `record` (its key),
    /// answered as [`Answer::Page`].
    Query {
        record: String,
        query: ActionDirectoryQuery,
    },
    /// One action of that directory, current now, answered as
    /// [`Answer::Resolution`]; `local_document` asks only for the retained
    /// snapshot a local document action was captured from.
    Resolve {
        record: String,
        revision: String,
        resource: String,
        action: String,
        local_document: bool,
    },
    /// A call of the provider the endpoint serves on `channel` ([`shell::ShellCall::Provide`]):
    /// a `tilo_actions::remote::RemoteActionFrame` request, as the device's dispatch admitted it,
    /// answered as [`Answer::Action`]. `caller` is who the dispatch admitted it for, which only
    /// the device's daemon says, on the link it made for its own shell. What the person typed into the ask an approved call
    /// answers travels beside the frame in `typed`, on this local connection alone, never in it.
    Action {
        channel: Handle,
        frame: Vec<u8>,
        /// Boxed: who a call is for is large, and every frame carrying one would be as large.
        caller: Box<tilo_actions::remote::KeptCaller>,
        typed: shell::Typed,
    },
}

/// An endpoint's answer to an [`Ask`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// An `ActionDirectoryPage`, as JSON (its descriptors skip absent
    /// fields, which postcard cannot carry).
    Page(String),
    /// An `ActionDirectoryResolution`, as JSON.
    Resolution(String),
    /// The `tilo_actions::remote::RemoteActionFrame` answering an [`Ask::Action`].
    Action(Vec<u8>),
}

/// One step of an approved check's reported progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckProgress {
    /// An encoded `approved_checks::Report`.
    Report(Vec<u8>),
    /// The encoded terminal `approved_checks::Receipt`; nothing follows.
    Receipt(Vec<u8>),
    /// The check failed; nothing follows.
    Failed(String),
}

/// What a [`Operation::Changed`] waits on. Each wait says only that
/// something changed; the endpoint asks again for what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Changes {
    /// The record's encryption keys.
    GroupKeys,
    /// Retired: Nearby Send is the device daemon's resource now, which the
    /// shell reads over its link. Kept in its place so a wait an app wrote
    /// still reads as itself; it is answered unimplemented.
    RetiredNearbyTransfers,
}

/// A host service a [`Operation::Command`] addresses. Their commands are
/// internally tagged JSON (`native_media::Command`, `audio_output::Command`,
/// ...) or their own postcard, which the wire carries as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Service {
    /// `audio_output::Command` -> `audio_output::Snapshot`, JSON.
    AudioOutput,
    /// `native_media::Request` -> `native_media::Snapshot`, JSON.
    NativeMedia,
    /// A realtime command and its reply, JSON.
    Realtime,
    /// Retired: Nearby Send is the device daemon's, invoked over its shell
    /// link. Kept in its place so a command an app wrote still reads as
    /// itself; it is refused.
    RetiredNearbyTransfer,
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
    /// A `tilo_resource_store::wire::Command` -> its `Reply`, JSON: the app's resources, written
    /// and read in the device's one store.
    Resources,
}

/// What an endpoint publishes about a record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Publication {
    /// A keyed presence update, fanned out to the workspace's other
    /// endpoints and replicated to its peers.
    Presence(PeerPresence),
    /// Content the record references, which the endpoint stored:
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
    /// The app answers for its own action directory from now on: the host
    /// asks it with [`HostFrame::Ask`] until it leaves the record.
    Directory,
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

/// What an endpoint reads of a record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Read {
    /// Content the record references and the endpoint lacks. It
    /// arrives on [`Topic::Content`], from this device or the workspace's
    /// others.
    Content { hash: String },
}

/// What a subscription delivers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Topic {
    /// The workspace's other members' presence.
    Presence,
    /// Content of the record that arrived, or where a read of it
    /// has got to.
    Content,
    /// Who is in the record's workspace and what this endpoint may do there,
    /// and the session's directed asks of it.
    Context,
    /// The invocations of the app's published actions, replacing any
    /// stream it held; `stable_ids` asks for stable invocation ids.
    Invocations { stable_ids: bool },
    /// The realtime continuity events this app sees.
    Realtime,
    /// The reported progress of the approved check the encoded
    /// `approved_checks::Request` starts, ending with its receipt.
    ApprovedCheck(Vec<u8>),
    /// A workspace control session with `target_device`, open while the
    /// subscription lives; input goes in [`Operation::ControlInput`].
    WorkspaceControl { target_device: [u8; 32] },
    /// A query of the app's resources held open (`tilo_resource_store::Query`,
    /// JSON): its first page, then each page a change makes different.
    Resources(Vec<u8>),
    /// What the host says of these actions a window shows, as the person at this endpoint would
    /// run them: all of them, then all again whenever one changes.
    ShownActions(Vec<crate::shown_actions::ShownAction>),
}

/// The host's answer to a call that succeeded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Done,
    /// The subscription is live under the call's id.
    Subscribed,
    /// What the call opened.
    Opened(Handle),
    Workspace(WorkspaceStatus),
    /// Whether an invocation's cancellation was asked.
    Cancelled(bool),
    /// The invocation a launch started, and its state now as
    /// `InvocationUpdate` JSON.
    Invoked {
        id: u64,
        update: String,
    },
    /// The revision a [`Operation::Changed`] waits from next.
    Changed(u64),
    /// A [`Operation::Command`]'s answer, in its service's encoding.
    Answer(Vec<u8>),
    /// The app's settings view: its revision, and its sections as JSON.
    Settings {
        revision: u64,
        document: Vec<u8>,
    },
    /// Controls pressed on the app's live activity, as a JSON array, and
    /// the serial to wait after next.
    Controls {
        serial: u64,
        controls: Vec<u8>,
    },
    /// Where an admitted local document action stands.
    Admitted(PublishedInvocationObservation),
    /// A [`Operation::Snapshot`]: the services' revision and `resource`'s
    /// reading as JSON, `None` when no service publishes it.
    Snapshot {
        revision: u64,
        reading: Option<Vec<u8>>,
    },
    /// What a [`Operation::Invoke`] did.
    Receipt(Receipt),
    /// The answer to a [`Operation::Shell`].
    Shell(shell::ShellReply),
}

/// What invoking an action of one of the session's published resources did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Receipt {
    /// It finished on return: the `tilo_actions::ActionOutput` as JSON.
    Done(Vec<u8>),
    /// It was taken and runs on the device; its result lands in the
    /// resource's reading.
    Accepted,
    /// The API refused it: the refusal's code (`unknown_action`,
    /// `unavailable`, `confirmation_required`, ...), and what it says.
    Refused { code: String, message: String },
}

/// One event a subscription delivers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Another member's presence, attributed by the host.
    Presence {
        member: collab_types::MemberId,
        presence: PeerPresence,
    },
    /// Content of the record: its bytes once `state` is
    /// resolved, else where it has got to.
    Content {
        hash: String,
        state: tilo_content::ContentState,
        bytes: Vec<u8>,
    },
    Context(ContextUpdate),
    /// An invocation handed to this app.
    Invocation(Invocation),
    /// A realtime continuity event, as JSON.
    Realtime(Vec<u8>),
    ApprovedCheck(CheckProgress),
    WorkspaceControl(WorkspaceControlEvent),
    /// A watched resource's reading changed: it as JSON, and the services'
    /// revision it was read at. `None` once no service publishes it.
    Reading {
        resource: String,
        revision: u64,
        reading: Option<Vec<u8>>,
    },
    /// A page of a query held open ([`Topic::Resources`]), as
    /// `tilo_resource_store::Page` JSON.
    Resources(Vec<u8>),
    /// What the host says of the actions a [`Topic::ShownActions`] names.
    ShownActions(Vec<crate::shown_actions::ActionDecision>),
}

/// One change of a record's context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextUpdate {
    /// The roster, this endpoint's permissions, and the apps installed on
    /// this device, replacing the last.
    Snapshot {
        members: Vec<Member>,
        permissions: Vec<String>,
        /// What each installed app is called, in each language its package
        /// names it in: a window names another app's changes by these, never
        /// by an id. Empty on a host that names none.
        apps: Vec<collab_types::history::NamedApp>,
    },
    /// The person at this desktop started following `peer_id`, or stopped.
    FollowChanged {
        peer_id: Option<tilo_actions::MemberId>,
    },
    /// The next step of a handoff addressed to this instance, as
    /// `collab_types::handoff::HandoffRequest` JSON.
    Handoff(Vec<u8>),
    /// Records of the application's trees a peer wrote and no window holds.
    ResourcesArrived(Vec<String>),
    /// Records another device wrote into this workspace's store: what a window that reads the
    /// store read of them is read again.
    RecordsMoved(Vec<String>),
    /// The record changed past what the endpoint was sent: it asks the changes from its own
    /// heads (the store command `RecordChanges`).
    RecordChanged,
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
    /// The host ended the subscription: the endpoint left its record or
    /// lost the right to it. No further events follow.
    Ended { subscription: u64, reason: String },
    /// The host asks the endpoint something it answers for; the endpoint
    /// answers with [`Operation::Answer`] naming `id`.
    Ask { id: u64, ask: Ask },
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::PeerPresencePayload;

    tilo_test_env::scenario! {
        fn wire_frames_and_session_calls;

        phase frames_round_trip_through_the_framing;
        {
            let call = Call {
                id: 7,
                request: Operation::Publish {
                    record: Record::new("app/workspace", ""),
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
                    member: collab_types::MemberId::person("peer"),
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

        phase the_session_calls_and_their_answers_round_trip;
        {
            let invoke = Call {
                id: 4,
                request: Operation::Invoke {
                    services: Handle(1),
                    resource: "sound".into(),
                    action: "set-volume".into(),
                    arguments: br#"{"level":0.4}"#.to_vec(),
                },
            };
            let bytes = FRAMING.encode_value(&invoke).unwrap();
            let read: Call = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
            assert_eq!(read, invoke);

            let reading = HostFrame::Event {
                subscription: 4,
                event: Event::Reading {
                    resource: "sound".into(),
                    revision: 9,
                    reading: Some(b"{}".to_vec()),
                },
            };
            let bytes = FRAMING.encode_value(&reading).unwrap();
            let read: HostFrame = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
            assert_eq!(read, reading);

            let receipt = HostFrame::Reply {
                id: 4,
                result: Ok(Reply::Receipt(Receipt::Refused {
                    code: "unavailable".into(),
                    message: "there is no sound output".into(),
                })),
            };
            let bytes = FRAMING.encode_value(&receipt).unwrap();
            let read: HostFrame = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
            assert_eq!(read, receipt);
        }

        phase a_call_of_a_shell_provider_carries_typed_answers_beside_its_frame_and_never_shows_them;
        // What a person typed into an ask travels to the shell's provider on the local link, and
        // no log of the frame shows it.
        {
            let typed = shell::Typed(vec![zeroize::Zeroizing::new("hunter2".to_owned())]);
            let ask = HostFrame::Ask {
                id: 3,
                ask: Ask::Action {
                    channel: Handle(2),
                    frame: br#"{"version":1}"#.to_vec(),
                    caller: Box::new(tilo_actions::remote::Caller::local_person("someone").kept()),
                    typed,
                },
            };
            let bytes = FRAMING.encode_value(&ask).unwrap();
            let read: HostFrame = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
            assert_eq!(read, ask);
            let shown = format!("{read:?}");
            assert!(!shown.contains("hunter2"), "a log shows no typed answer: {shown}");
        }

        phase a_roster_of_rows_without_their_optional_parts_round_trips;
        // A roster row without a role, avatar, join time or self mark reaches the endpoint as it
        // was sent: postcard reads fields by position, so a row that left one out would not decode
        // and the endpoint would take it for the host hanging up.
        {
            let bare = collab_types::Member::of_person(&collab_types::Person::new("p-1", "Ada"), None);
            let full = collab_types::Member::of_person(
                &collab_types::Person::new("p-2", "Grace"),
                Some(collab_types::Role::Owner),
            )
            .joined_at(Some(5))
            .claiming_self();
            for members in [vec![bare.clone()], vec![full.clone()], vec![bare, full]] {
                let frame = HostFrame::Event {
                    subscription: 2,
                    event: Event::Context(ContextUpdate::Snapshot {
                        members,
                        permissions: vec!["edit".into()],
                        apps: Vec::new(),
                    }),
                };
                let bytes = FRAMING.encode_value(&frame).unwrap();
                let read: HostFrame = FRAMING.read_value(&mut bytes.as_slice()).unwrap().unwrap();
                assert_eq!(read, frame);
            }
        }
    }
}
