# Signed replacement-rename transactions

## Problem

`rename(source, destination)` is one POSIX operation even when `destination`
already exists. A successful call makes the source record visible at the
destination name and tombstones the replaced record; observers must never see
only one of those effects as a committed operation.

The signed filesystem now provides this guarantee for a locally initiated
replacement rename and for a complete transaction already present in the
record repository. `QuickFS` has two durable authorities:

1. the signed record operation log, persisted by `RecordRepository`, which is
   also the only durable file history; and
2. the Automerge move tree, persisted separately and broadcast as a delta.

`FileHistoryStore` is a derived, rebuildable query index over the signed
`AppendVersion`, `AppendConflict` and `ResolveConflict` operations. Its
checkpoint is a cache keyed by the record log digest, never a second durable
history authority. Publishing an ordinary rename version for the source and an
ordinary delete version for the destination would therefore be unsafe: a crash
between participant records creates an authenticated half-operation, while a
crash between record publication and the move-tree write leaves signed history
for a rename the namespace never performed. Returning an error cannot roll back
those immutable envelopes.

Signed mounts without a persistent transaction journal reject an occupied
destination. Production host/join mounts open that journal, recover it before
FUSE becomes visible, and route occupied-destination rename through the
transaction coordinator. Exact-share recipient mounts remain read-only and do
not create transactions.

## Implemented state

The version-1 wire manifest, detached signature, participant projection hashes,
prepare/commit envelopes, participant-completeness index, durable journal,
atomic completed-transaction record commit, derived history fold, namespace
projection, restart recovery, and local FUSE entry point are implemented.
Ordinary record snapshots expose no transaction projection until both exact
participants have matching prepares and commits. The record materializer
applies only complete repository transactions.

The implementation has unit coverage for signature and projection binding,
commit-before-prepare refusal, incomplete visibility, durable staging across
restart, repository failure after the first participant file, atomic derived
history, namespace replay, broadcast-failure recovery, and the production signed-mount
rename path. These tests are necessary evidence, but they do not satisfy the
release requirements below by themselves.

The same journal/coordinator now carries signed V2 binary Keep Both. It creates
a deterministic bounded sibling name and stable record, resolves the exact open
source conflict, and publishes both namespace projections atomically. Dedicated
tests inject failure after the derived-history fold but before resolution and
after namespace-delta staging but before publication, reopen at each boundary, and
prove exact idempotent recovery, no half publication, one resolution and an
empty journal. Independently delivered participants are tested in both orders.
Later conflicts may reference a committed transaction-projected revision while
repository-wide participant completeness remains the visibility authority.
Codec-aware Merge remains open and is not presented by Files.

The required collaborative-files CI job invokes this V2 recovery test directly
alongside the deterministic offline partition/reconnect matrix, explicit receipt
and grant clock-skew checks, and authenticated read-only peer write denial. None
of those required checks use skip-on-missing-environment behavior.

Remaining product gaps are explicit: local mutations share a process-wide
publisher lock rather than a general reservation/conflict service; concurrent
remote structural conflicts are rejected instead of persisted as a first-class
conflict; exact RecordProtocol sessions do not yet drive cross-record completion;
the live harness is endpoint/store-level rather than separate mounted OS
processes with forced partition and clock control; and the real-FUSE
open-handle and errno matrix is not yet a mandatory proven CI job.

## Required wire records

Introduce `FileMutationTransaction`, signed once by the initiating certified
device. Its canonical signed view contains:

- schema and signature-domain versions;
- a random transaction ID, workspace ID, actor, and timestamp;
- operation `ReplacementRename`;
- source and destination parent **stable record IDs**, old and new names, and
  the expected source and replaced record IDs;
- expected pre-operation version IDs for both records;
- the proposed source move version and replaced-record delete version;
- hashes of both complete proposed record projections; and
- the exact participant set, sorted by record ID.

Every participant record receives an identical
`PrepareTransaction { manifest, signature }` envelope followed by a
`CommitTransaction { transaction_id }` envelope. A replica accepts prepare
only when its record is named in the manifest, the embedded version belongs to
that record, the actor/device signature is valid, and its expected parents are
present. Commit is valid only after prepare. A transaction is visible to the
record materializer only when all participants' matching commits are present.

The manifest must authenticate structure rather than only opaque metadata
hashes. This requires signed immutable record-to-node and directory bindings;
raw Automerge `record_id`, parent, and name fields are discovery hints, not
authority. Directory records must therefore be published before transactional
rename is enabled.

## Durable local journal

Add a bounded `RecordTransactionJournal` beneath the record repository root.
Each entry contains the signed manifest, both fully constructed envelope
batches, and one of these monotonic phases:

```text
Prepared
  -> ParticipantRecordsCommitted   (also the history commit: it is derived)
  -> NamespaceCommitted
  -> BroadcastCommitted
  -> removed
```

Each phase transition is an atomic file replacement followed by directory
sync. Recovery always resumes the current phase; it never constructs new
version IDs, signatures, or sequence numbers. Exact envelope replay and an
idempotent derived-history fold are success. A different envelope at a reserved
sequence, a reused transaction ID, or a mismatching record projection is
corruption and fails the mount closed. A journal written by an earlier release
with `history_committed` reopens as `ParticipantRecordsCommitted`, whose next
durable step is the namespace commit.

`RecordRepository::apply_transaction` must validate all candidate replicas
before changing disk. It then writes the journal intent and may replace the two
record files in either order: the journal makes a partial physical replace
recoverable, while replicas and materializers keep prepared transactions
invisible until every commit exists. It publishes all candidate replicas to
memory only after both record files are durable.

`FileHistoryStore` folds both versions against one cloned derived state and
publishes them together. It must never expose one participant without the
other. The durable commit is the single signed-record-log write above; a crash
before it leaves neither participant, and a crash after it leaves both for the
next rebuild.

## Namespace commit and concurrency

All local content and structural mutations must pass through one mutation
coordinator. Replacement rename holds a reservation for the source binding and
destination slot from precondition capture through namespace commit. It must
perform the normal POSIX checks before journal creation:

- source exists and both parent directories exist;
- directory/non-directory replacement compatibility;
- a replaced directory is empty;
- no move into the source directory's descendant;
- `RENAME_NOREPLACE` and other supported flags retain their kernel semantics;
- source and destination are in the caller's writable ACL view.

After the signed participant records (the durable history) commit, the
coordinator rechecks the stable bindings
and expected versions. It applies the two move-tree changes in one Automerge
transaction, installs the already verified source revision, synchronously
persists the namespace (not the current periodic best-effort snapshot), and
only then returns success. The resulting delta is stored in the journal before
broadcast, making rebroadcast after a crash idempotent.

Recovery treats an already-applied namespace result as success. If neither the
pre-state nor exact post-state is present—for example a concurrent remote move
won deterministic CRDT resolution—it records a first-class structural conflict
and leaves the transaction committed but unresolved. It must not overwrite the
new state or invent compensating history.

Remote materialization uses the same complete-participant rule. Seeing only
one participant schedules retrieval of the others and exposes neither path
effect. Authorization is checked independently for every participant; a peer
without access to the replaced record receives no transactional namespace
projection that leaks it.

## Implementation slices

1. **Implemented:** signed directory/node bindings and fail-closed duplicate
   binding checks.
2. **Implemented:** transaction wire types, signature validation, replica
   staging, and participant-completeness indexing.
3. **Implemented:** `RecordRepository::apply_transaction`, a durable journal,
   partial participant-file recovery, and failure injection at repository
   checkpoints.
4. **Implemented:** atomic derived-history fold for both immutable versions;
   the signed record log is the durable history and its query index is rebuilt
   from it.
5. **Partial:** the replacement coordinator synchronously persists namespace
   state and uses the publisher lock, but all mutation sources do not yet use a
   general reservation service and remote conflicts are not first-class.
6. **Implemented slice:** production `QuickFS::rename_named`, repository-wide
   remote materialization, and exact-record protocol reconciliation support
   complete transactions in either participant order and across staged
   restart. Independently delivered remote participants reconstruct their
   exact signed batches into the same durable phase journal; history,
   namespace snapshot replacement, live namespace publication, and broadcast
   then advance monotonically and recover idempotently.
7. **Implemented slice:** real-FUSE replacement tests prove new lookups resolve
   the source while an already-open victim handle keeps its old bytes. Writes
   through that unlinked handle remain private and are discarded at final
   close; signed replacement also proves both histories and the empty journal.
8. **Implemented slice:** a live two-endpoint Iroh record-mesh harness uses
   distinct certified identities, repositories, histories and namespaces and
   exercises both participant orders plus receiver restart using actual
   inventory/pull traffic. A separate live three-endpoint test proves
   exact-version replica acknowledgement, rejection of stale/wrong/aged
   receipts, safe owned-tag GC, owner-store restart and source-loss rehydration
   from the durable replica. Full mount/process isolation, forced partitions,
   authorization asymmetry and concurrent structural-conflict release evidence
   remain mandatory.

## Mandatory tests

- Same-directory and cross-directory file replacement preserve the source
  record ID, tombstone the victim record, and append exactly two linked signed
  versions.
- File-over-directory, directory-over-file, non-empty-directory replacement,
  descendant moves, and `RENAME_NOREPLACE` return the correct errno without a
  journal or signed side effect.
- Failure injection after every journal write, participant record replace,
  derived-history fold, namespace persist, and broadcast recovers to the exact
  post-state after restart. No checkpoint exposes a half-operation.
- Replaying recovery is idempotent and does not advance actor sequences or add
  history nodes.
- A concurrent local edit/move cannot bypass reservations. A concurrent remote
  edit/move deterministically produces either the exact transaction result or
  a structural conflict, never silent overwrite.
- Mesh delivery in both participant orders stages the first and materializes
  only after the second. Restart while staged preserves the same behavior.
- A peer authorized for only one participant learns neither the other record's
  metadata nor its old path.
- Real-FUSE tests verify open file handles continue to address the replaced
  inode while new destination lookups address the source inode, matching POSIX.
