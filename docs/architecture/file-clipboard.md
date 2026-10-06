# Local Files clipboard transfer boundary

Status: proposed for architectural review; not implemented by the selection
actions increment. No permission, grant, broker or transfer policy is changed.

## Observed boundary

Files stores its clipboard on each `FileExplorerWidget`. The platform clipboard
currently carries text, rich text and images. A second Files window therefore
has no file offer to paste. Encoding paths as text would expose an untrusted
path list to a filesystem-capable receiver without revalidating its source.

The existing action broker can route Files-to-Files calls under the person's
current authority, confined to the admitted room
(`collab_broker_capnp/action_host.rs::app_door`). This is reusable; it is not
authority to change grants or to bypass either window's policy.

`FileOp::Copy` and `FileOp::Move` persist source and destination paths. Two
owned-scratch diagnostics against `file_ops::execute` at base `750c04d9e0`
demonstrated that replacing a queued source copies the replacement, and
replacing the destination directory writes into the replacement. These
diagnostics are retained as a patch and failed-run evidence, not ignored tests
or a failing committed suite.

Nearby Send already has descriptor-rooted `SecureNearbySource` and
`SecureNearbyDestination`. Its opaque handles are deliberately bound to a
peer and channel; passing one window's handle to another is prohibited. Copying
that service's path checks into Files would create a second authority boundary.

## Proposed reviewed extension

Add a local Files clipboard service on the existing authenticated broker lane,
with no new socket, sharing grant, permission override or trust inference.
Reuse the existing secure transfer primitives rather than a raw-path clipboard
or a process-global app registry.

* Copy/Cut publishes an opaque host-issued offer for a bounded snapshot of the
  selected resources. Paths and admission material remain private to the host.
  The offer records the source route, room, exact identities and copy/cut mode.
* Paste names that offer and a destination handle issued to the receiving Files
  window for the exact folder shown. The service checks the source and receiver
  independently under their current admitted identities and live grants. The
  first increment is within one workspace; cross-workspace transfer is refused.
* The broker derives the executing principal and lease from invocation custody.
  An agent/script Paste keeps that caller's restrictions through the app-to-host
  hop; the Files process must not substitute its signed-in person's authority.
  The source offer's holder is provenance, not the recipient's authority.
* Source reads, destination writes and Cut's source deletion use existing role
  and filesystem-grant decisions. An offer never delegates the person's role.
  Revocation, closure, expiry or identity replacement refuses the transfer.
* Resolve and retain descriptors beneath the approved grants. Recheck authority
  before irreversible work, and use descriptor-relative operations throughout
  staging/publication. A path comparison followed by reopening that path does
  not satisfy exact identity. Preserve symlinks as data without following them;
  preserve existing collision naming and cut recovery semantics.
* Use normal invocation IDs and receipts for retry. An unknown acknowledgement
  observes the original invocation rather than launching another copy. Partial
  Cut consumes only receipted entries of that offer, never a replacement offer.
* Cancel before commit discards private staging. Committed changes stay visible
  in history and are compensated through the existing undo service with exact
  recorded identities. A failed cut must restore its source or expose retained
  recovery material; it must not lose data silently.

## Required proof before landing

Run two live Files surfaces through the production broker using owned scratch
files: Copy, Cut, destination selection/navigation changes, stale and replaced
sources/destinations, source and destination revocation, closed source, clipboard
replacement, partial failure/retry, cancellation, lost-reply observation, undo
and retained recovery. Check collision and symlink behavior and bound offer,
descriptor and worker counts. Tests must fail if they cannot observe the actual
broker or execution subject. Existing role/grant decisions remain authoritative.
