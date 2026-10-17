# Legacy document-media recovery authorization

## Explicit import creates new reachability

An explicit owner invocation of the registered
`workspace.legacy-media#import-legacy-media` action approves a **new import** of
selected resident content from the standalone runtime profile into its own room.
It does not certify historical ownership. Missing historical provenance is not
an additional refusal condition for this deliberate import.

Ordinary reads and offload still require an exact room media reference. Residency,
a known hash, room membership, mount ownership, or a newly written document is
not permission to read unscoped content. Automatic ownership-preserving migration
would need retained authenticated evidence binding the original account, room
and exact hash; this importer neither manufactures nor expands that evidence.

See [the standalone recovery command](legacy-media-recovery.md) for arguments,
limits, receipts and restart behavior.

## Implemented host and authority boundary

The implementation is **standalone-only**. `standalone/media_import.rs` registers
the typed action through the existing workspace governance action host.
`tilo-runtime --owner-action` takes the profile's normal exclusive lock, starts
its normal services and invokes that canonical action host. An active daemon
must be stopped first. There is no recovery socket, caller-selected source path,
foreign-profile importer or desktop filesystem-daemon recovery route. An adopted
read-only desktop workspace cannot use this service.

The operating-system owner boundary trusts the account's unconfined processes,
including administrator scripts and agents running as that account. It does
**not** prove a human gesture or distinguish a person from an unlabelled
same-account script. The desktop admin control socket likewise uses peer
credentials, namespaces and app ancestry to exclude other accounts and apps;
it is not human-presence attestation, and it does not expose this standalone
importer. A request's `Person` classification or confirmation boolean alone
would not authenticate a caller on an arbitrary transport.

Within the canonical action host, classified apps, scoped agents, bots,
Shortcuts, delegated/on-behalf-of callers and remote-device callers cannot
import. The standalone owner command supplies local owner custody from the
private profile boundary, not from an actor string in the request. It requires
the current room owner and current enrolled, unrevoked device identity.
Same-account trusted scripts are intentionally not excluded by these checks.

## Confirmation and existing witnesses

A canonical, explicit `Confirmed` owner invocation with the exact typed import
arguments is sufficient approval. No additional hardware gesture, witness or
consent service is required.

An existing applicable canonical witness for the exact owner action is also
sufficient: the broker consumes its existing, unexpired one-use grant, bound to
the actor, resource, action and exact arguments. It checks the canonical service
descriptor against current workspace policy before forwarding checked confirmation
to the importer. No second approval is required. A witness does not confer owner
custody: scoped or delegated actors still cannot import, and the host retains
its local-device, current-owner, enrollment and execution-lease checks. This
opt-in admission path does not change other governance services' semantics.

## Permit and publication

`crates/system/fs/src/document_media/import.rs` implements a private, bounded
host-held permit. It binds the fixed source store and unscoped source, exact
hashes, destination room, byte limit, approving account/device, argument digest,
invocation UUID, nonce and expiry. The importer cannot be called through the
ordinary blob socket to grant itself reachability.

Before publication it rechecks the originating execution lease, structural room
ownership and completed ownership-transfer evidence, current enrollment and
revocation. Unrelated task progress, effect receipts or layout changes are not
changes to owner authority. The maximum selection is 128 unique hashes and
256 MiB; execution is bounded to 120 seconds.

Publication durably spends the original invocation UUID. Completion is reported
only after destination references and the completion marker are durable. A
crash, timeout, authority change or I/O failure can leave partial new references,
not a success receipt. Reusing the UUID after restart refuses rather than
republishing; another attempt requires a fresh explicit invocation. Source bytes
and old roots are not deleted on success or failure.

## Verification locations

- `crates/runtime/tilo-runtime/src/standalone/media_import_tests.rs` exercises the
  canonical action door, refused caller classes, scoped reads, room-bound offload
  binding, invocation replay and the exclusive owner command.
- `crates/runtime/tilo-runtime/src/standalone/media_import/authority_tests.rs`
  exercises execution retirement, device revocation and authority revision
  stability across unrelated task/effect changes versus ownership changes.
- `crates/system/fs/src/document_media/import/` contains backend permit and
  publication tests. Ordinary room-scoped read tests remain in
  `crates/system/fs/src/document_media/tests.rs`.

These locations describe implemented tests, not a claim that every test passes
on every build host. Broker governance witness tests additionally cover exact
argument binding, one-use consumption, policy/custody refusal and preservation
of legacy governance admission.
