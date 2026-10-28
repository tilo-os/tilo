# Synced storage hydration and safe local reclaim

QuickFS is the sole authority for the physical residency of synced files. The
logical file remains a CRDT namespace entry; its content is a manifest and
content-addressed chunks in the iroh blob store. Reclaim removes only verified
local blobs. It never unlinks the logical file.

## Typed state

`runtime-fs::storage` exposes per-file identity, inode and content version plus
logical bytes, hydrated bytes, remote-only, dirty, pinned, in-flight,
remotely-durable and reclaimable state. The daemon aggregates the same records
by workspace and by an explicit `/apps/<app-id>/` namespace. It never scans a
host path and therefore never follows a symlink or attributes an arbitrary
directory name to an application.

Settings uses this canonical snapshot for the Synced Files total. It presents
logical/remote size separately from bytes on this device. “Free local space”
contains only records the daemon marked reclaimable; unsupported or offline
states are explanations, not decorative controls.

## Reclaim invariants

A file on the legacy namespace/hydration path is reclaimable only while online
and when it is clean, unpinned, not in flight, fully local, and every component
was previously fetched and bao-verified from one peer. Locally authored content
is not assumed durable on that path. The record replication plane now has an
authenticated, signed exact-version acknowledgement protocol and a durable
`AvailabilityRegistry`, but this Settings-facing namespace storage authority
does not consume those receipts yet.

Apply is preview/commit rather than path deletion. The client submits stable
node identity, inode and content hash. After remote verification, QuickFS
revalidates all three plus dirty and pin state. A mismatch aborts before GC.
Selective GC protects every unselected store blob and serialises against new
blob publication, including deduplicated chunks. Shared chunks are excluded
from per-file reclaim estimates. If the peer is offline or stops serving a
component, local content is kept.

The socket is `/run/tilo/fs-storage/storage.sock`, mode `0660`, alone in a
setgid `root:tilo-identity` directory. This deliberately reuses the existing
trusted device-user boundary and exposes only snapshot and safe-eviction
operations; it does not expose document bytes or general filesystem mutation.
Settings' system grant binds that directory into its sandbox. A host session's
Files service serves the socket from `$XDG_RUNTIME_DIR/tilo/files-storage`
instead, which the launcher binds at the same sandbox path.

## Deliberate conservative boundaries

- Remote-durability provenance for this legacy path currently lives for the
  daemon lifetime. After restart previously fetched files remain local but are
  not offered for reclaim until re-verified through normal hydration. The
  missing work is integration with the existing authenticated replica receipts
  and durable availability projection, not invention of another receipt type.
- A file whose chunks came from different peers is retained. Per-blob source
  receipts can relax this later without changing the public protocol.
- Application totals require the explicit app namespace. Legacy workspace
  layouts remain in the workspace aggregate and are not guessed.
