//! The broker's outbound half: handing a locally-authored payload to the
//! peer network, once, for both collaboration transports — one answer
//! to "does a local edit leave this host", not one per transport.
//!
//! A publish is addressed by the same [`CrdtRoute`] the caller handed to
//! the local fanout, rebuilt remotely through [`CrdtRoute::delivers_to`].
//! Scope follows the payload, not the transport: documents publish the
//! route verbatim, records included; presence reaches the room only,
//! since the wire frame has no record field.
//!
//! Every refusal to publish is logged once per publisher for
//! steady-state conditions (no network), not once per keystroke.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collab_types::{ArtifactId, PeerPresence, SignedOp};

use crate::shell::collab_broker::CrdtRoute;
use crate::shell::network::ShellNetwork;

/// The network operations the broker's outbound half needs. A trait
/// rather than a bare [`ShellNetwork`] so the routing decision can be
/// tested against a recording sink instead of standing up Iroh.
pub trait PeerPublishSink: Send + Sync {
    fn app_crdt_incremental(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    );

    fn app_crdt_signed(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: SignedOp,
    );

    fn app_crdt_snapshot(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    );

    /// Republish typed presence independently of the local app transport.
    fn app_presence(&self, app_id: String, cluster_id: String, presence: PeerPresence);

    /// Ask the room whether anyone still holds a range of history.
    fn scrollback_request(&self, artifact: ArtifactId, before_seq: u64, max: u16);
}

impl PeerPublishSink for ShellNetwork {
    fn app_crdt_incremental(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        self.publish_app_crdt_incremental(app_id, cluster_id, record_id, data);
    }

    fn app_crdt_signed(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: SignedOp,
    ) {
        self.publish_app_crdt_signed(app_id, cluster_id, record_id, envelope);
    }

    fn app_crdt_snapshot(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        self.publish_app_crdt_snapshot(app_id, cluster_id, record_id, data);
    }

    fn app_presence(&self, app_id: String, cluster_id: String, presence: PeerPresence) {
        self.publish_app_presence(app_id, cluster_id, presence);
    }

    fn scrollback_request(&self, artifact: ArtifactId, before_seq: u64, max: u16) {
        self.publish_scrollback_request(artifact, before_seq, max);
    }
}

/// Which transport a payload arrived on. Carried only so a refusal
/// names the door it came through — the routing decision is identical
/// either way, and that is the point of this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The legacy length-prefixed JSON socket (`QUICK_COLLAB_SOCK`).
    Json,
    /// The native Cap'n Proto socket — what a sandboxed app dials.
    Capnp,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Transport::Json => "json",
            Transport::Capnp => "capnp",
        }
    }
}

/// The one place a local payload becomes a peer publish. Construct with
/// [`PeerPublisher::new`] (network wired) or [`PeerPublisher::disabled`].
/// Both brokers hold the same instance, so "did this edit leave the
/// machine" has one answer per session instead of one per transport.
pub struct PeerPublisher {
    sink: Option<Arc<dyn PeerPublishSink>>,
    /// Set the first time a publish is dropped for no network — keeps
    /// the "collaboration is off" line to one per session.
    warned_no_network: AtomicBool,
}

impl PeerPublisher {
    /// A publisher backed by a live peer network.
    pub fn new(sink: Arc<dyn PeerPublishSink>) -> Arc<Self> {
        Arc::new(Self {
            sink: Some(sink),
            warned_no_network: AtomicBool::new(false),
        })
    }

    /// A publisher with nowhere to publish. Every call is a no-op,
    /// announced once.
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self {
            sink: None,
            warned_no_network: AtomicBool::new(false),
        })
    }

    /// `None` is a legitimate single-machine session, but worth naming
    /// in the log since from inside an app it's indistinguishable from
    /// a peer who never typed.
    pub fn from_network(net: Option<Arc<ShellNetwork>>) -> Arc<Self> {
        if let Some(n) = net {
            tracing::info!(
                "collab outbound: peer sync wired; local edits will be \
                 republished to the room"
            );
            Self::new(n as Arc<dyn PeerPublishSink>)
        } else {
            tracing::warn!(
                "collab outbound: no peer network — this session is \
                 LOCAL ONLY. Nothing typed in any app will reach another \
                 machine. (Peer sync is off, or QOS_PEER_KEY_PATH did not \
                 point somewhere writable.)"
            );
            Self::disabled()
        }
    }

    /// Whether this publisher can reach the network at all.
    pub fn is_wired(&self) -> bool {
        self.sink.is_some()
    }

    /// Returns whether it reached the network, so a caller that wants
    /// to assert delivery can.
    pub fn document_incremental(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        data: Vec<u8>,
    ) -> bool {
        let Some(sink) = self.ready(route, transport, "document increment") else {
            return false;
        };
        sink.app_crdt_incremental(
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.record_id.clone(),
            data,
        );
        true
    }

    /// The remote broker validates against its own channel state before
    /// showing it to anyone — publishing is not vouching.
    pub fn signed_document(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        envelope: SignedOp,
    ) -> bool {
        let Some(sink) = self.ready(route, transport, "signed document") else {
            return false;
        };
        sink.app_crdt_signed(
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.record_id.clone(),
            envelope,
        );
        true
    }

    /// Makes two *separately created* documents one document — sent in
    /// reply to a peer's `Arrived`, never on a commit. Without it, deltas
    /// refer to objects the receiver never saw: the peer's caret moves
    /// over a blank page.
    pub fn document_snapshot(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        data: Vec<u8>,
    ) -> bool {
        let Some(sink) = self.ready(route, transport, "document snapshot") else {
            return false;
        };
        sink.app_crdt_snapshot(
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.record_id.clone(),
            data,
        );
        true
    }

    /// The app id rides along so the receiver can attribute the caret;
    /// the record is dropped since "who else is here" is a room fact,
    /// and a peer editing a different document is still a legitimate audience.
    pub fn presence(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        presence: PeerPresence,
    ) -> bool {
        let Some(sink) = self.ready_quietly(route, transport, "presence") else {
            return false;
        };
        sink.app_presence(route.app_id.clone(), route.cluster_id.clone(), presence);
        true
    }

    /// Routed like any other outbound: a connection that never announced
    /// an address gets no cooperative fetch, since the answer would
    /// have nowhere to land.
    pub fn scrollback_request(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    ) -> bool {
        let Some(sink) = self.ready(route, transport, "scrollback request") else {
            return false;
        };
        sink.scrollback_request(artifact, before_seq, max);
        true
    }

    /// The two refusals every publish shares: no network, or a route
    /// that addresses nobody. "No network" is throttled to once per
    /// publisher; the route refusal is not, since repeats name a
    /// specific misbehaving connection. See [`Self::ready_quietly`] for
    /// the opt-out.
    fn ready(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        what: &str,
    ) -> Option<&Arc<dyn PeerPublishSink>> {
        self.ready_at(route, transport, what, true)
    }

    /// [`Self::ready`] for a payload that repeats (pointer rate) — same
    /// gate, logged at `debug!` so it doesn't bury the warning that matters.
    fn ready_quietly(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        what: &str,
    ) -> Option<&Arc<dyn PeerPublishSink>> {
        self.ready_at(route, transport, what, false)
    }

    fn ready_at(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        what: &str,
        loud: bool,
    ) -> Option<&Arc<dyn PeerPublishSink>> {
        let Some(sink) = self.sink.as_ref() else {
            if loud && !self.warned_no_network.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    transport = transport.as_str(),
                    "collab outbound: dropping every outbound {what} — this \
                     session has no peer network, so edits stay on this \
                     machine. Further drops are not logged.",
                );
            }
            return None;
        };
        // Fail closed on an unaddressable route: publishing an empty or
        // nil cluster would put a frame in a room no receiver can place.
        if !route.is_publishable() {
            if loud {
                tracing::warn!(
                    transport = transport.as_str(),
                    app_id = %route.app_id,
                    cluster_id = %route.cluster_id,
                    "collab outbound: NOT publishing {what} — the sender's \
                     route names no room, so it would reach nobody. An app in \
                     this state is collaborating with itself.",
                );
            } else {
                tracing::debug!(
                    transport = transport.as_str(),
                    app_id = %route.app_id,
                    cluster_id = %route.cluster_id,
                    "collab outbound: not publishing {what} whose route names \
                     no room",
                );
            }
            return None;
        }
        Some(sink)
    }
}

/// A sink that keeps what it was asked to publish, so a test can ask
/// "what route reached the wire" without standing up Iroh. Lives here
/// since both broker transports share this instrument.
#[cfg(test)]
pub mod recording {
    use super::*;
    use std::sync::Mutex;

    /// What actually reached the wire, in order.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Sent {
        Incremental {
            app_id: String,
            cluster_id: String,
            record_id: String,
            data: Vec<u8>,
        },
        Signed {
            app_id: String,
            cluster_id: String,
            record_id: String,
            payload: Vec<u8>,
        },
        Snapshot {
            app_id: String,
            cluster_id: String,
            record_id: String,
            data: Vec<u8>,
        },
        Presence {
            app_id: String,
            cluster_id: String,
            presence: PeerPresence,
        },
        Scrollback {
            before_seq: u64,
            max: u16,
        },
    }

    #[derive(Default)]
    pub struct RecordingSink {
        sent: Mutex<Vec<Sent>>,
    }

    impl RecordingSink {
        /// Everything published so far, oldest first.
        ///
        /// # Panics
        ///
        /// Panics if the internal mutex is poisoned by a prior panic on
        /// another thread.
        pub fn snapshot(&self) -> Vec<Sent> {
            self.sent.lock().unwrap().clone()
        }

        /// Takes everything published so far, leaving the sink empty.
        ///
        /// # Panics
        ///
        /// Panics if the internal mutex is poisoned by a prior panic on
        /// another thread.
        pub fn drain(&self) -> Vec<Sent> {
            std::mem::take(&mut *self.sent.lock().unwrap())
        }
    }

    impl PeerPublishSink for RecordingSink {
        fn app_crdt_incremental(
            &self,
            app_id: String,
            cluster_id: String,
            record_id: String,
            data: Vec<u8>,
        ) {
            self.sent.lock().unwrap().push(Sent::Incremental {
                app_id,
                cluster_id,
                record_id,
                data,
            });
        }

        fn app_crdt_signed(
            &self,
            app_id: String,
            cluster_id: String,
            record_id: String,
            envelope: SignedOp,
        ) {
            self.sent.lock().unwrap().push(Sent::Signed {
                app_id,
                cluster_id,
                record_id,
                payload: envelope.payload,
            });
        }

        fn app_crdt_snapshot(
            &self,
            app_id: String,
            cluster_id: String,
            record_id: String,
            data: Vec<u8>,
        ) {
            self.sent.lock().unwrap().push(Sent::Snapshot {
                app_id,
                cluster_id,
                record_id,
                data,
            });
        }

        fn app_presence(&self, app_id: String, cluster_id: String, presence: PeerPresence) {
            self.sent.lock().unwrap().push(Sent::Presence {
                app_id,
                cluster_id,
                presence,
            });
        }

        fn scrollback_request(&self, _artifact: ArtifactId, before_seq: u64, max: u16) {
            self.sent
                .lock()
                .unwrap()
                .push(Sent::Scrollback { before_seq, max });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::recording::{RecordingSink, Sent};
    use super::*;

    fn wired() -> (Arc<PeerPublisher>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let pubr = PeerPublisher::new(sink.clone() as Arc<dyn PeerPublishSink>);
        (pubr, sink)
    }

    fn presence() -> PeerPresence {
        PeerPresence {
            key: vec![1, 2, 3],
            payload: collab_types::PeerPresencePayload::Arrived,
        }
    }

    #[test]
    fn record_scope_survives_onto_the_wire() {
        let (pubr, sink) = wired();
        let route = CrdtRoute::record("notes", "11111111-1111-4111-8111-111111111111", "doc-7");
        assert!(pubr.document_incremental(&route, Transport::Capnp, vec![9, 9]));
        assert_eq!(
            sink.drain(),
            vec![Sent::Incremental {
                app_id: "notes".into(),
                cluster_id: "11111111-1111-4111-8111-111111111111".into(),
                record_id: "doc-7".into(),
                data: vec![9, 9],
            }],
            "the wire must carry the same route the local fanout used, \
             record included",
        );
    }

    #[test]
    fn a_capnp_cluster_route_publishes_without_an_app_id() {
        let (pubr, sink) = wired();
        let route = CrdtRoute::from_channel_key("22222222-2222-4222-8222-222222222222");
        assert!(pubr.document_incremental(&route, Transport::Capnp, vec![1]));
        assert_eq!(
            sink.drain(),
            vec![Sent::Incremental {
                app_id: String::new(),
                cluster_id: "22222222-2222-4222-8222-222222222222".into(),
                record_id: String::new(),
                data: vec![1],
            }],
        );
    }

    #[test]
    fn a_capnp_record_channel_publishes_its_record() {
        let (pubr, sink) = wired();
        let route =
            CrdtRoute::from_channel_key("33333333-3333-4333-8333-333333333333#record:letter");
        assert!(pubr.document_incremental(&route, Transport::Capnp, vec![2]));
        assert_eq!(
            sink.drain(),
            vec![Sent::Incremental {
                app_id: String::new(),
                cluster_id: "33333333-3333-4333-8333-333333333333".into(),
                record_id: "letter".into(),
                data: vec![2],
            }],
        );
    }

    #[test]
    fn an_unaddressable_route_publishes_nothing() {
        let (pubr, sink) = wired();
        for route in [
            CrdtRoute::cluster(""),
            CrdtRoute::cluster("00000000-0000-0000-0000-000000000000"),
            CrdtRoute::record("notes", "", "doc"),
        ] {
            assert!(
                !pubr.document_incremental(&route, Transport::Json, vec![7]),
                "route {route:?} addresses nobody and must not reach the wire",
            );
            assert!(
                !pubr.document_snapshot(&route, Transport::Json, vec![7]),
                "route {route:?} addresses nobody, so no snapshot either",
            );
            assert!(
                !pubr.presence(&route, Transport::Json, presence()),
                "route {route:?} addresses nobody, so no presence either",
            );
            assert!(
                !pubr.scrollback_request(
                    &route,
                    Transport::Json,
                    ArtifactId(uuid::Uuid::nil()),
                    1,
                    1
                ),
                "route {route:?} addresses nobody, so no history request either",
            );
        }
        assert!(sink.drain().is_empty());
    }

    #[test]
    fn a_disabled_publisher_never_reaches_the_wire() {
        let pubr = PeerPublisher::disabled();
        let route = CrdtRoute::record("notes", "44444444-4444-4444-8444-444444444444", "d");
        assert!(!pubr.is_wired());
        assert!(!pubr.document_incremental(&route, Transport::Capnp, vec![1]));
        assert!(!pubr.document_snapshot(&route, Transport::Capnp, vec![1]));
        assert!(!pubr.presence(&route, Transport::Capnp, presence()));
        assert!(!pubr.scrollback_request(
            &route,
            Transport::Capnp,
            ArtifactId(uuid::Uuid::nil()),
            5,
            3
        ));
    }

    #[test]
    fn scrollback_requests_carry_their_bounds() {
        let (pubr, sink) = wired();
        let route = CrdtRoute::record("notes", "55555555-5555-4555-8555-555555555555", "d");
        assert!(pubr.scrollback_request(
            &route,
            Transport::Capnp,
            ArtifactId(uuid::Uuid::nil()),
            42,
            9
        ));
        assert_eq!(
            sink.drain(),
            vec![Sent::Scrollback {
                before_seq: 42,
                max: 9
            }],
        );
    }

    #[test]
    fn a_snapshot_carries_its_record_onto_the_wire() {
        let (pubr, sink) = wired();
        let route = CrdtRoute::record("words", "66666666-6666-4666-8666-666666666666", "letter");
        assert!(pubr.document_snapshot(&route, Transport::Capnp, vec![4, 2]));
        assert_eq!(
            sink.drain(),
            vec![Sent::Snapshot {
                app_id: "words".into(),
                cluster_id: "66666666-6666-4666-8666-666666666666".into(),
                record_id: "letter".into(),
                data: vec![4, 2],
            }],
        );
    }

    #[test]
    fn presence_reaches_the_room_and_leaves_the_record_behind() {
        let (pubr, sink) = wired();
        let route = CrdtRoute::record("words", "77777777-7777-4777-8777-777777777777", "letter");
        let presence = presence();
        assert!(pubr.presence(&route, Transport::Capnp, presence.clone()));
        assert_eq!(
            sink.drain(),
            vec![Sent::Presence {
                app_id: "words".into(),
                cluster_id: "77777777-7777-4777-8777-777777777777".into(),
                presence,
            }],
        );
    }

    #[test]
    fn both_transports_publish_through_one_instance() {
        // The publisher never branches on `Transport` for anything but
        // the wording of a refusal.
        let (pubr, sink) = wired();
        let route = CrdtRoute::record("notes", "88888888-8888-4888-8888-888888888888", "doc");
        assert!(pubr.document_incremental(&route, Transport::Json, vec![5]));
        assert!(pubr.document_incremental(&route, Transport::Capnp, vec![5]));
        let sent = sink.drain();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[0], sent[1],
            "the same route and payload must reach the wire identically              whichever socket the app dialled",
        );
    }
}
