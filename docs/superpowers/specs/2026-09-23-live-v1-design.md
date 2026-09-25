# LIVE_V1: real-time multiplayer layer

> Mirrored from the Tilo Cloud repository (`docs/superpowers/specs`). Relative links to sibling Cloud documents resolve in that repository.

Goal: remote activity (cursors, selections, drags, typing) appears on other screens within one network hop, like Figma. The durable delivery carrier stays the only source of truth; LIVE_V1 is an additive, ephemeral fast path.

Wire contract: `tilo-relay/src/delivery_v3/live_wire.rs` (tests in `live_wire/tests.rs`). Descriptor media bit `MEDIA_LIVE_V1 = 2`, accepted beside `MEDIA_VOICE_V1` via `descriptor::MEDIA_CAPABILITIES`. A host advertises the bit only when its live relay is installed.

## Two paths

| Path | Carries | Transport | Guarantees |
|---|---|---|---|
| Live (new) | presence, pointer, selection, drag, text previews, early forwarded signed changes | MoQ Draft 21 over WebTransport at `/moq/v21/live` on the host's existing QUIC endpoint, subprotocol `moqt-21` | ephemeral, not stored, no authority, latest-value for presence |
| Durable (existing) | signed CRDT changes, governance, replay, cursors, snapshots | HTTPS delivery v3 (state upload, events, acks) | durable, ordered, recoverable |

## Authorization

1. Client holds an authenticated data session (capabilities 31).
2. `POST /peer/v3/live/grant` with a request proof, body `LiveGrantRequest` (device = session device, room, fresh `participation_id`, directions). The host checks current roster/role (viewers: subscribe only) and returns a `SignedLiveGrant` (host-signed, domain `tilo-live-grant-v1\0`, lifetime ≤ 60 s, audience = origin + `/moq/v21/live`, namespace = (room digest, device)).
3. The client presents the grant bytes as the MoQ Draft 21 Authorization Token on connect to `/moq/v21/live` (same mechanism as voice). The relay verifies signature, lifetime, audience, host boot, and current membership before any publish/subscribe, and re-checks on roster changes (revocation closes only that publisher's tracks).
4. Clients renew before expiry through the same route without reconnecting.

## Tracks

Namespace tuple: `("tilo-live", hex(room_digest), hex(publisher_device))`. A publisher may only publish into its own namespace; subscribers subscribe per member.

- `presence`: `PresenceFrame` as MoQ object datagrams, ≤ 1100 bytes, latest-value. Receivers drop frames with lower `seq`. The relay forwards immediately with no queueing (drop if the subscriber is congested); rate ≤ 120/s per publisher.
- `ops/<hex record digest>`: `OpsFrame` on ordered streams, ≤ 64 KiB per frame, ≤ 240/s per publisher, ≤ 16 open records per publisher.
  - `Preview`: ephemeral text ops shown until the durable change arrives.
  - `SignedChange`: the exact canonical delivery item bytes, forwarded as soon as the host has checked signature and permission, before durable commit. Receivers admit it exactly like replay (StatePeer checked path), de-duplicate by digest, and never advance the durable cursor from it. If the host later refuses the item, receivers are corrected by the durable path.

## Client behaviour

- Publish presence at frame rate while it changes, coalesced to the latest value; stop when idle.
- Render remote pointers/drags with interpolation (one frame of buffering) so 30–120 Hz updates look smooth; extrapolate briefly on loss; snap to the durable state on `End`/`Cancel` or when the durable change lands.
- The live path is optional: if unavailable (no bit, UDP blocked), everything still works over the durable path.

## Targets

Remote pointer/drag: one-way delay + ≤ 5 ms relay. Remote text: one-way delay + ≤ 10 ms, never falling behind while typing. Durable confirmation continues in the background.

## Lanes

- A: relay + host (live namespace in `qos-moq` relay, `/moq/v21/live` routing in `media_ingress`, grant route + roster checks, forward of admitted signed changes to `ops` tracks, advertise `MEDIA_LIVE_V1`).
- B: browser live client (grant, connect, publish/subscribe, interpolation buffer, JS API for apps).
- F: tab-to-tab latency harness.
- C (after faults lane): host forward-before-commit on the durable admission path.
- D (after faults lane): push instead of held polls; HTTP/3 for data.
- E (after frame lane): drawing remote cursors, selections and drags in ui-core/apps.

## Lane C: forward before commit (implemented)

Host (native `qos-session-peer`, Cloud `qos-cloud-host`, same design):

- The data routes run each state admission synchronously on the owner thread inside `early_forward::scoped(sink)`. Admission arms an item only after `StatePeer::receive_from_report` accepted it as new (signature, author certificate, role/capability, canonical form) and its durable publication was built. The owner fires the armed item after every remaining non-durability check (aggregate/replay validation, byte retention, encoding, policy fence; Cloud: `prepare_commit` and encoding) and immediately before the store write and barrier. The sink only enqueues; the forwarder task publishes `OpsFrame::SignedChange` while the owner syncs. Refused, deferred (`RetryLater`), metadata-only and duplicate items are never forwarded. Inline and staged (chunked) uploads use the same path; items whose frame exceeds 64 KiB (all browser-staged uploads, which start above 240 KiB) reach members only by durable replay.
- Record-less documents (empty record id, e.g. Chat) map to `LiveRecord { app, record: "@qos:package-primary" }`, the UI's `RecordId::package_primary()`. That id is never a valid durable record id, so the mapping is injective; a durable id spelling the sentinel, or ids over 128 bytes, are not forwarded.

Correction when the barrier fails: the forwarded item is not durable. The author keeps it in its durable outbox until it has an exact receipt, so a retry after reconnect commits the same bytes. If the author is revoked before the retry, or never retries, the item is never committed. Durable replay never carries the author's original bytes: it carries host-certified document publications with per-author coverage.

Receiver (browser `peer_live/speculation.rs`): a forwarded change is speculative until durable state covers it.

- Admission uses the checked native-cut path onto the current view. The Peer shows the result (state, record projection, mounts) but never stages the admission journal, cursor or outbox for it. The first outstanding change captures the durable basis.
- Durable cuts are admitted on the basis, never on the view. After each durable install the outstanding changes are rebased. A change is promoted when the new durable state `covers` it (host-certified coverage of the author's sequence). A change that is still admissible and younger than 10 s is re-applied on top. Any other change is withdrawn, and the stored record bytes of its record are replaced from the resulting state, since a CRDT merge cannot remove changes. `peer_live_reconcile` (timer in `live-client.mjs`) withdraws expired changes without waiting for a durable cut.
- Tradeoffs:
  - A local edit made while changes are outstanding promotes them, because the edit causally depends on what the user saw. Durable paths other than delivery cuts (committed-desktop, invitation) promote them as well, since those paths build on the view. These are the only ways a forwarded change is persisted before confirmation.
  - Withdrawal cannot rewrite a record whose owner is currently mounted in a desktop: its in-memory document keeps the change until the record is reopened.
  - Withdrawing early only costs a flicker, because a later retry still arrives by durable replay.

## Lane D2: O(1) catch-up (implemented)

- Group commit (no wire change; native `qos-session-peer`, Cloud `qos-cloud-host`): a multi-item `POST /peer/v3/state` admits consecutive newly accepted document items (`AppDocument`) onto one candidate and commits them as ONE commit and replay group; every per-item receipt references that group. Each item still passes the exact `StatePeer` admission on top of the items before it. Any other item (governance, duplicate, retry, metadata, refusal, receipt conflict, capacity) ends the group: the group commits first, then that item takes the unchanged single-item path, so a refused item never enters a commit and ordering and per-item semantics are unchanged. Lane C forwarding is kept: every verified item of the group is forwarded, in order, before the group's barrier.
- `MULTI_GROUP_REPLAY_V1 = 32` (`tilo-relay` `delivery_v3::capabilities`): an optional, transcript-only data bit. It is never in a signed descriptor (descriptors stay at `PRODUCT_DATA_V1 = 31`, so older clients keep accepting them) and never mandatory. Hosts allow it beside their descriptor bits (`handshake_policy`); clients request it optionally and allow it independently of the descriptor (`client_allowed`); negotiation intersects. With the bit a poll page may carry up to 128 complete groups (`EventChunk* CommitBoundary`, repeated) in strictly increasing commit order, at most 512 frames and 1 MiB; each group keeps its own 64-frame bound and checks. Without it a page carries exactly one group, as before.
- Browser: `collect_page` checks every group of a page; `admit_cuts` admits them in order onto one candidate (each group through its own native cut), and the page installs as one storage batch whose cursor is the last admitted group's; only that cursor is ACKed. A refusal after the first group installs the admitted prefix; the next poll resumes from it.
- The shared QUIC endpoint bounds connections per kind (`media_ingress/limits.rs`): 32 handshaking, 64 HTTP/3 data, 32 voice, 64 live, instead of one shared 16.
