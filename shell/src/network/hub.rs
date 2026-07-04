//! In-process broadcaster for [`NeighbourMsg`] — Phase 1 Task 1.2.
//!
//! [`Hub`] is a synchronous fan-out used by tests and by the hosted
//! single-process collaboration mode. Real cross-host delivery happens via
//! [`crate::network::p2p`]; both transports share the [`NeighbourMsg`] envelope
//! so subscriber implementations are transport-agnostic.

use crate::network::protocol::NeighbourMsg;

/// Anything that wants to receive broadcast traffic.
///
/// `id()` is the subscriber's identity and is also used to suppress echo —
/// a [`Hub::broadcast`] call with `from == sub.id()` will skip that
/// subscriber.
pub trait TopicSubscriber {
    fn id(&self) -> u64;
    fn handle(&mut self, sender: u64, msg: &NeighbourMsg);
}

/// Synchronous broadcast bus.
///
/// Subscribers are owned by the hub. To inspect a subscriber from tests,
/// wrap the underlying state in `Arc<Mutex<_>>` and attach a thin proxy
/// that forwards [`TopicSubscriber`] calls to it.
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

    /// Deliver `msg` to every subscriber whose id is not `from`.
    pub fn broadcast(&mut self, from: u64, msg: NeighbourMsg) {
        for sub in self.subscribers.iter_mut() {
            if sub.id() != from {
                sub.handle(from, &msg);
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
        recv: Vec<(u64, NeighbourMsg)>,
    }

    impl TopicSubscriber for RecordingSub {
        fn id(&self) -> u64 {
            self.id
        }
        fn handle(&mut self, sender: u64, msg: &NeighbourMsg) {
            self.recv.push((sender, msg.clone()));
        }
    }

    /// Proxy that lets tests retain a handle to a subscriber after attach.
    struct ArcSub<T: TopicSubscriber>(Arc<Mutex<T>>);

    impl<T: TopicSubscriber> TopicSubscriber for ArcSub<T> {
        fn id(&self) -> u64 {
            self.0.lock().unwrap().id()
        }
        fn handle(&mut self, sender: u64, msg: &NeighbourMsg) {
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
    fn broadcast_delivers_to_all_other_subscribers() {
        let a = make_sub(1);
        let b = make_sub(2);
        let c = make_sub(3);

        let mut hub = Hub::new();
        hub.attach(Box::new(ArcSub(a.clone())));
        hub.attach(Box::new(ArcSub(b.clone())));
        hub.attach(Box::new(ArcSub(c.clone())));

        let msg = NeighbourMsg::SyncReq {
            since: vec![9, 9, 9],
        };
        hub.broadcast(1, msg.clone());

        assert!(a.lock().unwrap().recv.is_empty(), "sender must not echo");
        assert_eq!(b.lock().unwrap().recv, vec![(1, msg.clone())]);
        assert_eq!(c.lock().unwrap().recv, vec![(1, msg)]);
    }

    #[test]
    fn broadcast_does_not_echo_to_sender() {
        let a = make_sub(10);
        let b = make_sub(20);

        let mut hub = Hub::new();
        hub.attach(Box::new(ArcSub(a.clone())));
        hub.attach(Box::new(ArcSub(b.clone())));

        let msg = NeighbourMsg::PresenceCleared {
            peer: 10,
            key: "cursor".to_string(),
        };
        hub.broadcast(10, msg.clone());

        assert!(
            a.lock().unwrap().recv.is_empty(),
            "the sender (id=10) must not receive its own broadcast"
        );
        assert_eq!(b.lock().unwrap().recv, vec![(10, msg)]);
    }
}
