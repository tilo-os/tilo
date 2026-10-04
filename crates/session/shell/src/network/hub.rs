//! In-process broadcaster for [`NeighbourMsg`].
//!
//! [`Hub`] is a synchronous fan-out used by tests and the hosted
//! single-process collaboration mode; real cross-host delivery happens
//! via `crate::network::p2p`, and both transports share the
//! [`NeighbourMsg`] envelope so subscriber implementations stay transport-agnostic.

use crate::network::protocol::NeighbourMsg;
use crate::shell::state::PeerOrigin;

/// Anything that wants to receive broadcast traffic.
///
/// `id()` is the subscriber's identity and also suppresses echo — a
/// [`Hub::broadcast`] call with `from == sub.id()` skips that subscriber.
///
/// `origin()` is the *transport* identity messages from this subscriber
/// carry, which a receiver may base an authority decision on — defaults
/// to the subscriber id, overridden by a subscriber standing in for a real peer with that peer's [`PeerOrigin`].
pub trait TopicSubscriber {
    fn id(&self) -> u64;
    fn origin(&self) -> PeerOrigin {
        PeerOrigin::hub(self.id())
    }
    fn handle(&mut self, sender: &PeerOrigin, msg: &NeighbourMsg);
}

/// Synchronous broadcast bus.
///
/// Subscribers are owned by the hub. To inspect one from tests, wrap the
/// underlying state in `Arc<Mutex<_>>` and attach a thin proxy that forwards [`TopicSubscriber`] calls to it.
pub struct Hub {
    subscribers: Vec<Box<dyn TopicSubscriber>>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            subscribers: Vec::new(),
        }
    }

    /// Add a subscriber to the bus. The hub takes ownership.
    pub fn attach(&mut self, sub: Box<dyn TopicSubscriber>) {
        self.subscribers.push(sub);
    }

    /// Deliver `msg` to every subscriber whose id is not `from`. The sender
    /// is named to receivers by its own [`TopicSubscriber::origin`] rather
    /// than the raw `from` id, so a subscriber standing in for a real peer
    /// delivers under that peer's identity; an unattached `from` is named by its id, the only thing known about it.
    pub fn broadcast(&mut self, from: u64, msg: &NeighbourMsg) {
        let origin = self
            .subscribers
            .iter()
            .find(|s| s.id() == from)
            .map_or_else(|| PeerOrigin::hub(from), |s| s.origin());
        for sub in &mut self.subscribers {
            if sub.id() != from {
                sub.handle(&origin, msg);
            }
        }
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct RecordingSub {
        id: u64,
        recv: Vec<(PeerOrigin, NeighbourMsg)>,
    }

    impl TopicSubscriber for RecordingSub {
        fn id(&self) -> u64 {
            self.id
        }
        fn handle(&mut self, sender: &PeerOrigin, msg: &NeighbourMsg) {
            self.recv.push((sender.clone(), msg.clone()));
        }
    }

    /// Proxy that lets tests retain a handle to a subscriber after attach.
    struct ArcSub<T: TopicSubscriber>(Arc<Mutex<T>>);

    impl<T: TopicSubscriber> TopicSubscriber for ArcSub<T> {
        fn id(&self) -> u64 {
            self.0.lock().unwrap().id()
        }
        fn origin(&self) -> PeerOrigin {
            self.0.lock().unwrap().origin()
        }
        fn handle(&mut self, sender: &PeerOrigin, msg: &NeighbourMsg) {
            self.0.lock().unwrap().handle(sender, msg);
        }
    }

    fn make_sub(id: u64) -> Arc<Mutex<RecordingSub>> {
        Arc::new(Mutex::new(RecordingSub {
            id,
            recv: Vec::new(),
        }))
    }

    #[test]
    fn broadcast_reaches_every_other_subscriber_but_not_the_sender() {
        // Phase `broadcast_delivers_to_all_other_subscribers`.
        {
            let a = make_sub(1);
            let b = make_sub(2);
            let c = make_sub(3);

            let mut hub = Hub::new();
            hub.attach(Box::new(ArcSub(a.clone())));
            hub.attach(Box::new(ArcSub(b.clone())));
            hub.attach(Box::new(ArcSub(c.clone())));

            let msg = NeighbourMsg::SyncReq {
                since: vec![9, 9, 9],
                room: "6b3f9a10-0000-4000-8000-00000000000b".to_string(),
                invite_id: None,
                nonce: 0,
            };
            hub.broadcast(1, &msg);

            assert!(a.lock().unwrap().recv.is_empty(), "sender must not echo");
            assert_eq!(
                b.lock().unwrap().recv,
                vec![(PeerOrigin::hub(1), msg.clone())]
            );
            assert_eq!(c.lock().unwrap().recv, vec![(PeerOrigin::hub(1), msg)]);
        }
        // Phase `broadcast_does_not_echo_to_sender`.
        {
            let a = make_sub(10);
            let b = make_sub(20);

            let mut hub = Hub::new();
            hub.attach(Box::new(ArcSub(a.clone())));
            hub.attach(Box::new(ArcSub(b.clone())));

            let msg = NeighbourMsg::PresenceCleared {
                peer: 10,
                key: "cursor".to_string(),
            };
            hub.broadcast(10, &msg);

            assert!(
                a.lock().unwrap().recv.is_empty(),
                "the sender (id=10) must not receive its own broadcast"
            );
            assert_eq!(b.lock().unwrap().recv, vec![(PeerOrigin::hub(10), msg)]);
        }
    }
}
