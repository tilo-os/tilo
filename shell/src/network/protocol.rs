//! Peer-sync wire protocol — message envelope shared by all neighbour transports.
//!
//! Phase 1 Task 1.1 of the peer-sync end-to-end plan: a single tagged enum
//! carrying every kind of cross-host payload (CRDT sync, presence, surface
//! frames, remote input, capability negotiation). Concrete transport bindings
//! (libp2p gossipsub, in-process [`crate::network::hub::Hub`]) operate purely
//! on `NeighbourMsg`; they never inspect the payload.

use serde::{Deserialize, Serialize};

/// Peer identifier — opaque numeric handle assigned per session.
pub type PeerId = u64;

/// Wayland surface identifier — local handle used for streaming/input routing.
pub type SurfaceId = u64;

/// Presence channel key — namespaces presence updates (e.g. cursor, selection).
pub type PresenceKey = String;

/// Tagged envelope for everything that crosses the neighbour boundary.
///
/// `f64` lives inside [`NeighbourMsg::PresenceMoved`] so we cannot derive
/// [`Eq`]; [`PartialEq`] is sufficient for tests and dispatch logic.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub enum NeighbourMsg {
    /// "Catch me up on this room": deliver every Automerge change to
    /// `room` after the included clock.
    ///
    /// `room` is a [`crate::shell::state::RoomId`] rendered as a uuid
    /// string — the durable identity of one collaboration room, the same
    /// value an invite token carries. It is what scopes the answer: a
    /// request names one room and is answered with that room's document
    /// and nothing else, so being in one room on a host never yields the
    /// others.
    ///
    /// An empty (or unparseable) `room` names no room. It is not a
    /// wildcard: it is answered with no document at all. The transport
    /// emits one on peer-join, before it knows what the peer is in, and
    /// receiving it is what prompts a replica to name its own rooms.
    SyncReq { since: Vec<u8>, room: String },
    /// Response carrying a batch of opaque Automerge change blobs for one
    /// room.
    ///
    /// `room` says which room's document the blobs belong to, so the
    /// receiver merges them into that room and no other. The blobs
    /// themselves also carry the room's identity, and the receiver checks
    /// the two agree.
    SyncResp { changes: Vec<Vec<u8>>, room: String },
    /// Peer's presence cursor moved within `key`'s coordinate space.
    ///
    /// `peer` is the publishing **session**, and that is the whole
    /// identity of the cursor: a session republishes the pointers of
    /// every app running under it, so one person driving two windows
    /// is one cursor on every other host rather than two. The
    /// per-connection ids the local broker mints never leave the host
    /// that minted them.
    ///
    /// `user` is who that session says is sitting at it
    /// ([`crate::shell::state::local_user_id`]). It is a claim by the
    /// publisher, not something the receiver infers, and it is what a
    /// peer's cursor is labelled with. Empty when the publisher did
    /// not say — the receiver then shows the session handle rather
    /// than inventing a name.
    PresenceMoved {
        peer: PeerId,
        #[serde(default)]
        user: String,
        key: PresenceKey,
        point: (f64, f64),
        ts: u64,
    },
    /// Peer's presence cursor disappeared from `key`'s coordinate space.
    PresenceCleared { peer: PeerId, key: PresenceKey },
    /// Encoded H.264 frame for a shared surface.
    ///
    /// `ts` is the owner-side capture timestamp in **monotonic nanoseconds**
    /// (as read from `Instant::now()` translated to `u64` at session
    /// start). Receivers subtract their own monotonic-now to compute
    /// motion-to-photon latency; a wall-clock (`SystemTime`) offset would
    /// suffer NTP drift on the WAN case.
    SurfaceFrame {
        surface: SurfaceId,
        encoded_h264: Vec<u8>,
        ts: u64,
    },
    /// Explicit start-of-stream announcement for a shared surface.
    ///
    /// Distinct from `SurfaceFrame` so late-joining peers can build a
    /// decoder / [`crate::shell::peer_view::PeerView`] with the correct
    /// dimensions *before* the first NALU arrives — the previous "peel
    /// dimensions off the first decode" path meant one dropped frame per
    /// share. Codec is negotiated up-front: `"h264"` in v1, but the field
    /// leaves room for `"av1"` / `"h265"` when hardware paths land.
    SurfaceStreamStart {
        surface: SurfaceId,
        /// Hashed libp2p PeerId of the owner (the machine running the
        /// Flatpak app). Distinguishes multiple concurrent shares in
        /// the same workspace.
        source_peer: PeerId,
        /// Natural width in pixels the encoder produces.
        width: u32,
        /// Natural height in pixels the encoder produces.
        height: u32,
        /// Codec label; v1 always `"h264"`. Receivers that don't
        /// recognise the codec ignore the stream + log.
        codec: String,
        /// Owner-side timestamp (monotonic nanoseconds).
        ts: u64,
        /// Sender's monotonic-clock reading at the moment the message
        /// left the encode task. Same origin as [`Self::SurfaceFrame::ts`]
        /// on this session.
        ///
        /// The receiver captures its own `Instant::now()` when it
        /// decodes the message and derives the sender↔receiver clock
        /// offset:
        /// `offset = receiver_now_ns - sender_monotonic_ns_at_send`.
        /// It then translates every subsequent per-frame `ts` into the
        /// receiver's clock domain via
        /// [`crate::diagnostics::PeerSurfaceMetrics::end_to_end_latency_ns`],
        /// yielding real motion-to-photon numbers even across hosts
        /// whose monotonic origins are (arbitrarily) offset from each
        /// other.
        ///
        /// `#[serde(default)]` so this field is wire-compatible with
        /// pre-handshake peers: those emit `sender_monotonic_ns_at_send
        /// = 0`, which
        /// [`crate::diagnostics::PeerSurfaceMetrics::set_clock_offset`]
        /// treats as "skip cross-host latency measurements" rather than
        /// stamping a bogus offset. The rest of the pipeline degrades
        /// gracefully — the OSD shows "—" for cross-host latency and
        /// same-host / loopback measurements (which are trivially at
        /// offset zero) still work.
        #[serde(default)]
        sender_monotonic_ns_at_send: u64,
        /// Colorimetry the encoder tags the bitstream with. `None` on
        /// pre-HDR peers → receiver assumes SDR BT.709 (matches the
        /// v1 behaviour), so this field is wire-compatible.
        #[serde(default)]
        colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
        /// HDR10 static metadata (SMPTE ST 2086 mastering + CTA-861.3
        /// content light) for HDR pipelines. `None` on SDR streams and
        /// on pre-HDR peers. The receiver's tone-mapper uses this to
        /// size compression correctly on SDR displays.
        #[serde(default)]
        hdr_static_metadata: Option<crate::collab::hdr_metadata::Hdr10Metadata>,
        /// Bit depth of the codec's target pixel format. `0` (the
        /// serde-default) means unspecified — receivers infer 8-bit
        /// from `codec = "h264"` on the wire; explicit values are 8 or
        /// 10 for HEVC Main10.
        #[serde(default)]
        bit_depth: u8,
    },
    /// Explicit end-of-stream — the owner tore down the share, or the
    /// captured surface unmapped.
    ///
    /// Receivers drop their PeerView + decoder on receipt. Implicit
    /// end-of-stream (owner disappears from gossipsub) still works via
    /// the heartbeat eviction path; this variant makes the graceful
    /// close explicit so we don't have to wait 15 s for eviction.
    SurfaceStreamEnd {
        surface: SurfaceId,
        source_peer: PeerId,
        /// Owner-side timestamp (monotonic nanoseconds).
        ts: u64,
    },
    /// Serialized input event from `peer` targeting `surface`.
    RemoteInput {
        peer: PeerId,
        surface: SurfaceId,
        event: Vec<u8>,
    },
    /// `peer` would like permission `capability`.
    PermissionRequest { peer: PeerId, capability: String },
    /// `peer` was granted permission `capability`.
    PermissionGrant { peer: PeerId, capability: String },
    /// Periodic liveness beacon — Actor publishes one per joined workspace
    /// every 5 s; receivers track arrival time and evict peers silent for
    /// 15 s. Per-message `peer` is the publisher's libp2p PeerId hashed to
    /// u64; `ts` is unix-millis.
    Heartbeat { peer: PeerId, ts: u64 },
    /// App-level CRDT incremental — a peer-to-peer bridge for the
    /// app-side Automerge incremental change blobs that flow through
    /// the local `QUICK_COLLAB_SOCK` broker. Distinct from
    /// [`NeighbourMsg::SyncResp`], which carries shell-side
    /// `WorkspaceTree` changes; this variant ships the bytes the
    /// `quick-ui` toolkit's `ui::collaboration::CollabRequest::SyncIncremental`
    /// produces, opaque to the shell.
    ///
    /// Two laptops with the broker wired up exchange these so two
    /// terminals (or any pair of apps) running on different hosts
    /// share their CRDT state via libp2p. The remote-receive path
    /// fans the payload into every locally-connected app as a
    /// `CollabEvent::CRDTIncremental` — same translation the broker's
    /// local fan-out does.
    AppCrdtIncremental {
        peer: PeerId,
        /// App-id from the originating `CollabRequest::Hello`. The
        /// receiving broker routes on it: only local apps that
        /// announced the same `(app_id, cluster_id)` pair are handed
        /// the payload. An empty value addresses nobody.
        app_id: String,
        /// Cluster id from the originating Hello. Second half of the
        /// routing key.
        cluster_id: String,
        /// Record id from the originating Hello — which *document*
        /// these changes belong to, empty for an app whose state is
        /// still one document per `(app, cluster)`.
        ///
        /// Carried on the wire because the receiving shell routes on
        /// it: without it a peer's per-document deltas would arrive
        /// unattributed and land in whichever document the receiving
        /// app happened to have open. `#[serde(default)]` so a frame
        /// from a peer that predates records still parses.
        #[serde(default)]
        record_id: String,
        /// Opaque Automerge incremental change blob, passed through
        /// untouched.
        data: Vec<u8>,
    },
    /// Phase 3 of the spaces+channels spec: a signed CRDT envelope
    /// crossing the libp2p boundary. Recipients route the envelope
    /// into their local broker, which runs the validation pipeline
    /// (`collab_types::validate_inbound_op`) before fanning the
    /// payload to any locally-connected app.
    ///
    /// The legacy `AppCrdtIncremental` variant is kept alongside this
    /// one so legacy peers on the same mesh continue to publish + see
    /// unsigned updates while their owners migrate. A peer that has
    /// upgraded keeps emitting `AppCrdtIncrementalSigned`; receiving
    /// shells fall back to the legacy fan-out for `AppCrdtIncremental`
    /// (no validation, advisory only).
    ///
    /// The envelope is boxed so the `NeighbourMsg` enum's stack size
    /// stays in the same ballpark as the other variants. Postcard's
    /// on-wire shape ignores the Box — it sees only the underlying
    /// `SignedOp` bytes.
    AppCrdtIncrementalSigned {
        /// Hashed libp2p PeerId of the publisher. Mirrors the `peer`
        /// field on the legacy variant; used by receiving shells to
        /// filter out self-publishes when they observe their own
        /// gossipsub echoes.
        peer: PeerId,
        /// App-id of the originating app, same routing key as on
        /// [`NeighbourMsg::AppCrdtIncremental`]. `#[serde(default)]`
        /// so an envelope from a peer that predates routing still
        /// parses — it arrives unaddressed, and an unaddressed
        /// envelope is dropped rather than shown to every local app.
        #[serde(default)]
        app_id: String,
        /// Cluster id of the originating app. Second half of the
        /// routing key; same backward-compatible default.
        #[serde(default)]
        cluster_id: String,
        /// Record id of the originating document — the third
        /// component of the routing key. Same backward-compatible
        /// default; empty means the sender announced no record.
        #[serde(default)]
        record_id: String,
        /// The signed-op envelope. The validator on the receiving
        /// shell consults its channel state to decide whether to
        /// fan the payload out to local apps or to drop it with an
        /// audit-log entry.
        envelope: Box<collab_types::SignedOp>,
    },
    /// App-level presence crossing the host boundary — a peer's caret,
    /// selection, pointer or just-typed run inside a shared document.
    ///
    /// The counterpart to [`NeighbourMsg::AppCrdtIncremental`], and it
    /// has to be its own variant rather than reusing
    /// [`NeighbourMsg::PresenceMoved`]: that one carries a point in a
    /// named coordinate space, which is all a pointer needs and none of
    /// what a caret needs. An app's presence is a
    /// `ui::collaboration::PresenceKey` — a widget path, a tab, a
    /// broadcast view — paired with a `PresencePayload` that may be a
    /// caret offset, a selection range or an authorship run. Flattening
    /// those into an `(x, y)` loses exactly the information the
    /// receiving app renders.
    ///
    /// So the originating `CollabRequest::Presence` frame rides across
    /// serialised and otherwise untouched, the same way the CRDT blob
    /// does. The shell does not interpret either.
    ///
    /// # Routed at cluster scope, not record scope
    ///
    /// Deliberately carries no `record_id`. The local broker fans
    /// presence with `CrdtRoute::to_cluster_scope` — every app in the
    /// room is a legitimate audience for "who else is here", where a
    /// document delta is not. Adding a record here would make remote
    /// presence narrower than local presence, so a peer would appear in
    /// the room to people on their own machine and be invisible to
    /// everyone else.
    AppPresence {
        /// Hashed libp2p PeerId of the publisher. Receiving shells drop
        /// their own gossipsub echoes on it, and it is what the remote
        /// peer is attributed to once the frame reaches an app.
        peer: PeerId,
        /// App-id of the originating app — first half of the delivery
        /// filter, as on the CRDT variants.
        #[serde(default)]
        app_id: String,
        /// Cluster id (the room, as a uuid string) the presence belongs
        /// to. The receiving shell refuses a room it is not in before
        /// the frame reaches any app.
        #[serde(default)]
        cluster_id: String,
        /// The originating `CollabRequest::Presence`, serde-encoded.
        /// Opaque to every hop between the two apps.
        frame: Vec<u8>,
    },
    /// App-level CRDT **snapshot** — one document's whole state, sent
    /// so a peer that has just joined can adopt a document it has no
    /// history for.
    ///
    /// # Why a whole document ever crosses
    ///
    /// [`NeighbourMsg::AppCrdtIncremental`] carries a change that names
    /// its dependencies by hash. A receiver that never saw those
    /// dependencies cannot apply it — automerge parks it — so two apps
    /// that each *created* a document rather than opening a shared one
    /// exchange deltas forever without converging: the bytes arrive,
    /// reference objects the receiving document has never seen, and
    /// land nowhere its widgets read. Something has to carry the state
    /// the deltas are relative to, once, and that is this variant.
    ///
    /// # Sent on arrival, not on every commit
    ///
    /// The frame behind this (`ui::collaboration::CollabRequest::
    /// SyncCRDT`) is not reliably a join reply: it also feeds same-host
    /// fan-out and the undo snapshot, and whether a commit produces one
    /// depends on the toolkit build rather than on anything visible in
    /// the frame. Republishing whatever arrives therefore risks a whole
    /// document on the room topic per commit — unbounded, growing with
    /// the session, and redundant with the delta that already carries
    /// the same edit to anyone who shares the history.
    ///
    /// So the publish is gated on somebody needing one. A peer's
    /// `PresencePayload::Arrived` passes through the local broker's own
    /// drain, which notes it
    /// ([`crate::shell::network::ShellNetwork::note_snapshot_request`]);
    /// the next whole document for that room goes out, and the ones
    /// after stay local until somebody else arrives. The rate is
    /// therefore once per peer that arrives, which is what makes a
    /// broadcast medium acceptable for it at all — the same rare,
    /// single-shot shape [`NeighbourMsg::ScrollbackRequest`] /
    /// [`NeighbourMsg::ScrollbackResponse`] already take.
    ///
    /// It is not addressed to the joiner, deliberately: merging is
    /// idempotent and commutative, so a peer that missed one converges
    /// from somebody else's copy, and naming a recipient would need the
    /// *app* to say who its reply is for.
    ///
    /// # Bounded by [`crate::shell::network::MAX_APP_SNAPSHOT_BYTES`]
    ///
    /// A document has no upper bound and a gossipsub RPC does (64 KiB,
    /// the libp2p default this shell does not raise). Publishing is
    /// therefore refused above a cap set below that ceiling rather than
    /// handing the transport a message it would reject — see the
    /// constant for the arithmetic. A document past the cap does not
    /// converge, loudly, in a log line that names the size; it does not
    /// half-converge and it does not fail somewhere unattributable.
    AppCrdtSnapshot {
        /// Hashed libp2p PeerId of the publisher. Receiving shells drop
        /// their own gossipsub echoes on it, as on every App* variant.
        peer: PeerId,
        /// App-id of the originating app — first component of the
        /// routing key.
        #[serde(default)]
        app_id: String,
        /// Cluster id (the room, as a uuid string). The receiving shell
        /// refuses a room it is not in before the frame reaches any app.
        #[serde(default)]
        cluster_id: String,
        /// Record id — **which document** this is the state of. A
        /// snapshot is the one payload where getting this wrong is
        /// unrecoverable rather than merely noisy: a whole document
        /// merged into the wrong record cannot be un-merged, so an
        /// unaddressed snapshot is dropped rather than guessed at.
        #[serde(default)]
        record_id: String,
        /// `automerge::Automerge::save()` output for that record,
        /// opaque to every hop between the two apps.
        data: Vec<u8>,
    },
    /// Phase 6: cooperative scrollback request. Broadcast over
    /// gossipsub when a peer's local broker has no in-memory
    /// scrollback for a given artifact range and the requesting app
    /// asked for it. Any peer that *does* hold the range replies
    /// with [`NeighbourMsg::ScrollbackResponse`].
    ///
    /// `requester` is the local peer hash of the broker that issued
    /// the request — receiving brokers use it to target the response
    /// (`ScrollbackResponse::responder` becomes the source). v1 does
    /// not deduplicate concurrent identical requests; the typical
    /// case is "rare, single-shot" so amplification isn't a concern.
    ScrollbackRequest {
        /// The artifact whose log to fetch.
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`. Same semantics as
        /// `CollabRequest::FetchScrollback::before_seq`.
        before_seq: u64,
        /// Page size cap. Matches `CollabRequest::FetchScrollback::max`
        /// so the cooperative path doesn't have to re-cap.
        max: u16,
        /// Hashed libp2p peer id of the requesting broker. Receivers
        /// use it to skip themselves when they echo their own
        /// requests back over gossipsub.
        requester: PeerId,
    },
    /// Phase 6: response to a [`NeighbourMsg::ScrollbackRequest`].
    /// The responding broker fetched `ops` from its own local op
    /// log and forwarded them; the requesting broker delivers them
    /// to the original requesting local app via
    /// `CollabEvent::ScrollbackBatch`.
    ///
    /// `responder` is the local peer hash of the answering broker
    /// (useful for "who served me the page" audit / replay
    /// diagnostics). It does NOT play a role in the validation path
    /// — every envelope inside `ops` is independently signed.
    ScrollbackResponse {
        /// Same as the request's `artifact`. Repeated on the
        /// response so the receiving broker can route without
        /// matching on a request-id.
        artifact: collab_types::ArtifactId,
        /// The page of validated ops the responder is sharing. Empty
        /// is allowed (means "I don't have the range either"); the
        /// requester treats an empty response as a non-answer.
        ops: Vec<Box<collab_types::SignedOp>>,
        /// Hashed libp2p peer id of the answering broker.
        responder: PeerId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(msg: NeighbourMsg) {
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: NeighbourMsg = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn sync_req_round_trips() {
        round_trip(NeighbourMsg::SyncReq {
            since: vec![1, 2, 3],
            room: "6b3f9a10-0000-4000-8000-00000000000c".to_string(),
        });
    }

    #[test]
    fn sync_resp_round_trips() {
        round_trip(NeighbourMsg::SyncResp {
            changes: vec![vec![0xAA, 0xBB], vec![0xCC]],
            room: "6b3f9a10-0000-4000-8000-00000000000c".to_string(),
        });
    }

    #[test]
    fn presence_moved_round_trips() {
        round_trip(NeighbourMsg::PresenceMoved {
            peer: 42,
            user: "bob".to_string(),
            key: "cursor".to_string(),
            point: (12.5, -3.25),
            ts: 1_700_000_000,
        });
    }

    #[test]
    fn presence_cleared_round_trips() {
        round_trip(NeighbourMsg::PresenceCleared {
            peer: 42,
            key: "cursor".to_string(),
        });
    }

    #[test]
    fn surface_frame_round_trips() {
        round_trip(NeighbourMsg::SurfaceFrame {
            surface: 7,
            encoded_h264: vec![0x00, 0x00, 0x00, 0x01, 0x67],
            ts: 1_700_000_001,
        });
    }

    #[test]
    fn surface_stream_start_round_trips() {
        round_trip(NeighbourMsg::SurfaceStreamStart {
            surface: 7,
            source_peer: 0xABCD,
            width: 1280,
            height: 720,
            codec: "h264".to_string(),
            ts: 1_700_000_002,
            sender_monotonic_ns_at_send: 1_700_000_002,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
        });
    }

    #[test]
    fn surface_stream_start_backfill_when_field_missing() {
        // Older peer emits the JSON without the extended fields; the
        // `#[serde(default)]` markers must backfill so receivers on the
        // new schema still parse legacy messages.
        let legacy = r#"{"SurfaceStreamStart":{"surface":9,"source_peer":123,"width":320,"height":240,"codec":"h264","ts":1234567}}"#;
        let parsed: NeighbourMsg = serde_json::from_str(legacy).expect("legacy parses");
        match parsed {
            NeighbourMsg::SurfaceStreamStart {
                sender_monotonic_ns_at_send,
                colorimetry,
                hdr_static_metadata,
                bit_depth,
                ..
            } => {
                assert_eq!(sender_monotonic_ns_at_send, 0);
                assert!(colorimetry.is_none(), "legacy has no colorimetry");
                assert!(hdr_static_metadata.is_none(), "legacy has no HDR metadata");
                assert_eq!(bit_depth, 0, "legacy defaults bit_depth to 0 (unspecified)");
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
    }

    #[test]
    fn surface_stream_start_hdr_round_trips() {
        use crate::collab::colorimetry::Colorimetry;
        use crate::collab::hdr_metadata::Hdr10Metadata;
        round_trip(NeighbourMsg::SurfaceStreamStart {
            surface: 7,
            source_peer: 0xABCD,
            width: 1920,
            height: 1080,
            codec: "hevc-main10".to_string(),
            ts: 1_700_000_002,
            sender_monotonic_ns_at_send: 1_700_000_002,
            colorimetry: Some(Colorimetry::hdr10()),
            hdr_static_metadata: Some(Hdr10Metadata::reference_bt2020()),
            bit_depth: 10,
        });
    }

    #[test]
    fn surface_stream_end_round_trips() {
        round_trip(NeighbourMsg::SurfaceStreamEnd {
            surface: 7,
            source_peer: 0xABCD,
            ts: 1_700_000_003,
        });
    }

    #[test]
    fn surface_stream_start_round_trips_postcard() {
        // Same rationale as `app_crdt_incremental_signed_round_trips_postcard`:
        // guard against a wire-format switch dropping the new variant.
        let msg = NeighbourMsg::SurfaceStreamStart {
            surface: 42,
            source_peer: 0x1234,
            width: 1920,
            height: 1080,
            codec: "h264".to_string(),
            ts: 1_700_000_004,
            sender_monotonic_ns_at_send: 1_700_000_004,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn remote_input_round_trips() {
        round_trip(NeighbourMsg::RemoteInput {
            peer: 3,
            surface: 7,
            event: vec![0xDE, 0xAD, 0xBE, 0xEF],
        });
    }

    #[test]
    fn permission_request_round_trips() {
        round_trip(NeighbourMsg::PermissionRequest {
            peer: 5,
            capability: "share-screen".to_string(),
        });
    }

    #[test]
    fn permission_grant_round_trips() {
        round_trip(NeighbourMsg::PermissionGrant {
            peer: 5,
            capability: "share-screen".to_string(),
        });
    }

    #[test]
    fn heartbeat_round_trips() {
        round_trip(NeighbourMsg::Heartbeat {
            peer: 0xDEADBEEF,
            ts: 1_700_000_002,
        });
    }

    #[test]
    fn app_crdt_incremental_round_trips() {
        round_trip(NeighbourMsg::AppCrdtIncremental {
            peer: 0xABCDEF,
            app_id: "terminal".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:/work/report.odt".into(),
            data: vec![0x01, 0x02, 0x03, 0x04],
        });
    }

    #[test]
    fn app_crdt_snapshot_round_trips() {
        round_trip(NeighbourMsg::AppCrdtSnapshot {
            peer: 0xABCDEF,
            app_id: "dev.qos.word-processor".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:/work/report.odt".into(),
            data: vec![0x85, 0x6F, 0x4A, 0x83],
        });
    }

    /// Build a syntactically-correct (but cryptographically empty)
    /// envelope for postcard round-trip exercising. The bytes don't
    /// need to verify — only the framing has to survive a
    /// serialise/deserialise cycle.
    fn dummy_envelope() -> Box<collab_types::SignedOp> {
        use collab_types::{ArtifactId, AttestationId, Ed25519PublicKey, OpClass, SignedOp};
        use std::time::SystemTime;
        Box::new(SignedOp::new_unsigned(
            vec![0xDE, 0xAD, 0xBE, 0xEF],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes([7u8; 32]),
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            42,
        ))
    }

    #[test]
    fn app_crdt_incremental_signed_round_trips_serde_json() {
        // The actor still encodes `NeighbourMsg` with `serde_json` for
        // historical reasons; the new variant must survive the same
        // pipeline. (postcard round-trip is exercised separately so
        // that future wire-format experiments can swap the codec
        // without touching this test.)
        round_trip(NeighbourMsg::AppCrdtIncrementalSigned {
            peer: 0xABCDEF,
            app_id: "notes".to_string(),
            cluster_id: "11111111-2222-3333-4444-555555555555".to_string(),
            record_id: "uuid:1c2f".to_string(),
            envelope: dummy_envelope(),
        });
    }

    #[test]
    fn app_crdt_incremental_signed_round_trips_postcard() {
        // Postcard is the canonical-bytes format used by
        // `collab_types` for signing; verifying that
        // `NeighbourMsg::AppCrdtIncrementalSigned` round-trips through
        // postcard guards against a future wire-format experiment
        // accidentally dropping the new variant.
        let msg = NeighbourMsg::AppCrdtIncrementalSigned {
            peer: 0x1234,
            app_id: "notes".to_string(),
            cluster_id: "11111111-2222-3333-4444-555555555555".to_string(),
            record_id: "uuid:1c2f".to_string(),
            envelope: dummy_envelope(),
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn scrollback_request_round_trips_serde_json() {
        // Phase 6 — the cooperative-fetch request must survive the
        // actor's serde_json pipeline the same way every other
        // gossipsub message does.
        use collab_types::ArtifactId;
        round_trip(NeighbourMsg::ScrollbackRequest {
            artifact: ArtifactId::new(),
            before_seq: 42,
            max: 50,
            requester: 0xABCDEF,
        });
    }

    #[test]
    fn scrollback_request_round_trips_postcard() {
        use collab_types::ArtifactId;
        let msg = NeighbourMsg::ScrollbackRequest {
            artifact: ArtifactId::new(),
            before_seq: 42,
            max: 50,
            requester: 0xABCDEF,
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn scrollback_response_round_trips_serde_json() {
        use collab_types::ArtifactId;
        round_trip(NeighbourMsg::ScrollbackResponse {
            artifact: ArtifactId::new(),
            ops: vec![dummy_envelope(), dummy_envelope()],
            responder: 0x123_456,
        });
    }

    #[test]
    fn scrollback_response_round_trips_postcard() {
        use collab_types::ArtifactId;
        let msg = NeighbourMsg::ScrollbackResponse {
            artifact: ArtifactId::new(),
            ops: vec![dummy_envelope()],
            responder: 0x123_456,
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }
}
