# G02: smallest honest scoped-service slice

Read-only source review, 2026-09-18. No product files, index or HEAD changed. Names below marked proposed are not existing APIs. This is an implementation design, not native qualification evidence.

## Recommendation

Extend the existing portal with one scoped, bounded **choose-and-read** operation, used by Passwords CSV import. Keep CSV parsing, preview and vault mutations app-owned. This gives the framework a real production caller while keeping the selected native file and its lifetime entirely inside the service. Do not start with a general ServiceProvider registry, command executor, or trait family.

This is NOT a two-file HostContext accessor change. Production host preparation and live revocation are prerequisites. Land the caller, host construction and cancellable native path together, or describe the work as incomplete. A HostContext field consumed only in tests does not satisfy G02.

## Source-confirmed seams and gaps

- `ui/src/application.rs::HostContext::prepare` checks canonical record package/workspace and view namespace, holds `Rc<RefCell<PermissionContext>>`, app/device/instance identity, author, `WakeScope`, UUID lifetime and `closing`. `callback_binding` shares those exact owners. `AppComponent::close_owned` marks closing before capture and closes wake routing; `teardown` repeats it safely. This is the correct lifecycle owner.
- `HostContext::permissions()` is a snapshot; `ActionContext` is history metadata, NOT authenticated principal evidence. Identity for authorization comes from the existing `PermissionContext::actor`, authenticated host route and canonical namespace, not the `author` string alone.
- All found `HostContext::prepare` call sites are tests. `launch::dispatch_definition` requires `ComponentHostPreparer`; its registrations are tests too. Notes has a real component factory but its live launcher has not adopted that route. Do not pretend the existing component path is already production host composition.
- `ui/src/app.rs::App` separately owns legacy `permission_ctx` (default permissive), with `set_permission_ctx`. Using this default as authenticated host authority would silently widen access.
- `PermissionContext` stores wall time at construction. `check` therefore does not notice expiry merely because time passes. It has no change notification. Existing tests mutate its surrounding `RefCell` directly. Async revocation needs a real owner update path, not another copied context.
- `portal::PickRequest::send_open_file` has real callers: Passwords import/restore, Mail attachment staging, Windows installers/documents. Workers return channels and ignore disconnected consumers. They do not retain cancellable native sessions, check instance authority or wake the owner.
- `OpenedFile` privately owns a File but publicly transfers it via `into_file()`. A service cannot revoke/close a transferred or duplicated descriptor. Existing `LaunchRequest` reopens a caller-supplied path; neither it nor path-only chooser results provide a reusable scoped grant.
- Passwords is the best first caller: `Showcase::request_import` at `apps/passwords/src/showcase.rs` is reached by `PasswordOperation::ImportCsv`, UI and accessibility (`passwords.import`). `poll_transfers` takes the selected descriptor and starts a CSV parser thread. `transfer::parse_browser_csv` already takes `impl Read`, and `MAX_IMPORT_BYTES` is 8 MiB. No format redesign is needed.
- `runtime-portal::native::ensure_attached` rejects malformed/incomplete inherited connected Unix sockets, but connected sockets alone are not authenticated package/workspace authority. Native bootstrap is Unix-only and still includes developer Cargo fallback. Those G02 acceptance items remain separate unfinished work.

## Exact proposed integration shape

### One authority owner, shared with HostContext

First introduce a private framework `HostBinding` by extracting the existing identity/authority/lifetime fields of HostContext, not by inventing a parallel identity vocabulary or registry. Canonical `HostContext::prepare` must construct/validate it using the already supplied canonical namespace, existing actor and host-authenticated route. Record/view custody stays in HostContext; a portal does not need record access.

The same binding may serve the transitional legacy launcher, so migrating Passwords does not require moving vault secrets into a CRDT record merely to get a chooser. The legacy launcher must receive verified host inputs through the SAME host preparer and construct the SAME binding; it must not synthesize records or read an app-provided workspace string as proof. Add a narrow host-aware root-factory route to LaunchSpec (proposed `fn(&LaunchArgs, HostServices) -> Result<Box<dyn Widget>, LaunchError>`), owned/prepared by the framework. `HostServices` is a non-constructible, bound client value, not a provider registry. Alternatively migrate Passwords fully to the canonical component host first; that is a substantially larger task and should be planned separately.

HostContext exposes proposed `portal(&self) -> PortalClient`. A legacy native App owns the same lifecycle guard and closes it in its normal close/Drop path. Host/test constructors remain outside ordinary app SDK exports. Tests inject a backend only through test-support host preparation. Production factories receive clients and cannot choose backends or manufacture verified bindings.

A mandatory precursor is defining how production host preparation obtains and authenticates package/workspace/device/actor from runtime identity/session services. Reuse the existing broker-bound identity route; never infer authentication from environment metadata or append `Principal::App` to a person actor and call it verified. If this cannot be wired in the first patch, do not implement a client field and claim G02 integrated.

### One operation, no new action door

Proposed public semantic entry:

    PortalClient::choose_and_read(PickRequest, ReadLimit) -> PortalOperation<SelectedBytes>
    PortalOperation::try_recv() -> Result<PortalResult<SelectedBytes>, TryRecvError>
    PortalOperation::cancel()

`SelectedBytes` has a display name and bounded, zeroizing bytes; no path or native handle. There is no `into_file`, `AsRawFd`, File clone, or Serializable grant. Portal state retains any native authorization resource until the operation reaches an acknowledged terminal state. Since delivered bytes are ordinary data, later revocation cannot retract knowledge already delivered; it does close the file and prevents pending results from publishing. Vault mutation still goes through existing live action permission checks.

Use one existing import resource action and its existing permission requirement. Capture the actual invocation principal/authority, including agent calls, through the existing checked dispatch scope; do not let an agent's operation inherit the factory's more powerful local actor. The service verifies the app/action scope before showing UI and revalidates live authority before opening/reading the approved file and before publication. A chooser approval authorizes the selected file only. Do not require a broad FsPath grant before the chooser; `plan/security.md` correctly says the dialog IS the file grant. Outside Tilo, native approval must be mapped into an opaque operation-scoped proof by the host adapter.

Passwords stores a `PortalClient` supplied during construction. Change `request_import` to call `choose_and_read(import_request(), MAX_IMPORT_BYTES)`. Its `poll_transfers` feeds `Cursor`/slice bytes into the existing parser. Keep bounded parsing off the UI thread and route completion through the captured wake/lifetime; close cancels/discards parser publication too. The existing `PasswordOperation::ImportCsv` and accessibility handler still call `request_import`. For action progress/cancel, use existing resource operation polling/InvocationId machinery; do not add a portal-specific action bus. The operation owner can track private native sessions in a per-instance collection; that is resource ownership, not a global provider registry.

### Live authority and cancellation

Replace ad-hoc permission snapshot updates with one host-owned update operation that updates the existing permission model and increments a revision, notifying registered pending operations. Preserve `decide_capability`; do not create another policy evaluator. Workers need a thread-safe read/authorization view of that owner. A synchronized owner replacing the RefCell everywhere is clearer than two unrelated permission copies; if a bridge is necessary, updates and revision publication must be atomic and all writers must migrate. Direct mutable legacy `permission_ctx` fields are a compatibility hazard that must be closed or explicitly phased.

Use a live clock for normal PermissionContext checks; retain explicit fixed-clock injection for tests. Expiry must trigger cancellation even if no UI frame runs. A notification/timer owned by the operation can do this; once-per-frame polling alone cannot meet the contract.

Portal errors distinguish Cancelled, Denied, Revoked, Unsupported, Unavailable, Busy and Failed with sanitized diagnostics. Scope close and explicit user cancellation have deterministic terminal outcomes; failed native cleanup must not be reported as successful cancellation. Each operation needs a state machine so cancellation racing completion cannot publish a stale success. Revocation invalidates the operation that observed the old authority revision even if authority is immediately granted again.

`closing` is set before view capture: trigger cancellation there, not only after potentially slow/panicking capture. Drop/unwind and setup failures use the same path. WakeScope closure only prevents delivery; it does not cancel the dialog or close the operation by itself. Use existing `WakeHandle::notify_events` for result queues, with receiver-side lifetime validation. Never block the UI waiting for native close/reap; a framework cleanup owner retains the native session until acknowledged termination, then releases it. Instance IDs can repeat; bind UUID lifetime as well.

## Native prerequisites that cannot be hidden by mocks

Linux: the pinned ashpd 0.9.3 `Request::close` exists, but the chooser builder's `send().await` calls `Proxy::request`, which awaits `prepare_response` before returning Request. Therefore `select!` around existing `send` and dropping the future does NOT provide a usable close handle for a pending dialog. Implement a cancellable XDG request adapter using an owned zbus connection/request token, response subscription installed before send, validated request object path, and explicit Request.Close. Or use a deliberately reviewed ashpd patch exposing the pending handle. Closing must handle cancel-before-method-return as well as cancel-after-registration. Test a real fake D-Bus peer which receives Close and releases the request, plus a native portal smoke test later.

Linux confinement: `plan/security.md` says shipped HostPathStore does not reach already-running confined apps. Do not turn a returned path into an authority proof. A live broker/document-store transfer which reaches the app is required for confined production. Trusted-host read can work as a separate explicit policy but cannot be silently substituted for confinement.

macOS: XDG availability does not provide NSOpenPanel. Need a main-thread native chooser adapter, owned cancellation, security-scoped resource lifetime when sandboxed, and safe open/read. The current spawn-a-Tokio-thread model cannot simply be reused for AppKit presentation.

Windows: need native chooser apartment/thread/message-loop ownership, actual cancellation, granted handle lifetime and safe reparse-point policy. `File::open` in the current non-Unix fallback does not prove the Unix no-follow guarantee. `ui/src/portal.rs::launch` imports `std::os::fd::AsFd` under the feature without Unix gating. Native runtime service bootstrap/handle transfer also remains unavailable on Windows.

All hosts: bounded bytes limits memory, not latency or cancellation. Blocking reads of a FUSE/cloud-backed regular file may not cancel merely by dropping a worker receiver or another handle. The owned backend must provide a cancellable read mechanism or bounded helper process which is actually terminated/reaped. Never claim complete cancellation from chunk checks alone. A chooser-only first stage can keep reading outside its acceptance, but then it does not solve file-grant lifetime/revocation and cannot claim this complete slice.

## Focused tests and compatibility

1. New `ui/tests/scoped_portal.rs`: use production HostContext preparation and portal dispatch with a controlled private backend. Prove distinct workspaces, apps, actors, reused numeric instance IDs and lifetimes cannot consume or resurrect each other's pending result. No public proof constructors.
2. Denied preflight causes zero backend requests. Grant revoked while chooser is pending sends native cancel; completion raced afterward drops handle/bytes and yields Revoked. Regrant does not revive the old operation. Expiry is tested with an injected advancing clock without replacing the context.
3. Close, Drop, setup failure and panic during local-view capture cancel outstanding work; mock acknowledgements prove release exactly once; no post-close wakes or publications. Tests with no observable operation must fail.
4. Pinned Linux backend protocol test observes Request.Close, no hanging response wait, cancellation before request registration, user cancellation distinct from backend failure, non-local URI rejection and retained no-follow regular-file checks.
5. Passwords production `request_import` through button, accessibility and actual resource operation reaches the same scoped service. Cancellation/revocation does not alter vault or show stale preview. Existing parser tests retain byte/row limits. Two concurrent app fixtures use separate injected contexts, no global suppression.
6. SDK consumer compile tests: app can use PortalClient and result enums but cannot construct bindings, inject native providers or extract a native handle. Existing sdk wildcard export needs deliberate curation for these types.
7. Native host tests are additional evidence, not replaced by mocks or cross-compilation. Run focused affected suites, `cargo check -p shell --tests` as required, plus native/package evidence on each host being claimed.

Keep existing `PortalOutcome`, `OpenFileOutcome`, `PickRequest::send*`, and their exhaustively matching consumers source-compatible initially. New typed results belong to the scoped path; old APIs remain tracked migration exceptions and cannot count as scoped/revocable. Preserve stored app IDs, workspace namespace, broker protocol and file formats. Never make legacy APIs implicitly select whichever instance most recently became current. Keep process-global suppression limited to legacy tests; scoped tests inject independently. Do not remove legacy APIs until their caller migration is complete.

A practical delivery sequence is (1) verified production host binding + live update owner, (2) cancellable scoped portal/read implementation and real Passwords caller together, (3) remaining callers and native adapters. This design closes a real slice of G02/D01; packaged startup without Cargo, authenticated inherited-authority qualification, full cross-host adapters and every legacy caller remain explicit outstanding gates.

## Source refresh after receipt implementation

Repository-wide Rust call-site search still finds `HostContext::prepare` only in test fixtures and `.component_host(...)` registrations only in `ui/src/launch.rs` tests. Production `dispatch_definition` requires the explicit preparer and returns MissingComponentHost when absent. Therefore production authenticated host construction remains an actual G02 prerequisite; a new service field alone still cannot close it. Concurrent framework work has added a `ControlDomain` to HostContext and an owner-bound `control_domain()` accessor plus custody inheritance. Future scoped-service integration must reuse this evolving invocation/custody path rather than introduce a parallel owner model. This was read-only source inspection; no authority or runtime qualification is inferred.
