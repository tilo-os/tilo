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
//! The verbs are few: publish into a channel, subscribe to one of its
//! topics, invoke, read. What travels in them are the domain payloads the
//! host and its peers already exchange, such as [`PeerPresence`], which
//! broker-to-broker replication carries unchanged.

use collab_types::history::RosterAgent;
use collab_types::inbound::OpAdmission;
use collab_types::presence::Collaborator;
use collab_types::{ArtifactId, PeerPresence, SignedOp};
use serde::{Deserialize, Serialize};

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
    /// Stop a subscription this connection started.
    Unsubscribe { subscription: u64 },
    /// Close a handle, ending its subscriptions.
    Close { handle: Handle },
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
}

/// The host's answer to a call that succeeded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Done,
    /// The subscription is live under the call's id.
    Subscribed,
    /// What the call opened.
    Opened(Handle),
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
