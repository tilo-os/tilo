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

use collab_types::PeerPresence;
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
}

/// What a subscription delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Topic {
    /// The room's other participants' presence.
    Presence,
}

/// The host's answer to a call that succeeded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Done,
    /// The subscription is live under the call's id.
    Subscribed,
    /// What the call opened.
    Opened(Handle),
}

/// One event a subscription delivers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Another participant's presence, attributed by the host.
    Presence {
        user_id: String,
        presence: PeerPresence,
    },
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
