# Canonical Record Collaboration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The controller serializes source authors, reviews each concrete commit and continues authorized tasks after review and exact-SHA host verification.

**Goal:** Connect the shared real Notes/Files host to authenticated record replication, with one worker and writer per canonical record, independent instance presence/follow, and recoverable offline operation.

**Architecture:** Extend the existing Cap'n Proto collaboration service, signed `DocumentMutation`, peer `AppDocument`/`AppPresenceV2`, `CollaborationManager`, and canonical `RecordOwners`/`RecordStore`. Transport owns bytes and authentication; the UI-thread canonical owner alone owns the document/history/persistence. Components own only ephemeral subscriptions and their existing local Views. Native window bootstrap supplies the same explicit enrolled inputs as an isolated library harness.

**Tech Stack:** Existing Rust workspace/toolchain/lockfile; Automerge, Cap'n Proto, Iroh, `qos-actions`, `qos-user-identity`, `channel-crypto`, existing sealed persistence and MLS governance.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` (§§ application identity/lifecycle, actions, collaboration/presence, offline/restart and cross-host qualification). Required predecessor contracts: `docs/superpowers/plans/2026-09-13-common-real-app-framework.md`; native consumer: `docs/superpowers/plans/2026-09-13-native-macos-window.md`, Tasks 5–6.

## Global constraints

- “Two tiles opening one record share its document, but keep independent Views and operation contexts.”
- “Following never merges another participant's view into the local view or turns a remote focus update into document mutation.”
- “Do not create a second Mac command system or a second collaboration document”.
- Same codebase, lockfile and toolchain on macOS and Fedora; preserve existing Linux RPC/peer wire ordinals, legacy constructors/callers and portable test targets.
- Exact `(profile root, app/package ID, workspace ID, record ID, kind)` is immutable. Original minting `home` is immutable but need not equal replica workspace. Display handles and active workspace are not authority.
- One canonical writer/history/document and one SDK worker per record. A second component gets a subscription, never another `CollaborationManager` or autosaver.
- Native main, Keychain, first-user/add-device enrollment ceremony, app schema/import migration and package/standalone process bridge are separate. This plan must compile/test before native Task 6; its generic inputs cannot import `NativeHostConfig` or `prepare_native_host`.
- The controller assigns one source/build author at a time. Controller accepts each source commit only after native evidence and Fedora checks at that exact SHA. This plan records requirements; it is not implementation or runtime evidence.

## What exists, and what this plan must produce

| Source inspected | Consequence |
| --- | --- |
| `ui/src/record.rs`: `RecordStore(Rc<RefCell<RecordSet>>)`; `with`, `read`, `with_history`, `tick`, `flush` | Canonical fail-closed opening/exclusive writer APIs below are **planned predecessor outputs**, not present merely because this document refers to them. This slice extends their owner slot, not an app-owned copy. |
| `ui/src/app.rs:3651,3780,3816,4164,5082` | Current delta send reads `App.state`; incoming snapshot merges/load_incremental mutate that outer state. Extract the shared decode/apply operation and target bound record owners; preserve legacy outer-state path. Follow currently restores only scroll/view-mode, a useful shared behavior. |
| `ui/src/collaboration.rs:1455,1506,1987,2007,2041,2939` | Explicit certified FD route, additional routes, MLS install and document signing exist. `for_route_fd` without identity is uncertified; never use it for production. `for_record` discovers environment; never call it from components. Sends currently swallow failures and consume save cursors. |
| `shell/src/shell/collab_broker_capnp.rs:215,255,2783,2815,3480,5495` | Join proof and membership checks exist; join reads ambient registry/fleet. Revalidation refreshes role only, and part of signed-send validation logs/drop then returns RPC success. Both need shared fixes. |
| `shell/src/shell/network.rs:591,3210`; `shell/src/network/identity.rs` | Network ingress authenticates full `PeerOrigin` and binds document author to device, but queued entry retains only a role/hash. Preserve full origin and revalidate after dequeue; never promote the u64 display hash into identity. PeerIdentityStore already retains signed certificate and additive revocations. |
| `shell/src/shell/collab_broker_capnp.rs:3590`; `collab_broker.rs:929,1100` | History needs exact artifact/route and current read authority, signature/author checks and record validation. A joined channel must not fetch arbitrary artifact history. |
| `shell/src/shell/collab_presence.rs`; `collab_broker_capnp.rs:5654`; `collab_broker.rs:892`; `collab_outbound.rs:133` | Existing presence deliberately widens to room; peer transport omits record. Record/instance presence requires an explicit compatible encoding and authenticated attribution, not an ambient-room inference. |
| `ui/src/presence/key.rs`; `ui/src/view.rs:88` | Key enum ordinals are wire ABI. `ViewSubset` has no participant instance. Keep existing key/payload enums and encode a versioned envelope in a reserved existing Resource key. |
| `channel-crypto/src/{group,persistence}.rs`; `shell/src/shell/collab_mls_authority.rs` | Sealed group save/load and `MlsVaultProvider::{checkout,persist}` exist. SDK install currently holds an unpersisted mutable group. A durable, exclusive document endpoint adapter coordinated with governance is missing and belongs to Task 2. Keychain provisioning remains a separate provider gate. |
| `shell/src/shell/room_records.rs:39,407,474,674` | `KnownRecord.package_id` can be absent; `home` is not a grant. Use validated observed route/app metadata and the actual room reference for admission; do not guess app from kind or cold-load path as authority. |

## Adopted predecessors and dependency direction

Common framework produces the following exact signatures (imports omitted):

```rust
impl RecordOwners {
    pub fn new(profile_state_root: PathBuf) -> Self;
    pub fn open(&mut self, package_id: &str, workspace_id: &str)
        -> Result<RecordStore, RecordOpenError>;
}
impl RecordStore {
    pub fn try_open_record(&self, want: &RecordCheckout)
        -> Result<RecordOpenOutcome, RecordOpenError>;
    pub fn reserve_record(&self, want: &RecordCheckout)
        -> Result<RecordReservation, RecordOpenError>;
    pub fn is_ready(&self, id: &RecordId) -> bool;
    pub fn save_status(&self, id: &RecordId) -> Result<RecordSaveStatus, RecordOpenError>;
    pub fn try_flush(&self) -> Result<(), String>;
}
impl HostContext {
    pub fn prepare(definition: &ApplicationDefinition, records: RecordStore,
        views: Rc<InstanceViewStore>, device_id: String, instance_id: InstanceId,
        permissions: Rc<RefCell<PermissionContext>>, author: UserId,
        action_context: qos_actions::ActionContext, target: &LaunchTarget)
        -> Result<Self, LaunchError>;
    pub fn record(&self) -> &RecordCheckout;
    pub fn author(&self) -> &UserId;
    pub fn permissions(&self) -> PermissionContext;
    pub fn wake_handle(&self) -> WakeHandle;
}
```

The **live permission handle** is the existing planned private `HostContext.permissions: Rc<RefCell<PermissionContext>>`; there is no produced method named `livePermission`. This plan retains that exact handle and adds a synchronous refresh hook before each canonical execution. The common bound dispatcher already consumes `&mut dyn qos_actions::ActionTransport`; its `invoke_resource_action` keeps existing actor, broker-admitted and confirmation evidence, invocation and context parameters. Preserve `BoundActionAddress { instance_id, address }` and per-instance UndoLog attribution.

Common framework → Tasks 1–4 here → real Notes Tasks 1–3 / Files Tasks 1–6 + Task 5 here. Native Task 6 then consumes this service through its already planned `ShellNetwork::spawn_enrolled(tree, keypair, identity, registry, fleet) -> io::Result<Self>`. The strict network constructor's **generic implementation** may be landed with Task 1 under this exact signature; native Task 6 consumes it and owns config/main wiring, rather than implementing a duplicate. Controller preflight adopted this producer reassignment in both tracked plans. Do not duplicate constructors. Tests here use explicit crypto fixtures/actors, not native bootstrap.

## File ownership

| Files | Responsibility/task |
| --- | --- |
| new `ui/src/record_collaboration.rs`; export in `ui/src/lib.rs` | Exact route, authority/transport errors, owner pump and status (Tasks 1–3). |
| `ui/src/collaboration.rs`; new `ui/src/collaboration/document_transport.rs` | SDK tracked send/completion, encryption checkpoint and reconnection transport (Task 2). Keep legacy request API. |
| new `shell/src/shell/record_collaboration.rs`; `shell/src/shell/mod.rs` | Explicit trusted broker/authority/endpoint provider and host owner attachment (Tasks 1, 3, 5). |
| `shell/src/shell/{collab_broker,collab_broker_capnp,network,collab_outbound}.rs`; `shell/src/network/{actor,identity}.rs` | Current authenticated admission, ingress/fanout/history validation, truthful failures (Tasks 1–2). |
| `shell/src/shell/collab_mls_authority.rs` | Exclusive durable document crypto adapter shared with governance (Task 2). |
| `ui/src/{record,app,application,bound_dispatch,headless}.rs`; `crates/collab/actions/src/executor.rs` documentation only; `ui/src/widget/tile_host.rs` | Owner attachment, current permissions, no outer-state merge, component subscription (Tasks 3–4). `application.rs`/`bound_dispatch.rs` are predecessor files. |
| new `ui/src/instance_presence.rs`; existing `ui/src/presence/{key,mod}.rs`, `shell/src/shell/collab_presence.rs` | Full shared codec/freshness/snapshot request (Task 3); local subscription/follow presentation (Task 4). |
| `shell/src/shell/{room_records,hosted_inputs,doc_open}.rs` | Validated discovered record metadata/exact open and host pump (Tasks 3, 5). |
| new `shell/src/shell/{canonical_record_collaboration_tests,instance_record_presence_tests}.rs`; `shell/src/shell/mod.rs`; colocated UI codec tests | Non-GPU owner/lifecycle/identity tests (Tasks 3–4). |
| new `shell/src/shell/{record_collaboration_tests,real_record_collaboration_tests}.rs`; colocated SDK/broker/network tests | Real certified RPC + actor transport, authority and real-app integration (Tasks 1–5). Register as library modules. |

## Task 1: Produce explicit, current authenticated record authority

**Consumes:** Existing certified join proof/`DeviceCert`, explicit `IdentityRegistryStore::at`, `DeviceFleetStore::at`, `WorkspaceTreeHandle`, `PeerIdentityStore`, exact route helpers and `RoomRecords`; common owner identities. No native window/config.

**Produces (new `ui::record_collaboration`):**

```rust
#[derive(Clone, Debug)]
pub struct RecordRoute { // private, validated; app/workspace/record/kind never mutable
    app_id: String, workspace_id: uuid::Uuid, checkout: RecordCheckout,
}
impl RecordRoute {
    pub fn new(app_id: String, workspace_id: uuid::Uuid, checkout: RecordCheckout)
        -> Result<Self, CollaborationError>;
    pub fn app_id(&self) -> &str;
    pub fn workspace_id(&self) -> uuid::Uuid;
    pub fn checkout(&self) -> &RecordCheckout;
    pub fn sdk_route(&self) -> crate::collaboration::RouteSpec;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollaborationError {
    InvalidRoute, BindingMismatch, NotReady, Unavailable(String),
    Denied(String), InvalidIdentity(String), InvalidDocument(String),
    Persistence(String), Transport(String), Overloaded, Busy, Closed,
}
#[derive(Clone, Copy, Debug)]
pub enum DocumentPurpose { Live, History }
// Trusted host interface; never serialized or constructed from app-supplied actor IDs.
pub trait RecordAuthority: Send + Sync {
    fn local_device(&self) -> [u8; 32];
    fn local_author(&self) -> crate::permissions::UserId;
    fn permissions(&self, route: &RecordRoute)
        -> Result<crate::permissions::PermissionContext, CollaborationError>;
    fn authorize_document(&self, route: &RecordRoute, op: &collab_types::SignedOp,
        purpose: DocumentPurpose) -> Result<(), CollaborationError>;
    fn authorize_read(&self, route: &RecordRoute) -> Result<(), CollaborationError>;
}
```

`RecordRoute` equality/hash uses app/workspace/id/kind only, not `RecordCheckout.references` (the latter is not Hash). Implement manually; reject nonempty references as routing authority or retain them only as metadata outside the key. Validate app with existing package validation, nonnil UUID, nonempty record/kind. Do not normalize/rewrite stored IDs or derive package from record kind.

**Produces (new shell module):**

```rust
pub struct EnrolledRecordAuthority { /* private explicit registry/fleet/tree,
    local root/device, PeerIdentityStore, current room-reference reader */ }
impl EnrolledRecordAuthority {
    pub fn new(tree: WorkspaceTreeHandle, registry: IdentityRegistryStore,
        fleet: DeviceFleetStore, local: &crate::network::actor::HostIdentity,
        peers: crate::network::identity::PeerIdentityStore)
        -> Result<Self, ui::record_collaboration::CollaborationError>;
}
// Additive broker entrypoint; original spawn_native/spawn_native_harness unchanged.
pub fn spawn_enrolled_broker(socket: PathBuf, network: Option<Arc<ShellNetwork>>,
    authority: Arc<EnrolledRecordAuthority>) -> std::io::Result<BrokerHandle>;
impl ShellNetwork {
    pub fn spawn_enrolled(tree: WorkspaceTreeHandle, keypair: iroh::SecretKey,
        identity: crate::network::actor::HostIdentity,
        registry: IdentityRegistryStore, fleet: DeviceFleetStore) -> std::io::Result<Self>;
    pub(crate) fn peer_identities(&self) -> crate::network::identity::PeerIdentityStore;
}
```

Private exact seams in `collab_broker_capnp.rs`: `verify_join_with_authority(route: &CrdtRoute, bytes: &[u8], authority: &EnrolledRecordAuthority) -> Result<AdmittedJoinIdentity, String>`; `revalidate_admission` retains signature but stores original signed presentation/cert and rechecks it. In network: add full `origin: PeerOrigin` to `InboundDocument` and `InboundAppPresence` (keep `peer` only for display), plus `pub(crate) fn revalidate_document(&self, entry: &InboundDocument) -> Result<collab_types::Role, String>`. Drainers call this before policy/record acceptance, never trust queued `role`. Broker fanout/subscriber stream `next`/history paths revalidate recipient read access before releasing queued data, not just at subscribe time.

### Authenticated cooperative-history seam (Task 1 producer, Task 2 consumer)

Actual path is `ShellNetwork::dispatch_neighbour_msg(ws, peer, msg)` → dispatch_scrollback_request/response → handle_scrollback_* → inbound queue → `DrainQueues::drain_scrollback_requests/responses`. Current request/response structs retain only requester/responder u64; current response fanout removes pending[artifact] and sets exhausted=true without authenticating the queued responder/author. The wire response is gossip broadcast, **not** a private reply. Keep existing NeighbourMsg layout/ordinals, but add these internal queue fields and explicit ingress methods:

```rust
// New fields on both existing InboundScrollbackRequest / InboundScrollbackResponse.
pub origin: Option<crate::shell::state::PeerOrigin>;
pub workspace_topic: Option<u64>;
// New fields originate only in dispatch_neighbour_msg's authenticated peer/ws.
impl ShellNetwork {
    pub(crate) fn queue_scrollback_request_from(&self,
        origin: crate::shell::state::PeerOrigin, workspace_topic: u64,
        requester: u64, artifact: collab_types::ArtifactId, before_seq: u64, max: u16);
    pub(crate) fn queue_scrollback_response_from(&self,
        origin: crate::shell::state::PeerOrigin, workspace_topic: u64,
        responder: u64, artifact: collab_types::ArtifactId,
        ops: Vec<collab_types::SignedOp>);
}
impl EnrolledRecordAuthority {
    pub(crate) fn register_route(&self, route: &RecordRoute)
        -> Result<(), CollaborationError>;
    pub(crate) fn route_for_artifact(&self, artifact: collab_types::ArtifactId)
        -> Result<RecordRoute, CollaborationError>;
    pub(crate) fn authorize_remote_read(&self, route: &RecordRoute,
        origin: &crate::shell::state::PeerOrigin) -> Result<(), CollaborationError>;
}
```

Existing public diagnostic `handle_scrollback_*` signatures remain compatibility wrappers with origin/topic None; strict canonical handling rejects missing evidence. `register_route` stores the exact validated route in an authority-owned artifact index on admitted join/activation; equal artifact with different route is BindingMismatch. Derive kind from the current held reference, never hash reversal or active room. At request dequeue, resolve exact route, compare supplied authenticated topic with room_topic_id, refresh remote DeviceCert/revocations and the requesting person's current Read capability/reference. `authorize_read(route)` remains **local** authority and cannot substitute. At response dequeue, require a still-pending bounded exact route request and current local recipient read access; revalidate the authenticated responder as a current room member/readable-route participant. Every contained SignedOp retains its original author/signature for separate current author verification; a valid responder does not vouch for another author. Do not consume pending state on malformed/unsolicited/revoked response. Preserve SignedOp through CollabEvent::ScrollbackBatch/Document and reauthorize at canonical execution after dequeue. Hashes are diagnostics only.

**Canonical legacy disposition:** Task 2 disables automatic cooperative gossip history fallback for strict canonical routes and refuses to emit old-history ScrollbackResponse for those routes, even to an allowed requester. Existing NetCmd::Publish cannot privately deliver it, and forwarding old MLS ciphertext to the whole room would not enforce an exact reader's current access. Strict local broker history stays available under current per-op policy. Canonical remote catch-up is Task 3's authenticated fresh SnapshotRequest → currently authorized owner → signed snapshot encrypted at the current shared MLS epoch. An authenticated cooperative response from an older peer is not adopted by canonical owners; it leaves CatchingUp and records unavailable history. Legacy managers retain existing wire/caller behavior and their tests; existing authenticated remote legacy requests still receive the new full-origin/current-member checks. An exact-reader historical peer service is a genuine absent primitive, explicitly unqualified here; implementing a separate generic private history transport is not needed for current-record convergence and must not be invented from the workspace-control channel. Tests cover revoked requester after enqueue, forged requester hash, removed/expired responder, wrong topic/artifact, mixed-author operations and delayed queue processing.

**Task-local test producer:** `AuthorityFixture` in `record_collaboration_tests.rs` has `async fn start() -> Self`, `async fn open_editor(&mut self) -> runtime_collab::ChannelHandle`, `fn signed_incremental(&self) -> Vec<u8>`, `fn revoke_editor_device(&mut self)`, and `fn accepted_document_count(&self) -> usize`. It creates real temporary registry/fleet + signed certificates, live workspace and production strict broker, opens a certified SDK channel, and signs a typed exact-route mutation. Revocation writes the existing signed fleet revocation through the real store. Count uses broker audit/subscriber observations, not a fabricated success counter.

```rust
#[tokio::test]
async fn authority_rechecks_device_after_channel_join() {
    let mut fixture = AuthorityFixture::start().await;
    let channel = fixture.open_editor().await;
    let bytes = fixture.signed_incremental();
    fixture.revoke_editor_device();
    assert!(channel.send(&bytes).await.is_err());
    assert_eq!(fixture.accepted_document_count(), 0);
}
```

- [ ] Write broker/network library regressions first: correct root/device/proof succeeds; missing proof, foreign local root, unenrolled/revoked/expired device fail; nickname cannot elevate role. Queue an operation, revoke/remove capability/reference/expire certificate, then drain: no document/presence/history bytes delivered. Request a different app/record artifact on an admitted channel: reject. Unknown historical author has no guessed identity.
- [ ] Run isolated focused nextest selection `test(record_collaboration_tests::authority_)` plus existing certified join/document tests; expect behavioral failure on stale admission and wrong-artifact history, not a GPU/runtime prerequisite.
- [ ] Implement explicit-root broker policy and strict network constructor with existing actor/protocol; never call `load_or_create_keypair`, `selected_identity_profile` ambient helper, `from_env` fleet, or `host_identity_for_key` fallback on this branch. Retain legacy constructor behavior explicitly; the shared authenticated ingress corrections below intentionally reject forged/stale history traffic. Validate network endpoint device key against supplied cert before binding.
- [ ] Refresh local permissions from current selected identity, fleet, workspace `role_of`, `capabilities`, person principal + app principal + exact room. Use `PermissionContext::from_capabilities(...).at_unix_time(now)` per execution; its construction-time `now` is otherwise stale. Missing/read failure produces a denied context, never retains the previous permissive one. Read exact `WorkspaceTree.room_records_held[room][record]` and match kind, ensuring the host synchronously updates it when the admitted room index changes; do not rely on a stale once-per-frame mirror. Read the workspace's replicated room references, not local active room or `KnownRecord.home`.
- [ ] For remote identities use full authenticated `PeerOrigin` → stored signed `DeviceCert`, current additive revocation state and current workspace membership. Do not require another person's root to equal the local selected root. Local process join still requires the enrolled local root/fleet. Verify signature/route/artifact/op class before using author; retain current revocation-at-receipt checks even for older signed history. Replay/history must check the requested artifact equals deterministic joined route, current recipient read access, current authenticated author policy, signature, MLS epoch/channel and identity of decoded record. Unknown/offline author evidence returns unavailable history; a live authorized peer can supply a fresh snapshot. Never waive authorization for backlog or pretend that empty history means an empty record.
- [ ] Keep current TTL/replay protection for new live operations. Local broker history delivery of already broker-validated stored operations is not fresh live ingestion: allow old timestamps only through the bounded exact-route history path, with signatures, current admission, redaction and dependencies rechecked. This is a deliberate correction to false-success/stale-authority behavior on shared Linux paths, not a protocol change.
- [ ] Green criteria: real signed RPC operations, queued revocation, expired cert and history tests pass with zero document mutation on failure; legacy fixture tests still run. Commit `feat(collab): bind record transport to current enrolled authority` after the gates below.

## Task 2: Make document transport acknowledge failure and persist encryption state

**Consumes:** Task 1 authority/route; existing `CertifiedEndpointIdentity::new(presentation, device_key)` and the worker/SDK internals behind `for_route_fd_with_identity` (not its blocking UI call), `DocumentMutation`, `SignedOp`, `ChannelGroup`, `MlsVaultProvider`, actual Cap'n Proto send and peer publisher.

**Produces (new UI document transport module, reexport through record_collaboration):**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentSendId(pub u64); // owner allocates monotonically within attachment
pub enum DocumentTransportEvent {
    Ready,
    BrokerAccepted(DocumentSendId),
    Failed { id: Option<DocumentSendId>, error: CollaborationError },
    Event(crate::collaboration::CollabEvent),
}
pub enum DocumentCryptoOperation {
    Encrypt(Vec<u8>),
    Decrypt { channel: collab_types::ChannelId, epoch: u64, ciphertext: Vec<u8> },
}
pub struct DocumentCryptoResult {
    pub channel: collab_types::ChannelId, pub epoch: u64, pub payload: Vec<u8>,
}
pub trait DocumentCryptoStore: Send {
    fn prepare(&mut self) -> Result<(), CollaborationError>; // worker-only vault checkout/validation
    fn channel_id(&self) -> collab_types::ChannelId;
    // Entire current-group lookup, crypto operation and durable checkpoint are
    // one serialized transaction; a split load/persist interface would race governance.
    fn apply(&mut self, operation: DocumentCryptoOperation)
        -> Result<DocumentCryptoResult, CollaborationError>;
}
pub enum RecordConnectSource {
    Connected(std::os::fd::OwnedFd),
    UnixSocket(std::path::PathBuf), // same existing typed broker socket
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointPhase { Connecting, Ready, Stopping, Stopped }
#[derive(Clone)]
pub struct RecordWorkerReaper { /* shared route registry + retired worker ownership */ }
impl RecordWorkerReaper {
    pub fn new() -> Self;
    pub fn poll(&self) -> usize; // joins only JoinHandle::is_finished(), returns count reaped
    pub fn pending(&self) -> usize;
}
pub struct CertifiedRecordEndpoint { /* one worker, queues, immutable route, cancellation */ }
impl CertifiedRecordEndpoint {
    pub fn start(source: RecordConnectSource, route: RecordRoute,
        identity: crate::collaboration::CertifiedEndpointIdentity,
        authority: Arc<dyn RecordAuthority>, crypto: Box<dyn DocumentCryptoStore>,
        retirement: RecordWorkerReaper) -> Result<Self, CollaborationError>;
    pub fn phase(&self) -> EndpointPhase;
    pub fn send_document(&mut self, id: DocumentSendId,
        mutation: collab_types::DocumentMutation) -> Result<(), CollaborationError>;
    pub fn send_presence(&mut self, key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload) -> Result<(), CollaborationError>;
    pub fn fetch_history(&mut self, before_seq: u64, max: u16)
        -> Result<(), CollaborationError>;
    pub fn poll(&mut self) -> Vec<DocumentTransportEvent>; // try_recv only
    pub fn cancel(&mut self); // signal + invalidate delivery; never joins a live worker
    pub fn reap_finished(&mut self) -> bool;
}
pub trait RecordConnector { // main-thread provider; not required to be Send
    fn start(&mut self, route: &RecordRoute)
        -> Result<CertifiedRecordEndpoint, CollaborationError>;
}
```

All these are **new producers**, not guessed existing SDK methods. Endpoint uses the existing worker, not a second thread: introduce private `SdkCommand::DocumentTracked { id, mutation }` and a private completion channel. `send_document_mutation` becomes a fallible async helper returning `Result<(), String>` to tracked caller; legacy sends may log their error but must not populate success status. `start` constructs channels and spawns the existing shovel worker without waiting for SDK/primary-ready or crypto I/O. It returns Connecting; `Ready` is emitted only after authenticated primary join and required crypto preparation. The worker performs Unix socket connection, existing SDK bootstrap, join, signer operations and vault preparation. Do not call blocking `for_route_fd_with_identity`, `spawn_sdk`'s recv_timeout, or `await_primary_route` on the canonical UI path. Refactor their shared worker startup so legacy constructors keep their existing blocking compatibility wrapper; canonical start observes the same ready event nonblockingly. Startup has a five-second deadline and cancellation selection, including stalled RPC join. The endpoint stores the same monotonic deadline; its nonblocking poll notices timeout even while a synchronous provider has stalled the worker, invalidates delivery, emits Failed and signals cancellation. A provider's synchronous call cannot be preempted safely: on timeout/cancel mark Unavailable/Stopping, suppress late publication, retain that worker, and do not start another worker for the record until it is reaped. `cancel`, `poll`, and Drop never join an active thread. `reap_finished` checks JoinHandle::is_finished before joining; Drop transfers unfinished handle/queues/provider ownership to the one host `RecordWorkerReaper`. The host polls its reaper and reports pending teardown in its existing retained-close mechanism; no unbounded detached workers or automatic successful exit while owned cleanup remains. The shared reaper registry reserves each exact route across active and retired workers; a replacement owner/start sees Busy until the prior worker is finished/reaped, even if the old RecordStore was dropped. Ordinary host teardown retains this registry until pending==0; forced process termination is outside that graceful guarantee. One bounded request queue; no second connect thread or thread per view. `send_document` return means enqueued only; only `BrokerAccepted` means authenticated broker accepted. No network peer acknowledgement exists in current wire, so no `Synced` state is produced here.

**Produces (shell crypto adapter):**

```rust
#[derive(Clone)]
pub struct RecordCryptoOwner { /* Arc of private per-handle serialized endpoint/governance state */ }
impl RecordCryptoOwner {
    pub fn new(vault: Arc<dyn super::collab_mls_authority::MlsVaultProvider>) -> Self;
    pub fn register_route(&self, route: RecordRoute, handle: runtime_collab::MlsVaultHandle)
        -> Result<(), CollaborationError>;
    pub fn lease(&self, handle: runtime_collab::MlsVaultHandle,
        route: &RecordRoute) -> Result<Box<dyn DocumentCryptoStore>, CollaborationError>;
}
```

Task 2 produces additive `MlsAuthorityCoordinator<P>::open_with_record_crypto(root: PathBuf, provider: Arc<P>, workspaces: WorkspaceTreeHandle, broker: Arc<Mutex<BrokerChannels>>, crypto: RecordCryptoOwner) -> Result<Self, MlsAuthorityError>` and extends its transaction implementation to share the **same** `RecordCryptoOwner` when a document endpoint is active. Private `with_group<R>(&self, handle: MlsVaultHandle, f: impl FnOnce(&mut ChannelGroup) -> Result<R, CollaborationError>) -> Result<R, CollaborationError>` loads/holds the current group and governance signer under one per-handle mutex, applies the closure, persists before returning its result, and poisons/reloads on failure; no independent checkout can overwrite another worker's epoch. Lease creation allocates only the authenticated opaque vault handle/expected route; `DocumentCryptoStore::prepare` performs checkout/channel validation on the shovel worker. The lease stores only the authenticated opaque vault handle; app code receives neither keys nor paths. A test-only sealed provider uses `ChannelGroup::{save_sealed,load_sealed}` with explicit temporary files and `VaultKey`; production adapter calls existing vault checkout/persist retaining its governance signer. Vault unavailability remains `Unavailable`, including on Mac; this task does not create a raw-key fallback.

- [ ] Write real certified broker-send tests with a broker which accepts the socket but withholds join completion, a stalled crypto provider, a dropped socket, failed channel send, malformed signature, replay, denial after enqueue, oversized queue, unavailable crypto provider and injected persistence failure. Assert no `BrokerAccepted` for failure. Add sealed group restart tests: encrypt/decrypt advances state, restart from checkpoint, next actual peer message decrypts; failed persist prevents publication/delivery. Two leases for one handle cannot race; governance mutation and document traffic serialize and persist one current epoch. A gated test worker blocks on a channel-controlled fixture: start returns Connecting, two ordinary owner/UI ticks and cancel return while that gate remains closed; no sleeps are used to let the UI timeout. Release gate, reap, assert no late Ready/document event and no replacement worker existed concurrently.
- [ ] Run RED selection `test(record_collaboration_tests::transport_)` and SDK tests for send/encryption; demonstrate current swallowed broker validation and missing persistence acknowledgement.
- [ ] Change `handle_signed_op_send(...) -> Result<(), capnp::Error>` and return it from `CapnpChannelHandle::send`; keep all existing RPC method ordinals and payloads. Propagate validation and required crypto receipt failures; `ChannelState::record_validated_op` currently returns unit and is not a durable storage acknowledgement. Do not manufacture one. Transport status must preserve publisher/network queue failure separately from accepted local broker state. Local accepted broker log with no peer route may still acknowledge local acceptance; it must never claim remote delivery. `PeerPublishSink::app_document` and `ShellNetwork::publish_document` currently return unit. Preserve those legacy methods. Task 2 adds `PeerPublishSink::try_app_document(&self, app_id: String, cluster_id: String, record_id: String, envelope: SignedOp) -> Result<(), String>` with default Err("queue acknowledgement unsupported"), and `ShellNetwork::try_publish_document` with identical arguments/return, returning the actual existing publish_message result. Add `PeerPublisher::signed_document_checked(&self, route: &CrdtRoute, envelope: SignedOp) -> Result<(), String>` for strict broker use; existing signed_document/legacy sinks keep behavior. Strict ShellNetwork and test RecordingSink implement the new method. A wired peer queue rejection returns RPC failure after local acceptance (an uncertain-send retry), while an intentionally local-only broker reports only local acceptance. This never proves peer delivery.
- [ ] Before a tracked send after Ready: refresh authority, validate route and nonempty typed mutation, encrypt using the serialized group, durably checkpoint advanced encryption state, sign with admitted device, await existing `channel.send`. Any failure returns Failed. Never reuse rolled-back MLS sender state after failed persistence; close/fail the endpoint and reload durable state. Receiver decrypts/validates under that same serialized endpoint, persists advanced state before delivering plaintext event; replay is ignored/rejected without altering document.
- [ ] Resolve a critical boundary: a returned success for broker delivery is not an acknowledgement of peer disk durability. Preserve owner dirty document regardless. Retry uses a **fresh signed snapshot/current changes**, never blindly resends an old MLS ciphertext or old sequence. Interrupted sends remain uncertain until catch-up; no fake remote acknowledgement field is introduced.
- [ ] `CertifiedRecordEndpoint::start` uses owned FD with exact close-on-error ownership; certified signer is an explicitly named enrolled-device backend, not diagnostic ephemeral fallback. Add private `SdkCommand::InstallDurableEncryption { spec: RouteSpec, store: Box<dyn DocumentCryptoStore>, reply: mpsc::SyncSender<Result<(), String>> }`; its SDK route owns the serialized store adapter, not a detached group copy. Bind/install required encryption before outbound document or accepting plaintext events. Existing unencrypted legacy managers remain compatible; canonical encrypted mode never downgrades.
- [ ] Green criteria: certified local sends fail visibly, good sends acknowledge broker only, group restart/replay/governance tests pass. Commit `fix(collab): acknowledge document sends and checkpoint encrypted endpoints`.

## Task 3: Attach one transport to each canonical owner and recover real records

**Consumes:** Accepted common `RecordStore`/exclusive persistence/UndoLog APIs, Tasks 1–2, existing App mutation decode path. No `App` constructor or window is required.

**Produces (`ui::record_collaboration`, `ui::record`):**

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordLinkState { Offline, Connecting, BrokerConnected, CatchingUp,
    Blocked(String), Closed }
#[derive(Clone, Debug)]
pub struct RecordCollaborationStatus {
    // BrokerConnected means local RPC admission only, never peer convergence.
    pub link: RecordLinkState,
    pub queued_local_changes: bool,
    pub broker_accepted_heads: Option<Vec<automerge::ChangeHash>>,
    pub last_remote_heads: Option<Vec<automerge::ChangeHash>>,
    pub last_error: Option<CollaborationError>,
    // Disk durability is queried separately with RecordStore::save_status.
}
pub trait NewRecordAdmission { // main-thread trusted host; invoked only by creator token
    fn publish_reference(&mut self, route: &RecordRoute) -> Result<(), CollaborationError>;
}
pub struct RecordCollaborationInput {
    pub route: RecordRoute,
    pub authority: Arc<dyn RecordAuthority>,
    pub connector: Box<dyn RecordConnector>,
    pub new_record_admission: Box<dyn NewRecordAdmission>,
}
pub struct DeferredRecordAttachment { /* non-Clone: store + reservation generation + inputs */ }
impl crate::record::RecordReservation {
    pub fn defer_collaboration(&mut self, input: RecordCollaborationInput)
        -> Result<DeferredRecordAttachment, CollaborationError>;
}
impl DeferredRecordAttachment {
    pub(crate) fn activate(self) -> Result<RecordCollaboration, CollaborationError>;
}
impl crate::application::HostContext {
    pub fn with_collaboration(mut self, input: RecordCollaborationInput)
        -> Result<Self, crate::launch::LaunchError>;
    pub(crate) fn activate_collaboration(&mut self)
        -> Result<(), CollaborationError>;
}
#[derive(Clone)]
pub struct RecordCollaboration { /* weak owner slot identity; no RefCell cycle */ }
impl RecordStore {
    pub fn attach_collaboration(&self, route: RecordRoute,
        authority: Arc<dyn RecordAuthority>, connector: Box<dyn RecordConnector>)
        -> Result<RecordCollaboration, CollaborationError>; // Ready only; Reserved => Busy
    pub fn pump_collaboration(&self) -> Result<bool, CollaborationError>;
}
impl RecordCollaboration {
    pub fn status(&self) -> RecordCollaborationStatus;
    pub fn reconnect(&self) -> Result<(), CollaborationError>; // request, nonblocking
    pub fn retry(&self) -> Result<(), CollaborationError>;
}
// Explicit remote hydration is different from New schema initialization.
pub struct RecordHydration { /* owner reservation, exact route, staging graph only */ }
impl RecordStore {
    pub fn begin_hydration(&self, route: RecordRoute,
        authority: Arc<dyn RecordAuthority>, connector: Box<dyn RecordConnector>)
        -> Result<RecordHydration, CollaborationError>;
}
impl RecordHydration {
    pub fn poll(&mut self) -> Result<Option<RecordCheckout>, CollaborationError>;
    pub fn status(&self) -> RecordCollaborationStatus;
}
```

`RecordHydration` is a new required primitive, not `try_open_record` silently creating. It reserves the canonical exclusive gate only for an authorized remote record already held by the workspace. It does not invoke an app schema initializer or change any source data. Task 3 adds private `RecordSet::publish_hydrated(route: &RecordRoute, document: AutoCommit) -> Result<RecordCheckout, RecordOpenError>`: exact metadata/schema validation, one baseline/history, canonical checkpoint, one Ready transition. `RecordReservation` remains exclusively New-owned; a second New/open/hydration sees Busy. If a valid local existing record appears, use it; never overwrite it from staging. Ready existing is merged through the owner. Invalid staging has no published writer/doc; close retains structured failure, not an empty success record.

Private owner seams (define beside owner slot in `record.rs`):

```rust
fn apply_remote_document(doc: &mut AutoCommit, route: &RecordRoute,
    mutation: collab_types::DocumentMutation) -> Result<bool, CollaborationError>;
fn changes_since(doc: &mut AutoCommit, heads: &[automerge::ChangeHash])
    -> Result<Vec<u8>, CollaborationError>;
```

`changes_since` uses the Automerge change graph (`get_changes` + raw change bytes/load_incremental contract at the pinned version), not `save_incremental`'s global save cursor. Snapshot serialization occurs on a clone so the writer and transport never steal each other's cursors. Remote apply clones the canonical graph, applies/merges into staging, validates full record metadata **including all conflicts** via `get_all`, app-state schema and scoped room/home, then publishes the staged graph preserving local actor and owner history. Incremental application which adds hidden conflicting identity fields is rejected even if the winning field happens to look valid. No partial mutation on decode/merge/identity failure. Finalize the original pending local action boundary before a foreign merge, without attributing remote heads to local undo or rebasing away prior undo entries.

- [ ] Add `canonical_record_collaboration_tests` library tests through actual SDK/broker: two instances, one store slot/worker/exclusive writer, edits through both reach a second owner; another package/workspace with equal record ID does not. Record mutation never appears in outer shell `State`. Autosave between local edit and transport pump must not steal delta bytes; transport before autosave must not lose disk writes.
- [ ] Add recovery tests: connected local edit + rejected send stays dirty/offline; disconnect → edit → durable flush → destroy all handles → restart same owner → reconnect → authenticated peer observes edit. Remote concurrent edits merge after reconnect. Revocation during offline period prevents later publishing; local bytes remain recoverable and status is blocked. Flush error retains the common close report's strong owner after last tile closes; no second writer starts for retry.
- [ ] Run RED focused selection `test(canonical_record_collaboration_tests::)`; actual missing owner integration must fail without launching GUI.
- [ ] Store one attachment per canonical slot. `attach_collaboration` accepts only Ready with no pending creator attachment; Reserved or creator-pending Ready returns Busy before consuming/replacing its connector. For New, `HostContext::with_collaboration` uses its private non-Clone reservation to call `RecordReservation::defer_collaboration`; that method checks exact reservation generation/route, installs one pending slot and returns the sole non-Clone activation token held by context. Repeated/mismatched deferral fails without changing existing input. The unique token holds a strong store handle through Ready. Initialization only marks Ready; it does not publish a reference or start transport. The checked instantiate wrapper calls the returned component context's activate_collaboration after successful factory/Ready/restore validation and before mount. Activation checks the same token/generation, invokes host reference admission outside any record borrow, installs the single attachment and requests nonblocking start only after admission. Ownership/route errors return Err from activate. Admission/start failure returns Ok of a Blocked attachment and keeps Ready local bytes plus the original pending admission/connector; retry uses that retained owner and rechecks authority. Factory failure/cancellation before activation drops the creator token, removes only its pending slot, and starts no worker/reference; a Ready record is still retained by common ComponentSetup error. Cancelling before Ready cancels the reservation normally. Another opener cannot reserve the creator's route connector during Reserved→Ready. Test both pre-Ready and post-Ready/pre-activation contention, cancellation, wrong-generation token, BindingMismatch and successful once-only activation. Repeated `attach_collaboration` with identical route and same authority identity returns shared handle; different supplied authority/device/route returns BindingMismatch. No reference cycle: store strongly owns attachment; component-facing handle weakly refers to owner. Host/common failed-close report provides the intentional strong owner lifetime. Shared store registry weak semantics stay unchanged.
- [ ] Pump in ordered phases outside mutable document borrow: current authority → transport polling/reconnect → validated incoming staging → document merge/history boundary → notify instance subscriptions → capture outbound changes → queue sends → autosave. Never call widgets or block on RPC while the record RefCell is borrowed. Call connector.start only when the prior worker phase is Stopped/reaped; while it is Stopping report unavailable and do not overlap workers. Pump/reap never blocks on startup, RPC or active-worker join. Reconnect backoff: 250 ms, 500 ms, 1 s, 2 s, capped 5 s, driven by host monotonic tick; explicit retry resets it, Closed never restarts. Endpoint failure retains canonical document and pending heads. No thread per view.
- [ ] On attachment/reconnect, nonblockingly start and wait for Ready, then request exact-route local broker scrollback and a fresh snapshot from connected record peers. Reuse `FetchScrollback` for authorized local history; the complete Task 3 codec below produces SnapshotRequest over existing Resource presence. Canonical remote catch-up uses a fresh authorized snapshot at current MLS epoch, not cooperative gossip forwarding of old ciphertext history. No temporary SDK control or Task 4 prerequisite exists; no new RPC or peer message variant. Respond only after read/write/current record authority; empty history is CatchingUp, not successful hydration. Existing live owners send a snapshot on authorized arrived/recovery request; canonical peers process it as ordinary signed encrypted DocumentSnapshot. Local queued heads are only compared/advanced on broker acceptance, not enqueue, and are recomputed from graph after remote merge.
- [ ] For hydration fetch authenticated bounded history into staging until dependencies resolve or a fresh valid snapshot arrives. No factory runs while absent/incomplete. Keep at most 16 MiB staged bytes/256 pages, max 50 ops/page; overflow is explicit recoverable error, never truncated success. A receiver must not invent record ID/schema/home. On accepted remote route/document use `RoomRecords::seed_observed(ObservedRecord { id, kind, home, package_id: exact_route_app })`; require workspace reference separately. This gives Files exact app routing without a new app schema/import mechanism.
- [ ] Keep legacy `App::broadcast_doc_delta`/record checkout APIs for old callers. Component-bound App/Driver/TileHost branch calls the common owner pump and does not apply record events to outer State. Shared decode/identity logic is reused, but do not silently tighten legacy unscoped document assumptions into canonical new-data creation.
- [ ] Green criteria: real delta exchange, duplicate attachment, independent writer/cursor, tampered identity, cold hydration and disk restart cases pass; document and history identity remain exact. Commit `feat(ui): attach collaboration to canonical record owners`.

### New record admission/reference producer (Task 3)

The existing `ShellOverlayHost::room_records()` returns the one `Rc<RefCell<RoomRecords>>`; FrameBuilder::replicate_room_records currently bridges it to WorkspaceTree per frame. Reuse that owner, not another RoomRecords or a stale scan. Task 3 implements private `HostedNewRecordAdmission { authority: Arc<EnrolledRecordAuthority>, rooms: Rc<RefCell<RoomRecords>> }` in shell/record_collaboration.rs with the exact `NewRecordAdmission::publish_reference` trait above. Its trusted input is created only for an explicit New launch/creator token, never a received snapshot or Existing open.

Task 1 adds private `EnrolledRecordAuthority::authorize_new_record_in(&self, tree: &WorkspaceTree, route: &RecordRoute) -> Result<(), CollaborationError>`: check current enrolled local root/device, current workspace membership and app+person Write capability for the exact prospective artifact, omitting only the nonexistent held-reference test. This is not an app-callable grant. Task 3's publisher rechecks that policy under the same WorkspaceTree write guard used for the mirror update, then obtains the shared RoomRecords mutable borrow. It rejects mismatched existing kind, calls existing `RoomRecords::reference(room, id, kind)`, and verifies exact id/kind through records_in: false+already-identical is idempotent success; false+absent is an error (including generation exhaustion). It then calls existing `WorkspaceTree::set_room_records(room, exact records_in map)` before releasing authority. Register the exact route and activate transport only after both reference and synchronous authority mirror are present. No network worker is started while either borrow is held. This reuses existing index persistence; success here asserts in-memory authority publication, not index or record disk durability. Any actual save failure remains visible through retained save status and cannot be labelled saved.

Order: authorized explicit New → common unique initialize → Ready → successful factory/local View restore → creator-token activate → current New admission → RoomRecords reference → synchronous set_room_records → exact route registration → connector.start → authenticated worker Ready → document/presence traffic. Failure before reference leaves the local Ready record and a Blocked attachment retaining retry input, with no worker; failure after reference leaves the authorized local reference but truthful unavailable transport. Never delete/replace local data on network failure. Hydration/Existing use current existing reference authorization only and cannot call this New producer. Include tests for denied publication, exhausted index generation, mismatch/idempotent retry, second-opener preemption, immediate broker check before another frame, and hydration with no reference (zero mutation/factory/worker).

## Shared presence codec produced in Task 3

Task 3 creates `ui/src/instance_presence.rs` with the **complete** stable codec and validation below, including both Instance and SnapshotRequest arms. Task 3 also implements reserved-key broker/network scope/freshness validation and SnapshotRequest handling needed by real catch-up. Task 4 adds component subscription and presentation behavior only. This order is independently compilable/testable.

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PresenceInstance {
    pub device: [u8; 32], pub instance: u64, pub mount: uuid::Uuid,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum RecordPresenceContent {
    Instance { source: PresenceInstance, sequence: u64, key: Vec<u8> },
    SnapshotRequest { request: uuid::Uuid },
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecordPresenceFrame {
    pub version: u16, // exactly 1
    pub issued_at_ms: u64, pub expires_at_ms: u64, // bounded freshness, not durable incarnation
    pub workspace: uuid::Uuid, pub record: String,
    pub content: RecordPresenceContent,
}
pub fn encode_record_presence(app: &str, frame: &RecordPresenceFrame)
    -> Result<crate::presence::PresenceKey, CollaborationError>;
pub fn decode_record_presence(key: &crate::presence::PresenceKey)
    -> Result<Option<(String, RecordPresenceFrame)>, CollaborationError>;
pub fn validate_record_presence(frame: &RecordPresenceFrame, now_unix_ms: u64)
    -> Result<(), CollaborationError>;
```

Encoding is existing `PresenceKey::Resource { app: exact_app, kind: "qos.record-presence.v1", id: hex(postcard(RecordPresenceFrame)) }` and existing `PresencePayload`. `key` inside Instance is one encoded **ordinary** PresenceKey (nested record envelopes forbidden). Version unknown/oversized/bad UTF8/invalid UUID/nonfinite view coordinates invalidates only the update. Cap total key at 16 KiB; max view scroll entries 64; TTL 15 s and 5 s heartbeat. Source mount is freshly generated per construction; durable instance is not a lifetime. Sequence is strictly increasing within a remembered `(authenticated device, instance, mount)` while alive. Source stamps issued_at_ms once when the live instance first publishes; the broker checks it but never redates a queued/replayed frame or extends its expiry. expires_at_ms must equal issued_at_ms + 15_000 without overflow. Reject future issuance >2_000 ms, expiry <= receiver wall time, invalid span, and clock-unavailable states. Sender heartbeat is fresh publication, never replay of retained bytes. Use monotonic 15-second receipt TTL in addition to wall-clock expiry. These fields are in the existing authenticated peer payload; an admitted sender can publish its own current presence but cannot claim another device. This is a bounded freshness guarantee, not an indefinite mount replay guarantee: after old frame expiry, late old bytes are rejected even if the tombstone was evicted or either UI reopened. Within the 15-second plus clock-skew bound an unseen old mount may still be listed until Left/new presence/expiry; it never automatically becomes a follow target. Follow pins the complete explicit source mount. Retain at most 4096 presence entries and bounded tombstones; overflow sheds ephemeral entries without a success claim. If clock assumptions are unavailable, canonical remote presence/follow is unavailable rather than claiming replay safety. A new local component always has a new WakeScope, so stale callbacks for the old local mount fail independently of this network freshness window.

This is a minimal **shared, versioned application payload extension** within existing opaque-key transport, not native wire. Pin old PresenceKey/PointerSpace/PeerPresence bytes; no reordered/appended enum ABI required. Legacy publishers retain room-wide behavior and existing follow UI. Legacy receivers see an unknown Resource kind and do not decode it as ordinary View; mixed-version instance follow/recovery availability is explicitly absent, not silently mapped to one user's last view. Ordinary legacy document mutations remain interoperable.


## Task 4: Bind ephemeral presence and explicit follow to component instances

**Consumes:** Task 3 owner attachment; planned `HostContext` live permissions/wake handle, `AppComponent` lifecycle/View helpers, common bound dispatcher; Task 1 authentication; existing key/payload/peer transports.

**Produces (`ui::instance_presence`, exact compatible encoding):**

```rust
pub enum FollowPresentationUpdate {
    Unchanged,
    Present { target: FollowTarget, view: crate::view::ViewContribution },
    Clear,
}
pub struct InstancePresence { /* weak record attachment, fresh mount, local peer map/follow */ }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowTarget { pub principal: String, pub source: PresenceInstance }
impl InstancePresence {
    pub fn publish(&self, key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload) -> Result<(), CollaborationError>;
    pub fn follow(&self, target: Option<FollowTarget>) -> Result<(), CollaborationError>;
    pub fn take_follow_presentation(&self) -> FollowPresentationUpdate;
    pub fn close(&self);
}
impl AppComponent {
    pub fn follow_record_instance(&mut self, target: Option<FollowTarget>)
        -> Result<(), CollaborationError>;
}
```

Task 4 consumes `PresenceInstance`, `RecordPresenceContent`, `RecordPresenceFrame` and `encode_record_presence`/`decode_record_presence`/`validate_record_presence` produced in Task 3 verbatim. No redeclaration or enum reordering occurs here.

Private producer seams in component/owner (all `pub(crate)` across UI modules): `RecordCollaboration::route(&self) -> Result<RecordRoute, CollaborationError>`, `RecordCollaboration::permissions(&self) -> Result<PermissionContext, CollaborationError>` and `RecordCollaboration::local_device(&self) -> Result<[u8; 32], CollaborationError>` expose only authenticated owner identity/snapshot; `RecordCollaboration::subscribe(&self, instance: u64, wake: WakeHandle) -> Result<InstancePresence, CollaborationError>` (derive device/record from authenticated owner; allocate mount internally); `HostContext::refresh_permissions(&self) -> Result<(), CollaborationError>` (fresh authority into its existing live handle); `InstancePresence::accept(&self, principal: &str, frame: &RecordPresenceFrame, payload: PresencePayload, now: Instant, now_unix_ms: u64) -> Result<bool, CollaborationError>`. Public caller cannot provide another device identity to subscribe.

- [ ] Write tests for A/B local instances of one record with distinct Views, two remote instances of same user, another record/app/workspace, spoofed device, old mount after reopen, duplicate/reordered/late Left, bad version/overlimit key, explicit follow/unfollow, remote focus/action presence and absent target. Assert canonical heads and UndoLog do not change after any presence/follow update. Assert A follows remote X only; B keeps its selection/scroll. Unfollow, target expiry/loss, close and reopen restore the independently owned local View. Persist only that owner's View, never the peer contribution. Test close while actively following, reopen with the original local focus/selection/scroll, a second local instance remaining unchanged, and a delayed old-mount frame after both TTL expiry and a reopen with tombstones discarded.
- [ ] Run RED selection `test(instance_record_presence_tests::)` plus real broker presence tests, showing existing room widening/one-user View map cannot satisfy instance assertions.
- [ ] Broker parses reserved key, verifies app/workspace/record equals its full admitted channel and current reference/read authority, and verifies/stamps source device from certified session before fanout. Route these envelopes with full record route. Legacy keys keep room widening. Incoming Iroh path verifies full PeerOrigin certificate/current membership, compares source.device with authenticated device, derives principal from its root and derives record from validated frame; don't trust network display user/hash or envelope's claim. Carry exact record through local queue/fanout; reject contradictory scopes before delivery. SnapshotRequest carries no source identity authority beyond authenticated peer route.
- [ ] Component receives its context-owned `RecordCollaborationInput` through Task 3's exact with_collaboration method before instantiate. After creator-token activation/Existing Ready attachment, Task 4 creates the subscription before mount; context/store/record/app/workspace/device must match. No Task 4 builder grants access to Reserved ownership. A blocked admitted-local record can mount with visible collaboration error and no presence publication. Mount drains stored subscription events under the component's WakeScope. Close first ends follow/restores owned View for common capture, then marks closed/removes subscription, emits best-effort Left only if authority remains, and closes wake as common lifecycle requires. A failed Left still expires remotely; closed/replaced scopes never wake or apply queued views.
- [ ] Before each bound callback that can mutate/launch action, resource invocation/start/poll, undo/redo and incoming apply: call the owner authority refresh and write the exact existing Rc permission handle. Discovery refreshes too but is not execution authority. Failure swaps a denied context and rejects callback, retaining recoverable document. This hook is synchronous/short shared authority data; do not rely on a previous frame's context event or strings in `CollabEvent::Context.permissions`.
- [ ] Preserve existing `ActionTransport` and shared `invoke_resource_action` door, confirmation evidence and invocation result/cancellation semantics. Canonical ClientLocal document actions change the owner once; their already-committed Automerge changes carry existing ActionContext provenance. Implement private `RecordActionTransport { upstream: Box<dyn ActionTransport>, observed: Rc<RefCell<Vec<qos_actions::ActionEnvelope>>> }`: replicate clones the **actual** envelope into observed (not a no-op), while submit_to_host delegates real upstream/default Unavailable. Immediately after common dispatch returns and before another invocation, the bound door drains observed into private `observe_committed_action(doc: &mut AutoCommit, history: &mut UndoLog, author: &UserId, instance: u64, envelope: qos_actions::ActionEnvelope) -> Result<(), CollaborationError>` in bound_dispatch.rs, using the already-borrowed canonical document/history pair. It must not reborrow RecordStore while that callback is active. This replaces, rather than duplicates, that branch's existing common record_committed_action_in call: correlate envelope.base_heads/actor/context with the committed graph and record the original per-instance boundary once; the owner's normal head comparison schedules delta transport after the borrow ends. The temporary vector is bounded by one synchronous invocation; rejection reports the owner error without pretending to undo an already committed change. It does not call upstream.replicate, because that would also replay the document action. This adapter's replication representation is the exact committed CRDT change, not a second serialized ActionEnvelope stream. Clarify existing qos-actions ActionTransport/Dispatched docs accordingly: AppliedLocal means locally applied and handed to the configured replication mechanism, not remotely acknowledged. Keep legacy transports/dispatch tests and envelope replay behavior unchanged. The existing executor apply_under_envelope commits ActionContext::change_message with pinned actor/time; ui::state::revision and ui::history_source read that metadata after real delta merge. Test equal sender/receiver change hashes, actor/time/resource/action context, one history/activity item and no second apply when the delta arrives; local event observer receives the actual envelope. No-op actions have local observed envelopes but create no invented durable activity change. Broker-confirmed ServerPreferred already uses apply_local_with_context in App's action door: preserve that path, committed provenance and owner delta scheduling without requiring a fake replicate call. Effect activity remains existing RecordLocalEffect/InvocationUpdate through the broker, never an invented document action. The host supplies upstream explicitly; do not fabricate host acceptance. Per-instance resource directories dispatch exact `BoundActionAddress`; instance selection is never inferred from foreground tile.
- [ ] Draw canonical per-instance presence through component binding while keeping outer shell desktop PresenceCtx. Reuse `ViewSubset` solely to derive transient `ViewContribution { scroll, view_mode }` for explicit matching follow. Do not change record target from `open_resource`; mismatched record subset rejected. The exact presentation boundary below temporarily restores peer contribution on the widget for rendering, while AppComponent.local_view remains the owner's independent state. Follow never calls common persisted View restore/capture with the peer contribution. Remote caret/selection is an overlay, never local focus. Incoming action presence is descriptive, not executable.
- [ ] Green criteria: actual broker/peer instance envelope tests, per-instance follow/headless component tests and all old ordinal/presence tests pass. Commit `feat(ui): scope record presence and follow to component lifetimes`.

### Owned View versus transient presentation (Task 4 producer)

Keep the common `AppComponent.local_view: View` as the owner's only persisted View. Add private `follow_presentation: Option<(FollowTarget, ViewContribution)>`; it contains no owned focus/selection and is never serialized. InstancePresence emits Present only for the explicit target, Clear on unfollow/expiry/target loss, and Unchanged otherwise; the component cannot confuse target loss with no update. At initial follow entry capture the latest owned View through the common capture helper before installing transient presentation. Task 4 produces these component-private seams used by the same bound root in App, Driver and TileHost:

```rust
impl AppComponent {
    pub(crate) fn with_presented_view<R>(&mut self,
        present: impl FnOnce(&mut dyn crate::widget::AppWidget) -> R) -> R;
    pub(crate) fn end_follow_and_restore(&mut self);
    pub(crate) fn release_follow_for_local_input(&mut self) -> bool;
}
```

`with_presented_view` enters the live component scope, saves only the root's owned ViewContribution, applies the already-authorized followed contribution with the existing root.restore_view_state hook for the presentation traversal, invokes presentation, and restores the owned contribution with a private RAII guard even on unwind. It never recaptures AppComponent.local_view from the followed widget. The bound adapter brackets the complete measure/layout/apply/draw presentation pass, including descendant traversal, not just root.draw; per-instance field contributions remain owned by that component. Its private `PresentedViewGuard<'a> { root: &'a mut dyn AppWidget, owned: ViewContribution }` has `fn new(root: &'a mut dyn AppWidget, peer: &ViewContribution) -> Self` and Drop restores the saved owned contribution. The presentation closure accesses root through that guard's exclusive borrow. Its guard changes presentation only and does not invoke document actions or histories. Local View capture/persistence and ordinary callback execution always happen with the owned contribution restored. Follow changes invalidate presentation layout, not document state.

On local pointer/key/IME/semantic document input, `release_follow_for_local_input` clears follow, restores owned presentation and requests layout reconciliation before that input is hit-tested/dispatched; queue that original input for the next reconciled frame if necessary. This avoids using followed hit-test coordinates against the owned viewport. Explicit unfollow/target loss/expiry/close uses end_follow_and_restore before common close captures View; close subsequently invalidates lifetime in the established sequence. Following a different remote instance replaces only transient contribution. Presence broadcasts the owned View, not another person's followed viewport, avoiding chains/feedback. Tests render followed scroll, then unfollow/expire/close and capture+reopen the exact original local View, including focus/selection. Two components sharing a record render independently; no remote View is adopted into local_view or saved envelope. Same-record document edits arriving during follow update shared content normally while owned viewport survives.

## Task 5: Wire the real shared host and prove authenticated cross-host behavior

**Consumes:** Tasks 1–4; actual `notes::definition()` from Notes Tasks 1–3; `file_explorer::application_definition()` and single `FilesServiceOwner` from Files Tasks 1–6; common `ApplicationDefinition::instantiate`, `TileHost::from_component`, `RecordOwners` and bound dispatch. Native Task 6 remains a downstream consumer, never a harness prerequisite.

**Produces (`shell::shell::record_collaboration`):**

```rust
pub struct HostedRecordCollaboration { /* explicit authority, broker/network owners,
    crypto registry and reconnect inputs; no canonical document copies */ }
impl HostedRecordCollaboration {
    pub fn new(authority: Arc<EnrolledRecordAuthority>, broker: BrokerHandle,
        crypto: RecordCryptoOwner, identity: crate::network::actor::HostIdentity,
        rooms: Rc<RefCell<super::room_records::RoomRecords>>)
        -> Result<Self, CollaborationError>;
    pub fn component_inputs(&mut self, route: RecordRoute)
        -> Result<ui::record_collaboration::RecordCollaborationInput, CollaborationError>;
    pub fn poll_retired_workers(&self) -> usize;
    pub fn pending_worker_teardown(&self) -> usize;
    pub fn attach(&mut self, records: &ui::record::RecordStore,
        route: RecordRoute) -> Result<RecordCollaboration, CollaborationError>;
    pub fn prepare_hydration(&mut self, records: &ui::record::RecordStore,
        route: RecordRoute) -> Result<ui::record_collaboration::RecordHydration,
        CollaborationError>;
}
```

**Certified signer producer (Task 5):** `HostIdentity` already contains public `device_cert`, concrete `device_signing_key: ed25519_dalek::SigningKey`, `workspace_device_certs: HashMap<WorkspaceId, DeviceCert>` and signed workspace revocations. Pass this explicit value to the host service; authority is a verifier, not a secret accessor. Change Task 1 `EnrolledRecordAuthority::new` to borrow `local: &HostIdentity` and retain only expected public identity/cert information, so it does not consume or expose the signing secret. The already-enrolled native bootstrap/isolated fixture owns the original HostIdentity and supplies the necessary owned values to network and service. Validate exact public-key equality against authority and current registry/fleet. Private `HostedRecordCollaboration::endpoint_identity(&self, route: &RecordRoute) -> Result<ui::collaboration::CertifiedEndpointIdentity, CollaborationError>` selects **only** workspace_device_certs[room_topic_id(exact room)], validates that cert's root/device match current authority and the concrete signing key, serializes the existing DeviceCert with serde_json, signs runtime_identity::collaboration_join_signing_bytes(exact workspace string), encodes existing runtime_collab::CertifiedJoinIdentity, then constructs existing CertifiedEndpointIdentity::new(presentation, device_signing_key.clone()). Never use HostIdentity::material_for's ambient/fallback branch or derive a key from IDs. Missing exact cert/secret returns typed unavailable before start. Hardware-only/nonexportable custody requires its separately qualified signer provider; do not fabricate a SigningKey. The service retains its supplied identity, not an absent authenticated process issuer.

`component_inputs` produces the exact Task 3 input with current route, shared authority, private nonblocking connector and HostedNewRecordAdmission using the same rooms owner. The connector only packages RecordConnectSource::UnixSocket(broker.capnp_socket().to_path_buf()), explicit endpoint identity, lease descriptor and shared reaper, then calls CertifiedRecordEndpoint::start. It performs no synchronous socket connection/join/vault load. `attach` is Ready-only for direct owner consumers; New component callers use component_inputs → HostContext.with_collaboration → creator token. The service owns one RecordWorkerReaper and exposes pending teardown to PreparedShellHost's existing HostCloseStatus/HostCloseIssueKind::Teardown rather than inventing a successful synchronous close. All stopped workers are reaped before ordinary ReadyToExit; stalled provider teardown remains a truthful pending issue.

`HostedRecordCollaboration` retains broker/network/crypto handles until all attachments/retained dirty owners are closed or recoverably reported. `new` validates explicit identity but cannot invent key provisioning: exact route → already admitted opaque group handle must be registered through Task 2 owner. Use `RecordCryptoOwner::register_route(&self, route: RecordRoute, handle: MlsVaultHandle) -> Result<(), CollaborationError>` from Task 2 above (validate channel equality on checkout, reject different handle for live route). `component_inputs`/`attach` build the same private connector using the explicit signer producer above, existing typed socket and exact route registry. Missing group/provider is typed unavailable and local offline record remains usable only where current authority allows; it does not qualify encrypted collaboration.

`PreparedShellHost` (native Tasks 5–6) retains this host service as a field; native Task 6 constructs it from strict config/enrolled inputs and calls `HostContext::with_collaboration` on the same prepared context. Existing Files `FilesServiceOwner` is reused, never recreated here. Shell frame and common headless runner invoke `RecordStore::pump_collaboration` once per owner tick plus an instance event drain under each bound scope; network wake only schedules the host owner pump, not a selected/outer app. Actual app factories remain unchanged except consuming optional typed collaboration in HostContext.

**Meaningful integration fixtures (new library test support, private):**

```rust
struct RecordPairFixture { /* two temporary explicit profiles, two distinct user roots,
    enrolled device certs, real WorkspaceTrees, brokers, Iroh actors,
    sealed test crypto providers, actual canonical stores and real app catalogue */ }
impl RecordPairFixture {
    async fn start() -> Self;
    async fn pump_until(&mut self, predicate: impl Fn(&Self) -> bool);
    async fn disconnect_second(&mut self);
    async fn restart_second(&mut self);
    fn deny_second_editor(&mut self);
}
```

These methods are produced in Task 5 test module, not preexisting fixtures. Build from existing `shell/src/network/actor.rs` tests' `identified_endpoint`, `enrol`, real `Actor::spawn_reporting_addrs`, bounded timeouts and `NetCmd::JoinWorkspace/ConnectHost`; do not call production's private test helper by an inaccessible name. Move shared crypto fixture helpers to a `#[cfg(test)]` library support module only if reused. Use actual `ShellNetwork` authenticated event processing + actual broker publisher/drainer; forwarding in-memory `CollabEvent` or `NeighbourMsg` manually does not qualify actor path. The actors must observe certificates for two different roots and actual encrypted signed `AppDocument` bytes on both directions, and assert failure on wrong endpoint signer.

Extend this private fixture with `fn create_page_first(&mut self, title: &str) -> Result<(), qos_actions::DoorError>`, `fn second_has_page(&self, title: &str) -> bool`, `fn second_store(&self) -> &RecordStore`, `fn second_checkout(&self) -> &RecordCheckout`, and `fn second_link(&self) -> RecordCollaborationStatus`. `create_page_first` discovers the actual `notes.notebook` resource's `new-page` action from `apps/notes/src/actions.rs:847`, then uses common bound dispatch with the title argument and actual authenticated local author. `second_has_page` reads the actual Notes model projection after component sync. No direct document put/merge is used to simulate an edit.

```rust
#[tokio::test]
async fn notes_files_actual_action_crosses_authenticated_actors() {
    let mut pair = RecordPairFixture::start().await;
    pair.create_page_first("A real shared page").unwrap();
    pair.pump_until(|p| p.second_has_page("A real shared page")).await;
    assert!(pair.second_has_page("A real shared page"));
    pair.second_store().try_flush().unwrap();
    assert!(matches!(pair.second_store().save_status(&pair.second_checkout().id),
        Ok(ui::record::RecordSaveStatus::Saved)));
    pair.restart_second().await;
    pair.pump_until(|p| p.second_has_page("A real shared page")).await;
    assert!(pair.second_link().last_error.is_none());
}
```

- [ ] First create real-app test `real_record_collaboration_tests::notes_files_two_actors_reconnect_and_revoke`. Construct actual Notes via definition; invoke a real Notes action through common dispatcher; open its exact referenced record from real Files catalogue on peer; wait for peer's actual Notes widget projection. Repeat simultaneous edits and two local views. Check schema content, document heads, app/workspace/record/kind/home and disk bytes after close/reopen. No shell Notes substitute, fake model, snapshot supplied directly by test, or `apply_collab_event_for_test` is allowed in this gate.
- [ ] RED must show real missing host attachment/presence/hydration or transport integration; not a missing GPU/window. Assert actual network transport counters/events and nonzero signed document traffic so local shared Rc success cannot hide a missing wire path.
- [ ] Add action/permission integration: discover then revoke before execution, queued incoming revoke before application, viewer cannot edit but can receive allowed read/history, workspace reference removed, wrong app/artifact and forged sender cannot read/merge. Test author/UndoLog stays local to invoking instance; remote edit remains after that instance's undo. Display alias must not equal/elevate authorization principal.
- [ ] Add offline/restart test with actual disk persistence and actor reconnect, group reload, fresh signed snapshot catch-up and observed remote heads; remote-only view/focus traffic must not alter document bytes. Disconnect before/after enqueue and before acknowledgement; no status can report Synced or Saved on that evidence. Inject durable writer failure and retain close recovery through shared host owner while sibling tile continues normally.
- [ ] Implement host service integration and publish capability/status honestly: record local save status from canonical `save_status`; link/last error/broker accepted/remote observed from actual service. Missing transport returns required-provider reason where capability requires it. Network connected without group/record catch-up cannot count as live collaboration. Do not serialize host-native paths, grants, local following or caches into Notes/Files collaborative metadata.
- [ ] Preserve every portable shell/UI integration target. Register new tests as library modules reachable on macOS independently of native main. Before final acceptance, controller runs full target portability gates after native Task 6 classifies individual true Linux modules. No whole `shell/tests/it.rs` gate, ignored portable tests, successful stub main or model-only fixture substitutes. Required Mac↔Fedora physical Notes/Files collaboration walkthrough remains a later exact-SHA controller gate; this author does not launch either GUI/VM without controller assignment.
- [ ] Green criteria: real authenticated two-actor real-app test passes on both hosts, failures remain truthful, library/portable targets compile; native final integration and physical cross-host evidence explicitly recorded separately. Commit `feat(shell): connect real hosted apps to canonical record collaboration`.

## Mandatory RED/GREEN and controller gates

For each task list discovered test names before execution and require nonzero discovered/executed counts. Use `cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --message-format json` with the identical package/target/filter selection; verify the JSON contains at least one selected test, then use run with --no-tests fail. All package selection uses the root graph, never a nested standalone lockfile. Controller runs subagent-driven-development with source authors serialized, task review/validation gates and no user check-in between already-authorized tasks. Native command prefix:

```sh
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
```

Fedora prefix in controller's isolated checkout:

```sh
CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
```

Append one payload below per invocation (do not run parallel cargo processes against the same target). All tests use the root manifest/lockfile/config with `--locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail`, isolated helper, explicit temp paths, no HOME/XDG mutation; process isolation is mandatory even where old tests mutate environment. Preserve current `.config/nextest.toml` GPU scheduling and add `--test-threads 1` whenever a selected regression touches a real GPU; none of this plan's focused tests requires GPU. Explicit retries=0 overrides the profile default retries=1, so flaky reruns cannot count as first-pass green. Record exact SHA/rustc host/target and full command, pass count and error text. Author runs native gates then self-reviews and commits; controller reviews concrete commit, syncs **that SHA** and repeats Fedora gates before next source task. A baseline blocker requires parent-SHA reproduction, not an unexplained skip.

| Task | Focused tests (in addition to legacy regression selections named in tasks) | Check/lint scope |
| --- | --- | --- |
| 1 | `cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p shell --lib -E 'test(record_collaboration_tests::authority_)'` | `cargo check --manifest-path Cargo.toml --locked -p shell --lib`; `cargo clippy --manifest-path Cargo.toml --locked -p shell --lib -- -D warnings`; UI all targets below |
| 2 | `cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p shell --lib -E 'test(record_collaboration_tests::transport_)'`; `cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p ui --lib -E 'test(collaboration::)'`; channel-crypto sealed/governance regressions | Shell library plus `channel-crypto` all targets, UI all targets |
| 3 | `cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p shell --lib -E 'test(canonical_record_collaboration_tests::)'` | UI/qos-app/app-state all targets; shell library |
| 4 | `cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p shell --lib -E 'test(instance_record_presence_tests::)'`; shell library reserved/legacy presence regressions | UI/qos-app all targets; shell library |
| 5 | `cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p shell --lib -E 'test(real_record_collaboration_tests::)'` | `ui`, `qos-app`, `notes`, `file-explorer` all targets; shell library until native Task 6, then shell all targets on both hosts |

Expand “UI all targets” to `cargo check --manifest-path Cargo.toml --locked -p ui -p qos-app --all-targets` and `cargo clippy --manifest-path Cargo.toml --locked -p ui -p qos-app --all-targets -- -D warnings`; include app-state/channel-crypto/packages indicated in the same command. Run `cargo fmt --manifest-path Cargo.toml --all --check` on both hosts every task. Fedora additionally retains shell `--all-targets` checks/clippy throughout. Final required shell all-target Mac gate is a sequencing dependency on native Task 6's actual main/target classification, **not** waived portability. No merge/push is required for plan task commits. After a normal push, wait for automatic final CI and use `fgj actions` on failure; relevant CI must be green before merge.

## Explicit unresolved dependencies and self-review

1. The common framework APIs are adopted plan outputs, not source present in the inspected checkout. Tasks 3–5 wait for accepted owner/construction/dispatch/lifecycle commits. Task 1 owns strict `ShellNetwork::spawn_enrolled` under the controller's accepted producer ruling; native Task 6 consumes it. The controller updates both tracked plans on candidate adoption, avoiding a cycle through native main.
2. Real Notes/Files definitions, Notes schema/one-writer migration and exact Files opens are separate producers. This plan never imports old app state implicitly. Cold **remote** hydration of an already authorized exact record is a new producer in Task 3, not local app-data migration.
3. A first Mac profile still needs enrollment and a real secret provider. Existing Linux Secret Service MLS provisioning and opaque registered handles can supply Task 2's adapter; Mac Keychain and first-user MLS group acquisition/role invitation UI remain separate, currently unqualified. Missing provider must report unavailable. Fixture sealed providers prove checkpoint/crypto semantics but cannot qualify native Keychain or enrollment. No absent standalone issuer is assumed.
4. Current peer wire has no remote durability acknowledgement. This plan deliberately promises broker acceptance and authenticated remote observation only; Saved is local writer evidence. It cannot honestly label these facts “fully synced everywhere.” Adding stronger peer durability acknowledgement would require a separately reviewed shared versioned protocol extension.
5. Exact-reader historical peer retrieval is absent: the current gossip response has no private delivery boundary. Canonical remote current-record catch-up uses a fresh snapshot at the current MLS epoch; when no currently authorized peer can supply one and local history is insufficient, status remains CatchingUp/unavailable. This is not an empty/synced result.
6. Source-backed audit found current history authorization, stale role/device checks and room-only instance presence insufficient. Tasks 1, 2, 3 and 4 explicitly produce their corrections and legacy disposition. The reserved existing Resource envelope has one producer shared by request/presence consumers; old enum/peer bytes are pinned. No new native-only message schema is introduced.
7. This candidate gives five reviewable source units, each with a meaningful real-path test cycle. Authority and reliable encryption/ack are separate because either can be rejected independently; owner replication and view/follow are separate because document semantics and ephemeral UI lifetime have different tests; final real apps are a distinct integration gate. No source/build/test/VM/GUI/git action was performed while writing it.

### Candidate self-review corrections

- Changed crypto adapter from a split load/persist contract to a serialized apply transaction; otherwise governance could overwrite an SDK ratchet checkpoint. Existing governance write-ahead ordering stays intact within the shared handle lock; no new independent group owner is accepted.
- Put actual-broker owner/presence tests in shell library modules, avoiding a UI→shell test dependency/native-main cycle. UI retains colocated codec/low-level tests.
- Restricted store attachment to Ready; non-Clone creator token owns deferred inputs across Reserved→Ready, and host reference admission precedes activation. Cancellation cannot preempt another connector.
- Preserved unit-returning legacy peer publisher methods; added explicit checked methods whose default is unavailable rather than guessed queue success.
- Identified CurrentPermission as the common private Rc handle, not an invented preexisting accessor; added exact new refresh hooks.

### Bounded preflight revision disposition

The controller preflight's ten items are addressed in the adjacent response artifact. Full codec production is in Task 3; creator-token ownership and host reference publication are explicit; follow is transient over separately owned View; presence provides a stated bounded freshness guarantee rather than indefinite UUID replay protection; history queues retain full origins; canonical remote catch-up does not use the existing room-broadcast historical reply; startup/cancel/reap are nonblocking on UI; actual action envelopes are observed and committed provenance travels in deltas; host signing inputs are explicit; validation uses root graph, retries 0 and /home/parallels/qos-target. No implementation, source changes, build or runtime validation is claimed by this revision.
