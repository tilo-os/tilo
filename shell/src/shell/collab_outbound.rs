//! The broker's outbound half: handing a locally-authored payload to
//! the peer network.
//!
//! Both collaboration transports — the legacy JSON dialect in
//! [`crate::shell::collab_broker`] and the native Cap'n Proto one in
//! [`crate::shell::collab_broker_capnp`] — fan a local frame out to
//! other apps *on this machine* through the shared
//! [`crate::shell::collab_broker::PeerTable`]. Reaching another
//! machine is a second step, and this module is that step, once, for
//! both of them.
//!
//! It exists because there used to be two answers to "does a local
//! edit leave this host": the JSON reader loop republished every
//! `SyncIncremental` over libp2p, and the capnp server did not
//! republish anything at all. Sandboxed apps dial the capnp socket, so
//! everything a launched app typed stopped at the machine boundary —
//! and stopped silently, because a peer who sees nothing cannot tell
//! "not delivered" from "nobody typed". Two implementations of "decide
//! the route, then publish" is how those two answers came to differ;
//! one implementation is how they stop differing.
//!
//! ## What goes on the wire is what the local fanout used
//!
//! A publish is addressed by the same [`CrdtRoute`] the caller just
//! handed to `fanout_to_route`. The remote broker rebuilds that route
//! from the wire fields and runs it through the same
//! [`CrdtRoute::delivers_to`] its local frames go through, so a
//! document that is record-scoped here stays record-scoped there.
//!
//! Scope follows the payload, not the transport:
//!
//! - **Documents** — CRDT increments (signed or not) and whole-state
//!   snapshots — publish on the route verbatim, records and all. A
//!   document is what people share, and which document it is is part
//!   of its address.
//! - **Presence** reaches the room, not the record. Callers hand over
//!   the same un-widened route, and the record is dropped here because
//!   the presence frame on the wire has no record field: "who else is
//!   here" is a fact about the room, so a peer editing a different
//!   document in the same room is still a legitimate audience for it.
//!
//! Beyond that per-payload rule this module never widens or narrows a
//! route it is given. Handing it a cluster-scoped route publishes
//! cluster-scoped; that decision belongs to the caller that knows what
//! kind of payload it holds.
//!
//! ## Refusals are loud
//!
//! Collaboration failures are invisible by construction: "the network
//! is off", "this connection never announced an address" and "the
//! other person is not typing" all render as an empty screen. So every
//! path here that declines to publish says so through
//! [`tracing::warn!`] — once per publisher for the steady-state
//! conditions, so a session that runs for an hour with peer sync off
//! leaves exactly one line to find rather than one per keystroke.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collab_types::{ArtifactId, SignedOp};

use crate::shell::collab_broker::CrdtRoute;
use crate::shell::network::ShellNetwork;

/// The network operations the broker's outbound half needs.
///
/// A trait rather than a bare [`ShellNetwork`] so the routing decision
/// can be tested against a recording sink: the thing worth asserting
/// is *which route reached the wire*, and standing up libp2p to find
/// that out would test the transport instead of the decision.
pub trait PeerPublishSink: Send + Sync {
    /// Republish an unsigned CRDT increment to the peers in a room.
    fn app_crdt_incremental(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    );

    /// Republish a signed CRDT envelope to the peers in a room.
    fn app_crdt_signed(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: SignedOp,
    );

    /// Republish one document's whole state to the peers in a room.
    fn app_crdt_snapshot(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    );

    /// Republish a presence frame to the peers in a room. The frame is
    /// a serialised `CollabRequest::Presence`, which is what the
    /// receiving drain decodes — so a caret published over capnp and
    /// one published over JSON are the same bytes on the wire.
    fn app_presence(&self, app_id: String, cluster_id: String, frame: Vec<u8>);

    /// Ask a room whether anyone still holds a range of history.
    ///
    /// `cluster_id` is the room to ask. An artifact id names an artifact
    /// and not a room, so without it the only reachable question is one
    /// put to everybody.
    fn scrollback_request(
        &self,
        cluster_id: &str,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    );

    /// Take content an app just stored, and offer it to the room.
    ///
    /// This is the only way a document's *bytes* leave the process that made
    /// them. An app's blobs live in its own sandbox — the launcher gives every
    /// app a private `HOME` and `XDG_CACHE_HOME` — so the shell cannot go and
    /// read them, and until the app hands them over the shell holds a
    /// reference to a photograph it does not have and can send nobody.
    fn offer_blob(&self, cluster_id: &str, hash: &str, bytes: &[u8]) -> bool;

    /// Content this machine already holds, if any, so an app can be answered
    /// without troubling the room.
    fn blob(&self, hash: &str) -> Option<Vec<u8>>;

    /// Ask the room for content an app needs and this machine lacks.
    fn want_blob(&self, cluster_id: &str, hash: &str);

    /// Tell a room that a participant opened an application in it.
    fn app_opened(&self, app_id: String, cluster_id: String, with: Option<String>, who: String);
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

    fn app_presence(&self, app_id: String, cluster_id: String, frame: Vec<u8>) {
        self.publish_app_presence(app_id, cluster_id, frame);
    }

    fn scrollback_request(
        &self,
        cluster_id: &str,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        self.publish_scrollback_request(cluster_id, artifact, before_seq, max);
    }

    fn offer_blob(&self, cluster_id: &str, hash: &str, bytes: &[u8]) -> bool {
        self.offer_local_blob(cluster_id, hash, bytes)
    }

    fn blob(&self, hash: &str) -> Option<Vec<u8>> {
        self.holds_blob(hash)
    }

    fn want_blob(&self, cluster_id: &str, hash: &str) {
        ShellNetwork::want_blob(self, cluster_id, hash);
    }

    fn app_opened(&self, app_id: String, cluster_id: String, with: Option<String>, who: String) {
        self.publish_app_opened(app_id, cluster_id, with, who);
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

/// The one place a local payload becomes a peer publish.
///
/// Construct with [`PeerPublisher::new`] (network wired) or
/// [`PeerPublisher::disabled`]. Both brokers hold the same instance,
/// so "did this edit leave the machine" has one answer per session
/// instead of one per transport.
pub struct PeerPublisher {
    sink: Option<Arc<dyn PeerPublishSink>>,
    /// Set the first time a publish is dropped because there is no
    /// network. Keeps the "collaboration is off" line to one per
    /// session rather than one per keystroke.
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

    /// A publisher with nowhere to publish — the shell is running
    /// without peer sync. Every call is a no-op, announced once.
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self {
            sink: None,
            warned_no_network: AtomicBool::new(false),
        })
    }

    /// Build from the shell's optional network handle. `None` is a
    /// single-machine session, which is a legitimate configuration —
    /// but one worth naming in the log, because from inside an app it
    /// is indistinguishable from a peer who never typed.
    pub fn from_network(net: Option<Arc<ShellNetwork>>) -> Arc<Self> {
        match net {
            Some(n) => {
                tracing::info!(
                    "collab outbound: peer sync wired; local edits will be \
                     republished to the room"
                );
                Self::new(n as Arc<dyn PeerPublishSink>)
            }
            None => {
                tracing::warn!(
                    "collab outbound: no peer network — this session is \
                     LOCAL ONLY. Nothing typed in any app will reach another \
                     machine. (Peer sync is off, or QOS_PEER_KEY_PATH did not \
                     point somewhere writable.)"
                );
                Self::disabled()
            }
        }
    }

    /// Whether this publisher can reach the network at all.
    pub fn is_wired(&self) -> bool {
        self.sink.is_some()
    }

    /// Republish a document increment on the route it was fanned out
    /// locally. Returns whether it reached the network, so a caller
    /// that wants to assert delivery can.
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

    /// Republish a signed document envelope on the route it was fanned
    /// out locally. The remote broker validates it against its own
    /// channel state before showing it to anyone, exactly as this host
    /// did — publishing is not vouching.
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

    /// Republish one document's whole state on the route it was fanned
    /// out locally.
    ///
    /// This is the frame that makes two *separately created* documents
    /// one document. The toolkit sends it in reply to a peer's
    /// `Arrived`, never on a commit, so publishing it answers a
    /// question rather than announcing into the room — and
    /// `ShellNetwork::publish_app_crdt_snapshot` is where that "is
    /// anyone actually waiting" gate and the gossip size cap live, so
    /// both transports are bounded by one rule rather than two.
    ///
    /// Without it the deltas cross correctly and refer to objects the
    /// receiving document has never seen: the peer's caret moves over
    /// a blank page.
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

    /// Republish a presence frame — a caret, a selection, an arrival —
    /// to the room the route names.
    ///
    /// Pass the route **un-widened**: the app id rides along so the
    /// receiving side can attribute the caret, and the record is
    /// dropped here rather than by the caller because the wire frame
    /// (`NeighbourMsg::AppPresence`) has no record field at all. "Who
    /// else is here" is a fact about the room, so a peer editing a
    /// different document in the same room is still a legitimate
    /// audience — the room scope is the payload's, not the caller's.
    ///
    /// `frame` is the serialised `CollabRequest::Presence` the
    /// receiving drain decodes, so the two transports put identical
    /// bytes on the wire.
    ///
    /// Without this a peer's caret stopped at the machine that produced
    /// it, and every collaborator looked like they were doing nothing.
    pub fn presence(&self, route: &CrdtRoute, transport: Transport, frame: Vec<u8>) -> bool {
        let Some(sink) = self.ready_quietly(route, transport, "presence") else {
            return false;
        };
        sink.app_presence(route.app_id.clone(), route.cluster_id.clone(), frame);
        true
    }

    /// Tell a room that a participant opened an application in it.
    ///
    /// Addressed to the room and not to a document, so the route is the
    /// cluster's — an open is a fact about the workspace, the way presence is,
    /// and the application named in it is the *subject* of the announcement
    /// rather than the sender of it. That is also why `app_id` is a parameter
    /// here where every other publish takes it off the route: the connection
    /// that asked is some other application entirely, most often an agent
    /// with no window at all.
    ///
    /// Returns whether it reached the network, so a caller can say "this
    /// stayed on one machine" rather than leaving a collaborator wondering
    /// why their workspace never grew the window.
    pub fn app_opened(
        &self,
        room: &str,
        transport: Transport,
        app_id: &str,
        with: Option<String>,
        who: &str,
    ) -> bool {
        let route = CrdtRoute::cluster(room);
        let Some(sink) = self.ready(&route, transport, "application open") else {
            return false;
        };
        sink.app_opened(
            app_id.to_string(),
            route.cluster_id.clone(),
            with,
            who.to_string(),
        );
        true
    }

    /// Take a document's content from the app that made it and offer it to
    /// the room.
    ///
    /// Routed like every other outbound: an app that never announced an
    /// address has no room to offer content to. Unlike the others, a refusal
    /// here is reported by the caller — a picture that does not cross has no
    /// symptom other than a placeholder that never resolves.
    pub fn offer_blob(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        hash: &str,
        bytes: &[u8],
    ) -> bool {
        // Remember which application these bytes came from, before anything
        // about routing can fail. This is the only moment the pair is known:
        // content is addressed by its hash from here on, and an action that
        // later names that hash carries no record of who produced it. See
        // `crate::shell::action_flight` for what the pair is then used for.
        //
        // Noted even when the offer does not reach the room. Whether a peer
        // can be told is a fact about the network; that this app made this
        // content is a fact about this machine, and it is the second one
        // that decides what a person watching this screen should see.
        crate::shell::action_flight::note_offer(hash, &route.app_id);
        let Some(sink) = self.ready(route, transport, "document content") else {
            return false;
        };
        sink.offer_blob(&route.cluster_id, hash, bytes)
    }

    /// Content this machine already holds, so an app's request can be answered
    /// without a round trip through the room.
    pub fn blob(&self, hash: &str) -> Option<Vec<u8>> {
        self.sink.as_ref()?.blob(hash)
    }

    /// Ask the room for content an app is waiting on.
    pub fn want_blob(&self, route: &CrdtRoute, transport: Transport, hash: &str) -> bool {
        let Some(sink) = self.ready(route, transport, "content request") else {
            return false;
        };
        sink.want_blob(&route.cluster_id, hash);
        true
    }

    /// Ask the room for a range of history the local op log no longer
    /// holds. Routed like any other outbound: a connection that never
    /// announced an address gets no cooperative fetch, because the
    /// answer would have nowhere to land.
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
        sink.scrollback_request(&route.cluster_id, artifact, before_seq, max);
        true
    }

    /// The two refusals every publish shares: no network, or a route
    /// that addresses nobody. Both are logged; the "no network" line is
    /// throttled to once per publisher because it is a session-long
    /// state, the route refusal is not, because it names a specific
    /// misbehaving connection and repeats are the signal.
    ///
    /// See [`Self::ready_quietly`] for the one payload that has to opt
    /// out of the second half of that.
    fn ready(
        &self,
        route: &CrdtRoute,
        transport: Transport,
        what: &str,
    ) -> Option<&Arc<dyn PeerPublishSink>> {
        self.ready_at(route, transport, what, true)
    }

    /// [`Self::ready`] for a payload that repeats. A pointer publishes
    /// at cursor rate, so an unaddressable route would put a warning on
    /// every frame and bury the one that matters; a lost caret is also
    /// cosmetic where a lost edit is not. Same gate, same decision,
    /// logged at `debug!`.
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
        // Fail closed on an unaddressable route, and say which
        // connection did it. The wire carries the route verbatim, so
        // publishing an empty or nil cluster would put a frame in the
        // room that no receiver can place — the remote broker drops it
        // and neither side has a record of why.
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

/// A sink that keeps what it was asked to publish instead of
/// publishing it.
///
/// The question a test about this module wants to ask is "what route
/// reached the wire", and standing up libp2p to ask it would exercise
/// the transport instead. Lives here rather than in one test module
/// because both broker transports have the same question to answer and
/// should answer it against the same instrument.
#[cfg(test)]
pub mod recording {
    use super::*;
    use std::sync::Mutex;

    /// What actually reached the wire, in order.
    #[derive(Debug, Clone, PartialEq, Eq)]
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
            frame: Vec<u8>,
        },
        Scrollback {
            before_seq: u64,
            max: u16,
        },
        /// Content an app handed over for the room.
        Blob {
            cluster_id: String,
            hash: String,
            bytes: Vec<u8>,
        },
        /// A request for content this machine does not hold.
        BlobWanted {
            cluster_id: String,
            hash: String,
        },
        /// An application a participant opened in the room.
        AppOpened {
            app_id: String,
            cluster_id: String,
            with: Option<String>,
            who: String,
        },
    }

    #[derive(Default)]
    pub struct RecordingSink {
        sent: Mutex<Vec<Sent>>,
        /// Content this sink was handed, so it can answer for it the way a
        /// real machine's store does.
        blobs: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl RecordingSink {
        /// Everything published so far, oldest first.
        pub fn snapshot(&self) -> Vec<Sent> {
            self.sent.lock().unwrap().clone()
        }

        /// Take everything published so far, leaving the log empty.
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
                payload: envelope.payload.clone(),
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

        fn app_presence(&self, app_id: String, cluster_id: String, frame: Vec<u8>) {
            self.sent.lock().unwrap().push(Sent::Presence {
                app_id,
                cluster_id,
                frame,
            });
        }

        fn scrollback_request(
            &self,
            _cluster_id: &str,
            _artifact: ArtifactId,
            before_seq: u64,
            max: u16,
        ) {
            self.sent
                .lock()
                .unwrap()
                .push(Sent::Scrollback { before_seq, max });
        }

        fn offer_blob(&self, cluster_id: &str, hash: &str, bytes: &[u8]) -> bool {
            // Records what it was handed *and* keeps it, so `blob` can answer
            // for it afterwards. A sink that took content and then denied
            // holding it would let a test prove a delivery that could not
            // happen on a real machine.
            self.blobs
                .lock()
                .unwrap()
                .insert(hash.to_string(), bytes.to_vec());
            self.sent.lock().unwrap().push(Sent::Blob {
                cluster_id: cluster_id.to_string(),
                hash: hash.to_string(),
                bytes: bytes.to_vec(),
            });
            true
        }

        fn blob(&self, hash: &str) -> Option<Vec<u8>> {
            self.blobs.lock().unwrap().get(hash).cloned()
        }

        fn want_blob(&self, cluster_id: &str, hash: &str) {
            self.sent.lock().unwrap().push(Sent::BlobWanted {
                cluster_id: cluster_id.to_string(),
                hash: hash.to_string(),
            });
        }

        fn app_opened(&self, app_id: String, cluster_id: String, with: Option<String>, who: String) {
            self.sent.lock().unwrap().push(Sent::AppOpened {
                app_id,
                cluster_id,
                with,
                who,
            });
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
        // `joinChannel` carries no app identity, so a capnp route is
        // cluster-scoped. That is a legitimate address, not a missing
        // one — refusing it here is exactly the bug this module exists
        // to close.
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
            // Every payload kind fails closed on the same test. A
            // snapshot or a caret published into a room that does not
            // exist is as undeliverable as a delta, and the nil-uuid
            // case would put all of them in one room shared by every
            // unminted workspace on every machine.
            assert!(
                !pubr.document_incremental(&route, Transport::Json, vec![7]),
                "route {route:?} addresses nobody and must not reach the wire",
            );
            assert!(
                !pubr.document_snapshot(&route, Transport::Json, vec![7]),
                "route {route:?} addresses nobody, so no snapshot either",
            );
            assert!(
                !pubr.presence(&route, Transport::Json, vec![7]),
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
        assert!(!pubr.presence(&route, Transport::Capnp, vec![1]));
        assert!(!pubr.scrollback_request(&route, Transport::Capnp, ArtifactId(uuid::Uuid::nil()), 5, 3));
    }

    /// An open is addressed to the room, and names the application it is
    /// about rather than the one that asked for it.
    #[test]
    fn an_open_reaches_the_room_naming_the_application_it_opened() {
        let (pubr, sink) = wired();
        let room = "77777777-7777-4777-8777-777777777777";
        assert!(pubr.app_opened(
            room,
            Transport::Json,
            "dev.qos.photos",
            Some("b3f00d".to_string()),
            "Claude",
        ));
        assert_eq!(
            sink.drain(),
            vec![Sent::AppOpened {
                app_id: "dev.qos.photos".into(),
                cluster_id: room.into(),
                with: Some("b3f00d".into()),
                who: "Claude".into(),
            }],
            "the room, the application, the content it was bound to and who \
             asked all have to cross — a receiver missing any one of them \
             cannot decide the open, only guess at it",
        );
    }

    /// The same fail-closed rule every other payload gets. A room that does
    /// not exist is not a smaller audience, it is the wrong one: the nil uuid
    /// is shared by every workspace whose durable identity is unminted, on
    /// every machine at once.
    #[test]
    fn an_open_in_no_room_reaches_nobody() {
        let (pubr, sink) = wired();
        for room in ["", "00000000-0000-0000-0000-000000000000"] {
            assert!(
                !pubr.app_opened(room, Transport::Json, "dev.qos.photos", None, "Claude"),
                "room {room:?} addresses nobody and must not reach the wire",
            );
        }
        assert!(sink.drain().is_empty());
        let off = PeerPublisher::disabled();
        assert!(!off.app_opened(
            "77777777-7777-4777-8777-777777777777",
            Transport::Json,
            "dev.qos.photos",
            None,
            "Claude",
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
        // A whole document merged into the wrong record cannot be taken
        // back out, so the record matters more here than on a delta,
        // not less.
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
        // The app id rides along so the far side can attribute the
        // caret; the record does not, because a peer editing another
        // document in this room is still someone you should be able to
        // see.
        let (pubr, sink) = wired();
        let route = CrdtRoute::record("words", "77777777-7777-4777-8777-777777777777", "letter");
        assert!(pubr.presence(&route, Transport::Capnp, vec![1, 2, 3]));
        assert_eq!(
            sink.drain(),
            vec![Sent::Presence {
                app_id: "words".into(),
                cluster_id: "77777777-7777-4777-8777-777777777777".into(),
                frame: vec![1, 2, 3],
            }],
        );
    }

    #[test]
    fn both_transports_publish_through_one_instance() {
        // The bug this module exists to close was two answers to "does
        // a local edit leave this machine" — one per transport. The
        // publisher does not branch on `Transport` for anything but the
        // wording of a refusal, and this is the assertion that keeps it
        // that way.
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
