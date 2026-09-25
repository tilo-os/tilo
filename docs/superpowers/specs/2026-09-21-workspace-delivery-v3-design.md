# Workspace delivery v3

> Mirrored from the Tilo Cloud repository (`docs/superpowers/specs`). Relative links to sibling Cloud documents resolve in that repository.

Date: 2026-09-21. Status: proposed contract. Parent: [Reliable workspace](2026-09-21-reliable-workspace-design.md).

## Responsibility

Deliver canonical state and private broker messages through WebTransport reliable streams or HTTPS without changing their owners or admitting duplicate effects. The contract includes authenticated sessions, bounded framing, commit receipts, cursor recovery and backpressure. It does not implement application state, execute agents, or carry live media.

Extract an application-session interface above Iroh in the browser/native relay client. Reuse existing canonical envelope validation, state signatures and broker dispatch. Do not encapsulate QUIC packets in HTTP. Do not modify frozen v1/v2 enum discriminants: delivery v3 has a separate framing/negotiation boundary and can embed the existing canonical state/private payloads.

## Identity and protocol values

| Value | Meaning and persistence |
| --- | --- |
| `workspace`, `host`, `device`, `root` | Existing certified identifiers; validated in native code, never inferred from URL/account ownership |
| `actorId` | Existing persisted CRDT actor identity; independent of reconnect and browser custody reacquisition |
| `leaseEpoch` | Ephemeral custody fencing token for the current local writer; regenerated when custody is acquired, never used as operation identity |
| `operationId` | Existing signed state operation identity or digest of exact canonical signed bytes; stable across batching and carriers |
| `invocationId` | Existing command invocation UUID; private broker semantics |
| `deliveryId` | UUID for one immutable upload item; repeated ID with changed digest is rejected |
| `sessionId`, `carrierGeneration` | Ephemeral authenticated connection binding; invalid after host boot/session expiry; never business-operation identity |
| `consumerId` | Stable device/workspace replay consumer tied to certified identity; cannot authorize another device |
| `storeGeneration`, `sequence` | Durable replay namespace and monotonically increasing u64 commit position within that namespace |
| `cursor` | Opaque authenticated encoding of consumer, workspace, store generation, authorization view and committed sequence |

JS treats u64 positions as opaque bytes/decimal strings or BigInt, never an IEEE-754 Number. Rust owns canonical encoding, signing, identity verification and protocol decoding. JS owns browser APIs and transfers bounded byte buffers to/from WASM. Unknown protocol major versions fail before admission; unknown mandatory capabilities fail explicitly.

## Signed bootstrap and mutual authentication

Enrollment/join returns a signed transport descriptor containing protocol major 3, workspace/root, certified host identity, exact allowed HTTPS origin, WebTransport path, HTTPS base path, native endpoint equivalent, allowed capabilities, issue/expiry times and descriptor generation. Validate the descriptor under existing workspace authority before connecting. Hostname TLS validation and native device certification are both required. A transport URL cannot contain credentials or fragments; do not follow cross-origin redirects.

Keep runtime/enrollment module URLs same-origin as today. Only the authority-validated transport origin is allowed cross-origin. CORS permits the actual deployed Cloud origin and required headers; no wildcard authenticated origins. Read/status routes still require device authorization.

Extend the existing mutually signed nonce handshake using a new closed `tilo-delivery-v3` domain. Canonical transcript includes protocol, both fresh 32-byte nonces, workspace, device/root, certified host/root, exact origin/audience, descriptor generation, server boot ID, session ID, carrier kind and requested capabilities. Verify certificate validity, current roster/role/revocations and both signatures before opening state/private streams. Do not retry a failed proof under a legacy domain. Use existing signature/hash primitives and golden Rust/JS fixtures; no new cryptographic algorithm.

A WebTransport session performs this handshake on its control stream; no workspace data flows beforehand. The server binds all subsequent streams to that authenticated connection and a negotiated lane. The browser does not need arbitrary headers on WebTransport construction. Reject unauthorized origins at CONNECT and bound unauthenticated handshakes before the proof is complete.

HTTPS starts with challenge/auth requests. The resulting short-lived session is device-bound, not an account bearer. Each authenticated HTTP request includes session ID, fresh request nonce, expiry and device signature over the canonical method, exact path/query, body digest, session binding and nonce. Reuse business IDs on retry but generate a fresh request proof. Reject repeated proof nonces; never log these headers. This provides proof of possession across independent requests without depending on cookies. Disable cross-origin redirects and ambient credentials.

Sessions expire after 15 minutes or earlier certificate/grant expiry. HTTP proof lifetime is at most 60 seconds, accepted with at most 30 seconds clock tolerance. The server retains seen nonces until that proof cannot be accepted and caps storage; overload rejects admission rather than evicting still-valid replay evidence. A host restart invalidates sessions and demands a fresh handshake. Private-key material stays in the existing browser identity owner. Zero-RTT application mutations are disabled.

Begin session renewal after 12 minutes or sufficiently before an earlier credential expiry. Use fresh mutual proof and the same owner/cursors/invocations; atomically fence the prior carrier generation when the replacement is ready. No overlapping mutation authority is granted. If renewal cannot complete before expiry, pause remote admission while retaining local editing/durability. Timely renewal must not remount the desktop or restart a healthy independent call.

Authorization is checked again at admission/commit scheduling and before each emitted replay batch. Removal closes affected streams and invalidates media grants; membership in one room grants no access to another. Offline operations with expired attestation evidence use the existing certified retained-evidence policy; transport code cannot change timestamps or signatures to force admission.

Construct broker source root/device/certificate only from the verified session. Reject client-supplied server-only delivered/receipt variants; never trust a claimed `source_user` or copy transport metadata into a privileged broker assertion without verification. Keep canonical broker authorization after ingress authentication, including target host/room binding.

## Framing and lanes

Every v3 frame has a 12-byte header: four bytes `TLD3`, u16 little-endian major version (`3`), u16 little-endian message type, then u32 little-endian payload length. Payload is a canonical Rust/Postcard structure defined for that type/version. Frame length excludes the fixed header; routing metadata counts against the payload cap. The first authenticated stream frame identifies its negotiated lane and session. Both implementations share fixtures for every message type, truncation boundary and bound. Never deserialize unbounded collections before checking encoded and decoded-size limits.

Initial message registry: 1–4 `ClientHello`, `ServerHello`, `ClientProof`, `Ready`; 5 `LaneOpen`; 10–17 `StateItemChunk`, `StateAdmission`, `EventChunk`, `CommitBoundary`, `FlowCredit`, `CursorAck`, `SnapshotRequired`, `SnapshotManifest`; 20–25 `Invoke`, `Status`, `Cancel`, `PrivateUpdate`, `PrivateHistoryQuery`, `PrivateHistoryPage`; 30–36 `Ping`, `Pong`, `Drain`, `Error`, `Subscribe`, `Unsubscribe`, `Presence`. Other values are reserved and rejected in v3. Session/lane role defines permitted directions. Authentication frames are limited to 8 KiB; their structure is the signed transcript above. HTTPS methods map to these same typed payloads; they do not invent different mutation semantics.

| Lane | Contents | Ordering and recovery |
| --- | --- | --- |
| Control | Authentication, capability, ping/pong, drain, subscription, acknowledgements | One reliable bidirectional stream; application-level committed cursor ACKs |
| State upload | Signed publications and explicit admission results | Up to two in-flight batches; stable item/operation IDs; native causal admission preserved |
| State download | Live changes or replay for one workspace consumer | One ordered stream per active consumer; cursor advances only after contiguous commit |
| Private commands | Broker invoke/status/cancel and private updates | Separate reliable streams, at most eight active requests; invocation/status recovery, not public replay |
| Bulk | Snapshot/content chunks | Bounded independent streams; digest-verified resumable objects; lower scheduling weight |

No durable data uses WebTransport datagrams. Presence is an optional latest-value lane with expiring leases; it must not share durable queues. Start with reliable bounded presence updates, coalesced per participant, rather than adding another datagram protocol.

A state upload is a transport batch of individually atomic items. Each result names `deliveryId`, canonical digest, admitted operation IDs and `accepted`, `duplicate`, `rejected`, or `retry-later`. This deliberately does not promise atomicity across independent records. If a native mutation spans multiple records, carry its existing indivisible commit group as one item and preserve that owner's atomic commit rule. This corrects the earlier proposal's overly broad all-or-nothing batch assumption.

Private payloads remain capped at the existing 256 KiB including routing overhead. State payloads retain the existing 64 MiB canonical envelope ceiling, but travel as chunks at most 256 KiB. Large items stage in bounded private temporary storage with item ID, total size, digest and chunk offsets; receiving chunks is not durable semantic admission. Validate the completed envelope and commit through the normal owner before an acceptance receipt. Stage cleanup never deletes admitted state or the sender's journal. Reserve decode memory before assembling a large native envelope; one 64 MiB envelope must not bypass aggregate limits.

Partial frames/chunks are discardable staging; replay uses original identity. Reject conflicting chunks, overlapping changed bytes, unknown lengths and oversized decompression. Version one uses no application compression. Transport flow-control ACKs never retire sender evidence.

Separate **byte-flow credit** from **committed cursor acknowledgement**. A receiver can release byte credit after moving validated chunks out of its RAM window into reserved staging, even before the complete semantic item is admitted. This cannot retire the publisher's durable journal or advance the consumer cursor. Without this distinction, a 64 MiB item would deadlock behind a 4 MiB receive window. On staging loss the receiver requests those chunks again; only complete local semantic commit advances the durable cursor.

## HTTPS mapping

All paths are below the descriptor's `/peer/v3` base. JSON may be used for bounded handshake/errors, but canonical mutations and event bodies stay binary.

| Request | Semantics |
| --- | --- |
| `POST /challenge`, `POST /sessions` | Bounded mutual authentication; no workspace read before proof |
| `POST /state` | Up to 1 MiB batch of small items/chunks; per-item admission results |
| `GET /events?cursor=...&mode=stream` | Binary framed subscription; cursor is a position, not a credential |
| `GET /events?cursor=...&mode=poll` | Hold up to 20 seconds; return up to 1 MiB/64 frames or an empty response; immediately reissue |
| `POST /acks` | Commit-qualified contiguous cursor acknowledgement |
| `POST /commands/invoke`, `/commands/status`, `/commands/cancel` | Same broker semantics as WebTransport; status takes the original invocation |
| `POST /snapshots`, `GET /snapshots/{id}/chunks/{index}` | Authorized pinned snapshot manifest and bounded chunks |
| `POST /objects`, `PUT /objects/{id}/chunks/{index}`, `POST /objects/{id}/complete`, `GET /objects/{id}/chunks/{index}` | Optional negotiated content capability; digest-verified staging and authoritative final admission |
| `DELETE /sessions/{id}` | Best-effort explicit detach; no deletion of records, invocation ledger or journal |

A streaming response sends available frames immediately; heartbeats do not advance its cursor. Drain after five minutes to refresh authentication and resume. Use `Cache-Control: no-store`; disable intermediary buffering; support concurrent upload/status during download. HTTP/2 is preferred; qualify HTTP/1.1 long polling with a bounded connection count too. HTTP success means only the explicit receipt in its body.

Errors are bounded codes: `unauthenticated`, `permission-denied`, `unsupported-version`, `conflicting-id`, `cursor-reset-required`, `quota-exceeded`, `temporarily-unavailable`, and `invalid-frame`. HTTP maps these to 401/403/426/409/409/429 or 507/503/400 as appropriate; error bodies distinguish reset from ID conflict. Honor `Retry-After`; never treat permission failure as permission to change transport. A response lost after admission is recovered through operation identity/receipts, not an assumption that HTTP failed before execution.

## Commit and receipt protocol

1. A local edit mutates the existing in-memory native model for immediate rendering. The storage owner atomically commits record bytes, admission basis and outgoing journal. Only successful transaction completion advances local durable heads.
2. The delivery scheduler publishes committed items. The ingress verifies bounds/auth/identity and passes them to the existing state owner; it cannot acknowledge acceptance from an in-memory queue.
3. The authoritative store makes state, deduplication evidence and replay discoverability durable under its commit barrier. Prefer commit metadata referencing existing immutable state bytes, not a second payload database. If the existing layout cannot atomically add a replay index, commit a replay-intent reference with the canonical mutation and recover it before serving a later cursor. Fault tests must prove both sides of this barrier.
4. The relay may return a durable relay-admission receipt. The native workspace owner separately issues the existing signed coverage receipt tied to exact document heads. “Synced” still requires that native receipt; relay acceptance is only an intermediate state.
5. Downloaded state is verified/admitted and committed with the local cursor in one IndexedDB transaction. The runtime's batch acknowledgement includes the cursor extension so neither half can commit alone. Failed storage leaves the prior cursor and preserves replay eligibility.
6. The client acknowledges only its committed prefix. The server durably records consumer progress before using it to release that consumer's replay references. Canonical data retention is independent of a consumer ACK.
7. Sender journal retirement uses the existing native coverage condition. Receipt loss produces retransmission/status recovery with stable IDs; duplicates may return a new equivalent coverage receipt for the same durable basis, not a second edit.

Deduplication uses canonical operation identities, not only batch UUIDs. Rebatching, switching carriers or snapshot catch-up must not re-admit the same signed operation as a new effect. Transport batches may complete out of order; document causality and prerequisite admission remain native rules. Unknown prerequisites are retained within existing admission budgets, not skipped with an advanced cursor.

## Replay and snapshots

Replay position is an ordered durable commit reference, not the current filesystem enumeration order. Add a recoverable index under the relay store's existing single-writer authority. Store generation persists across ordinary restart; create a new generation for a restore/rewind so an old cursor cannot skip restored history. An index rebuild must either reproduce exact positions or issue explicit reset-required, never silently reuse a number for different content.

The existing relay sequence reservation and temporary replay capture do not already implement this contract. Reserved positions can have gaps; an allocated number is not evidence of a committed item. The index must distinguish reservations from committed entries and expose only a verified committed boundary. Its recovery rules must resolve incomplete reservations before issuing a later cursor, without waiting forever for a number that was never committed. Existing write-offset cleanup cannot substitute for the consumer's durable cursor acknowledgement.

Consumer streams contain only authorized frames and commit boundaries. Their scoped cursor can advance over undisclosed internal positions without exposing another room's data. A cumulative cursor is issued only at a complete visible commit boundary. The browser commits that token with all preceding visible items; out-of-order fragments cannot advance it.

Retain replay accelerators for up to seven days and at most 256 MiB per workspace, whichever bound is reached first. These are reconstructible references to canonical committed data, not the only copy of an edit. Compaction may replace references only when a certified snapshot plus suffix can reconstruct the required admitted state/continuity evidence. If that proof is unavailable, retain the necessary data and apply backpressure instead of deleting it. Unresolved commands are never governed by this replay TTL.

On a stale cursor, create a snapshot manifest with workspace, authorization view, store generation, watermark, document heads, chunk digests and native authority proof. Pin its chunks and suffix for five minutes, renewable while making progress, within the snapshot budget. A concurrent mutation after the watermark belongs to the suffix. Revocation invalidates the pin's access immediately. Expired/capacity-rejected pins cause a new snapshot attempt with explicit progress reset, not loss of local work.

The browser verifies chunks, hydrates the certified native state and merges its own pending journal through the normal CRDT admission rules. Stage candidate state separately; atomically install admitted records/journal/cursor only when consistent. A client lacking disk space remains on its prior valid state. It does not clear IndexedDB or replace unsent work with the snapshot. Pending operations now forbidden by current policy remain visible as unsent/rejected and require user handling.

Supported content/file transfer is separate from replay: reuse the native content digest/store as owner, 256 KiB verified chunks, resumable offsets and explicit final durable receipt. Validate current read/write authority on every access. Staged incomplete uploads expire after 24 hours; committed content follows workspace retention and is not deleted by transport GC. Missing blob capability is explicit. Inventory every enabled content path during qualification; no hidden `iroh-blobs` WebSocket fallback is allowed on a path advertised as migrated.

## Connection state machine

States: `hydrating → authenticating → catching-up → online`, with independent transitions to `offline`, `backoff`, `authorization-required`, `storage-blocked`, and `closed`. Only `closed` releases the owner after successful cleanup. An authenticated connection may still be catching up; readiness requires canonical record admission.

When WebTransport is supported, try it first with a 750 ms connection budget. On reachability or WebTransport negotiation failure, abort the attempt and try HTTPS streaming; use long polling if streaming is unavailable or buffering prevents progress. Do not downgrade certificate, origin, device-proof, delivery-v3 version or authorization failures. On a known restrictive network, cache HTTPS preference for ten minutes. Do not probe QUIC repeatedly while a healthy fallback is serving work; reconsider on a later reconnect/network change. Media may establish its independent session without moving the data carrier.

Switching is break-before-make for mutations: pause new sends, fence the old carrier generation at ingress, close its streams, authenticate the replacement, recover cursor/receipts and command status, then resume. An operation admitted before fencing may finish; its stable ID/outcome remains authoritative. Old callbacks cannot mutate the new session's projections. If the old path cannot be fenced remotely, the new handshake atomically replaces its session generation; already admitted work is still reconciled rather than duplicated.

Use one jittered recovery coordinator. Base retry is 250 ms, exponential growth capped at 10 seconds, reset only after 30 seconds of stable service; server `Retry-After` takes precedence. A network-online signal triggers one immediate attempt. Heartbeats every 10 seconds and a 30-second contact deadline apply to active pages; freeze/resume resets the liveness probe, not storage or identity. Planned drain resumes immediately. Local storage failures block publication independently of transport retries.

## Initial budgets

These are explicit v3 defaults to qualify, not current production measurements. Peers negotiate downward, never above server policy; changing a bound requires updated contract tests and load evidence.

| Budget | Default |
| --- | --- |
| State chunk; ordinary HTTP batch | 256 KiB; 1 MiB |
| Canonical state envelope; private frame | Existing 64 MiB; existing 256 KiB including routing overhead |
| Active state uploads; private requests | 2; 8 per session |
| Undrained download byte-flow window | 4 MiB per consumer; producer pauses until byte credit is released; semantic cursor ACK remains separate |
| Control reserve | 256 KiB per session, independent of bulk/state window |
| Aggregate serialized/replay buffer budget | 256 MiB per host; includes outstanding permits, not just queue lengths |
| Large-envelope decoding | One admitted large decode at a time per host; charge peak copies against aggregate budget |
| Live sessions | 32 per initial hosted machine; 4 per device across rooms; reject excess before allocation |
| Snapshot/bulk staging | 256 MiB per workspace, 1 GiB per host; reserve before accepting chunks |
| Browser outstanding publication bytes | Warn at 32 MiB; block additional affected-record edits at 64 MiB or actual quota failure, whichever comes first; excludes committed document/cache bytes |
| Retained legacy journal above new cap | Open without deleting data, block growth, and drain/recover before new edits |
| Small-edit coalescing | At most 5 ms after local commit; batch size also bounded |
| Request proof replay cache | At most 16,384 entries per session; reject overload without evicting still-valid nonces |

Use byte permits throughout encode, queued output, decoder and delivery ownership. Count simultaneous raw/encoded copies, as the current replay code already does. Control traffic gets service before bulk, but not unlimited priority: at least one state batch is serviced for every eight ready control batches so state cannot starve. Reserve bulk scheduling capacity only when control/live state is not backlogged. Upload staging uses disk quotas, private paths and atomic cleanup manifests; it is not a new durable document owner.

Global disk low-watermark is the larger of 1 GiB or 5% free. Stop new durable admission when the reserved space for state/receipt/metadata cannot be guaranteed; return storage-pressure without pretending acceptance. Already accepted work remains readable/recoverable. Operators can raise quotas or restore capacity; transport never evicts unresolved unique work to appear healthy.

## Integration points and proof

Browser: split connection mechanics from `src/peer.rs`; retain `peer_journal.rs`, storage batches and `web/peer-session.mjs` owner. Add focused adapter/framing modules rather than putting both protocols in the React shell. Extend `web/peer-recovery.mjs` once; retire its old Iroh retry wiring for v3 sessions.

Relay: add v3 wire/auth and WebTransport/HTTPS ingress modules near existing `auth.rs`, `client.rs`, `transport_v2.rs`, `peer.rs`; reuse state admission, `store/`, `persistence.rs` and their serialization. Native hosts supply the same owner/broker callbacks. Keep protocol code independent of concrete host executables to avoid Cargo cycles.

Required proofs: every commit/receipt/cursor crash boundary; same ID/different body rejection; partial chunk resume; cursor reset and concurrent snapshot suffix; compaction with offline edits; cross-room replay denial; carrier switch during an in-flight command; slow reader with bounded RAM; quota failure without false acknowledgement; old/new native participants; and the entire workflow with WebSockets blocked. Real browser/native tests must validate the actual storage transaction and rendered state, not just in-memory fixtures.

## Approved HTTPS product-carrier contract amendment (2026-09-22)

The shared delivery capability assignments are `STATE_REPLAY_V1 = 1`, `PRIVATE_COMMANDS_V1 = 2`, `PRIVATE_READS_V1 = 4`, `SNAPSHOT_V1 = 8`, and `RESUMABLE_STATE_V1 = 16`; the complete enabled product mode requires mask31. Media capabilities remain in their separate mask. These bits describe actual installed and qualified behavior, not a configuration flag, parser, test result or fabricated owner witness. Existing literal masks must be migrated through the shared known-capability definitions with explicit owned changes. No partial implementation advertises full product readiness.

Preserve the existing exact state/events/ACK/snapshot and `/peer/v3/commands/invoke`, `/commands/status`, `/commands/cancel` paths. Add exact `POST /peer/v3/commands/catalogue`, `POST /peer/v3/reads`, and `GET /peer/v3/private/events?mode=poll`. Private events are bounded broker/read responses and never enter public state replay or cursor acknowledgement. The previously unused v3 message IDs26–29 are now CatalogueRequest, CatalogueResponse, AgentRead and AgentReadResult respectively. Existing IDs, legacy join/envelope discriminants and their encoded bytes remain frozen. Request/reply directions and allowed existing ActionMessage variants must be closed and validated.

Fresh invited HTTPS onboarding uses exact `POST /peer/v3/join/inspect`, `/join/challenge`, `/join/complete`, `/join/membership-challenge`, and `/join/membership-complete`. A closed signed device-possession wrapper binds the actual request and pinned origin/host/scope before the existing native join service receives an authenticated remote identity. A wire certificate key is not an authenticated endpoint and cannot supply that evidence. Existing invitation issuance, cancellation, membership checks, join proof domains and native barrier remain authoritative. Both fresh owner enrollment and invited onboarding supply actual retained signed governance bytes and admit them normally in StatePeer before descriptor/session authorization; no positive room reconstruction or relaxed current authority is allowed.

The complete carrier includes64MiB canonical state fragmentation/resume, real initial snapshot+suffix and reset, durable consumer registration before Ready, same broker command/status/read semantics, original-request held-poll continuation, atomic browser state+cursor persistence before ACK, and real resource scopes for every native owner mutation. Data renewal/replacement fences old mutation generations while independently healthy media remains retained; expired shared authority still fences all dependent authorization.

Implementation follows `docs/superpowers/plans/2026-09-22-https-data-carrier-installation.md`. Root assigned shared/private broker to command_audit, onboarding to managed_agent_custody, staging/snapshot to live_runtime_inventory, actual resource/all-callers integration to session_delivery_writer, and parent registry/finish/scheduler to native_transport_seam. Production data issuance remains closed until the actual bounded native owner qualifier and live installed handlers exist. Root owns graph/index. The user requested deployment after local validation: complete and review the real local product proof before any AWS testing or deployment. Deployment remains part of the goal; this sequencing constraint does not introduce a separate permission requirement.
