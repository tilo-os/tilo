@0xd833c1f47930d736;

# Tilo workspace filesystem — public Cap'n Proto contract.
#
# The FUSE driver in `fs/` mounts a projected filesystem at
# /var/workspace. Apps running inside a launcher-managed sandbox may
# not have permission to touch that path directly (the sandbox
# filesystem view can be restricted per-cluster). This RPC surface
# lets sandboxed apps do file I/O through a proxy that the launcher
# owns, so the launcher can enforce the cluster's file-access policy
# regardless of the app's own view of the tree.
#
# The wire types are intentionally minimal — enough to cover the
# read/write/stat/list paths system apps need. Higher-level
# conveniences (glob, watch) live in the SDK's Rust wrapper, not on
# the wire.

interface WorkspaceFs {
  # Open a file for reading or writing. `path` is a workspace-relative
  # path (e.g. "documents/notes.md"); leading '/' is tolerated. Returns
  # a FileHandle whose lifetime bounds the open file — dropping the
  # capability closes it.
  open @0 (path :Text, mode :OpenMode) -> (handle :FileHandle);

  # List a directory. Returns a snapshot of DirEntry rows. Empty
  # directories return an empty list. Missing directories fail the
  # promise. This is a snapshot, not a stream: for large trees the
  # SDK is expected to paginate at the app layer.
  list @1 (path :Text) -> (entries :List(DirEntry));

  # Stat a single path. Returns Metadata; missing paths fail the
  # promise (mapped to NotAvailable::ConnectionFailed by the SDK for
  # the "not present" case).
  stat @2 (path :Text) -> (metadata :Metadata);

  # Create a directory (with intermediate mkdirs). Idempotent — a
  # pre-existing dir is not an error. Existing non-directory at the
  # target IS an error.
  mkdir @3 (path :Text) -> ();

  # Remove a file (unlink) or an empty directory (rmdir). Recursive
  # deletion is deliberately not exposed on the wire: the launcher
  # can gate large-effect ops behind a prompt.
  remove @4 (path :Text) -> ();

  # Present only when the host independently admitted a native DirectoryGrant.
  # A selected-file backend cannot supply this capability.
  directory @5 () -> (handle :DirectoryHandle);
}

# Access modes. A backend may refuse modes its admitted capability lacks.
enum OpenMode {
  read @0;
  # Truncate + write. Creates the file if missing.
  write @1;
  # Append. Creates the file if missing.
  append @2;
  # Read + positioned write of an existing inode; no create or truncate.
  readWriteExisting @3;
  # Write-only existing inode; never create, truncate, append or grant Read.
  writeExisting @4;
}

# A file handle. Bytes are addressed by explicit offset (no server-
# side seek pointer). The separate append operation is non-idempotent.
interface FileHandle {
  # Read up to `maxLen` bytes starting at `offset`. Returns fewer
  # bytes at EOF; returns an empty blob when offset ≥ length. The
  # server caps `maxLen` at an internal maximum (~1 MiB per read);
  # callers requesting more receive a truncated response.
  read @0 (offset :UInt64, maxLen :UInt32) -> (bytes :Data, status :NativeEntryStatus);

  # Write `bytes` starting at `offset`. Returns the number of bytes
  # written; a successful partial write is permitted. Append-mode handles refuse
  # this positioned operation.
  write @1 (offset :UInt64, bytes :Data) -> (written :UInt32, status :NativeEntryStatus);

  # Persist any buffered writes. Idempotent.
  flush @2 () -> (status :NativeEntryStatus);

  # Stat this open handle. Cheaper than a separate WorkspaceFs.stat
  # for the same path when you already hold the handle.
  metadata @3 () -> (metadata :Metadata, status :NativeEntryStatus);

  # Exactly one atomic append, on an append-mode handle only. A successful
  # partial count is permitted. Lost replies have unknown effects: no retries.
  append @4 (bytes :Data) -> (written :UInt32, status :NativeEntryStatus);
  # Scope-only check. No metadata, byte access, or new authority.
  checkScope @5 () -> ();
}

# A directory row. `name` is the last path component. `metadata`
# is inlined so the caller doesn't have to round-trip for each row.
struct DirEntry {
  name @0 :Text;
  metadata @1 :Metadata;
}

# Path stat. Times are seconds since UNIX epoch; sub-second precision
# isn't exposed (matches the FUSE driver's 10 s TTL — sub-second
# precision here would be misleading).
struct Metadata {
  kind @0 :EntryKind;
  # Total byte length for files; 0 for directories.
  size @1 :UInt64;
  # Last-modified time, seconds since UNIX epoch. 0 if unknown.
  modifiedUnixSeconds @2 :Int64;
  # True when the file is projected but not yet hydrated locally
  # (the FUSE driver knows about it but bytes haven't been fetched).
  # Callers using this signal for progress UI should be aware that
  # a read() call will trigger hydration transparently.
  needsHydration @3 :Bool;
}

enum EntryKind {
  file @0;
  directory @1;
  symlink @2;
  # Reserved for future kinds (socket, fifo, block, char). Apps
  # should treat unknown values as "other".
  other @3;
}


# Raw native components, never lossy Text paths. Each method revalidates the
# underlying actual directory grant. Cursor 0 begins a retained bounded snapshot;
# returned nonzero cursors are opaque single-use continuations on this capability.
interface DirectoryHandle {
  list @0 (cursor :UInt64) -> (entries :List(NativeDirEntry), next :UInt64, status :NativeEntryStatus);
  openDirectory @1 (name :Data) -> (handle :DirectoryHandle, status :NativeEntryStatus);
  openFile @2 (name :Data, mode :OpenMode, expectedDevice :UInt64, expectedInode :UInt64) -> (handle :FileHandle, device :UInt64, inode :UInt64, status :NativeEntryStatus);
  createFile @3 (name :Data) -> (job :NativeJob, status :NativeEntryStatus);
  createDirectory @4 (name :Data) -> (job :NativeJob, status :NativeEntryStatus);
  # Same-parent, never overwrite. Atomic replacement is not provided by this RPC.
  renameNoReplace @5 (name :Data, newName :Data) -> (job :NativeJob, status :NativeEntryStatus);
  # Live metadata for this held directory, and no-follow metadata for one child.
  metadata @6 () -> (metadata :Metadata, device :UInt64, inode :UInt64, fileType :UInt32, status :NativeEntryStatus, executeBits :UInt16);
  stat @7 (name :Data) -> (present :Bool, metadata :Metadata, device :UInt64, inode :UInt64, fileType :UInt32, status :NativeEntryStatus, executeBits :UInt16);
  # Metadata-only held regular inode. No byte methods exist.
  pinMetadata @8 (name :Data, expectedDevice :UInt64, expectedInode :UInt64) -> (handle :MetadataPin, status :NativeEntryStatus);
  # Independent live enumeration; no stable snapshot or total-row limit.
  openCursor @9 () -> (handle :DirectoryCursor, status :NativeEntryStatus);
  # Actual source filesystem capacity observed on this held directory.
  statfs @10 () -> (stats :DirectoryFilesystemStats, status :NativeEntryStatus);
  # Raw held-link observation; does not authorize target traversal or byte IO.
  readlink @11 (name :Data, expectedDevice :UInt64, expectedInode :UInt64) -> (target :Data, status :NativeEntryStatus);
  # Journal IDs/cursor are observations, never grants.
  recoveries @12 (after :Data) -> (entries :List(NativeRecovery), next :Data, done :Bool, status :NativeEntryStatus);
  # Current parent/source grants authorize restoration without byte access.
  restoreCaptured @13 (recovery :Data, name :Data) -> (job :NativeJob, status :NativeEntryStatus);
  # Zero effects; source requires its independently sealed certified verifier.
  prepareReplace @14 (temporary :Data, temporaryDevice :UInt64, temporaryInode :UInt64,
    victim :Data, victimDevice :UInt64, victimInode :UInt64) -> (handle :PendingReplacement, status :NativeEntryStatus);
  # Actual original-credential durability barrier on this held directory.
  syncDirectory @15 () -> (status :NativeEntryStatus);
  retireCreated @16 (name :Data, expectedDevice :UInt64, expectedInode :UInt64) -> (job :NativeJob, status :NativeEntryStatus);
  setCreatedPermissions @17 (name :Data, expectedDevice :UInt64, expectedInode :UInt64, mode :UInt32) -> (status :NativeEntryStatus);
  # Closed by default. Only an enrolled protected replacing-save coordinator.
  renameReplace @18 (name :Data, newName :Data) -> (job :NativeJob, status :NativeEntryStatus);
  # Explicit held-Stage AtomicSave arm. Zero effects, default denied.
  atomicSave @21 (temporary :Data, expectedDevice :UInt64, expectedInode :UInt64, target :Data) -> (status :NativeEntryStatus);
  # Actual original Read custody; observation labels grant no history enrollment.
  historyReadSubject @19 (name :Data) -> (handle :HistoryReadSubject, status :NativeEntryStatus);
  # Existing history xattr vocabulary only; default denied without enrolled Read.
  historyQuery @20 (name :Data, expectedDevice :UInt64, expectedInode :UInt64, query :Data) -> (response :Data, status :NativeEntryStatus);
}
interface MetadataPin {
  metadata @0 () -> (metadata :Metadata, device :UInt64, inode :UInt64, fileType :UInt32, status :NativeEntryStatus, executeBits :UInt16);
  checkScope @1 () -> ();
}
struct NativeDirEntry { name @0 :Data; metadata @1 :Metadata; }
# This is an actual canonical native FsJob. UUIDs identify its durable owner
# records; they confer no authority. Poll drains bounded native updates.
interface NativeJob {
  poll @0 () -> (operationId :Data, updates :List(NativeJobUpdate), status :NativeEntryStatus);
  cancel @1 () -> (status :NativeEntryStatus);
  checkScope @2 () -> ();
  # Completed genuine replacing job only. No history mutation is exposed.
  pinReplacement @3 () -> (handle :ReplacementPin, status :NativeEntryStatus);
  # Actual completed CreateFile custody only, never polled DTO authority.
  createdTemporary @4 () -> (handle :CreatedTemporary, status :NativeEntryStatus);
}
# Holding this capability excludes restoration of its protected victim.
# Observations carry no authority and never imply immutable victim bytes.
# Dropping the final capability releases the native exclusion claim.
interface ReplacementPin {
  commitWithHistory @2 (receipt :Data) -> ();
  observe @0 () -> (observation :ReplacementObservation);
  check @1 () -> ();
}
struct ReplacementObservation {
  operation @0 :Data; sourceOwner @1 :Data; capture @2 :Data;
  sourceDevice @3 :UInt64; sourceInode @4 :UInt64;
  victimDevice @5 :UInt64; victimInode @6 :UInt64;
  destinationDevice @7 :UInt64; destinationInode @8 :UInt64;
}
struct NativeJobUpdate {
  union {
    progress @0 :NativeJobProgress;
    item @1 :NativeJobItem;
    finished @2 :NativeJobFinished;
  }
}
struct NativeJobProgress { sequence @0 :UInt64; item @1 :UInt64; phase @2 :Text; bytes @3 :UInt64; }
struct NativeJobItem {
  item @0 :UInt64; state @1 :NativeItemState; error @2 :Text;
  recoveries @3 :List(Data);
  hasDestination @4 :Bool; destinationDevice @5 :UInt64; destinationInode @6 :UInt64;
}
struct NativeJobFinished { completed @0 :UInt64; failed @1 :UInt64; cancelled @2 :UInt64; }
enum NativeItemState {
  completed @0; failed @1; cancelled @2; copiedSourceRetained @3;
  recoveryRequired @4; outcomeUnknown @5;
}

# Only a still-live producer may emit refused for native operation failure.
# Revocation, authority loss, timeout and malformed replies remain RPC errors.
enum NativeEntryStatus { ok @0; refused @1; }
interface DirectoryCursor {
  page @0 (expectedSequence :UInt64) -> (entries :List(NativeStreamEntry), nextSequence :UInt64, done :Bool, status :NativeEntryStatus);
}
struct NativeStreamEntry {
  name @0 :Data;
  status @1 :NativeEntryStatus;
  metadata @2 :Metadata;
  device @3 :UInt64;
  inode @4 :UInt64;
  fileType @5 :UInt32;
  executeBits @6 :UInt16;
}

# Counts use fragmentSize units. This is metadata, never byte-access authority.
struct DirectoryFilesystemStats {
  blocks @0 :UInt64; blocksFree @1 :UInt64; blocksAvailable @2 :UInt64;
  files @3 :UInt64; filesFree @4 :UInt64; filesAvailable @5 :UInt64;
  blockSize @6 :UInt64; fragmentSize @7 :UInt64; nameMax @8 :UInt64;
  readOnly @9 :Bool;
}

struct NativeRecovery {
  recovery @0 :Data; operation @1 :Data; originalName @2 :Data;
  kind @3 :NativeRecoveryKind;
}
enum NativeRecoveryKind {
  needsRevalidation @0; knownStaged @1; unknownNativeCommit @2;
  publishedSourceRetained @3; quarantinedNative @4;
}

# Actual prepared native Work, never a caller supplied operation ID or receipt.
interface PendingReplacement {
  observe @0 () -> (observation :PendingReplacementObservation);
  # The source verifies the signed receipt against its root-sealed verifier.
  # One shot. An interrupted response is unknown; never replay raw start.
  startWithHistory @1 (receipt :Data) -> (job :NativeJob, status :NativeEntryStatus);
  cancel @2 () -> ();
  checkScope @3 () -> ();
  armDaemonHandoff @4 (challenge :Data) -> (ticket :Data);
  # SAME submitted job, only after native signed history ownership verification.
  acknowledgedCommittedPin @5 () -> (handle :ReplacementPin, status :NativeEntryStatus, job :NativeJob);
  # Original controller only; ACK means exact handoff joined and callbacks drained.
  retireDaemonHandoff @6 (ticket :Data) -> ();
}
enum ReplacementPurpose { replacementRename @0; atomicSave @1; }
struct PendingReplacementObservation {
  operation @0 :Data; sourceOwner @1 :Data;
  sourceDevice @2 :UInt64; sourceInode @3 :UInt64;
  victimDevice @4 :UInt64; victimInode @5 :UInt64;
  intentDigest @6 :Data; archivePlanDigest @7 :Data;
  reservationDescription @8 :Data;
  purpose @9 :ReplacementPurpose;
}

# One-use native custody of this job's own created transient. Retirement captures
# it into recovery; this does not grant Purge or unlink a selected victim.
interface CreatedTemporary {
  checkScope @0 () -> ();
  retire @1 () -> (job :NativeJob, status :NativeEntryStatus);
  prepareReplace @2 () -> (pending :PendingReplacement, status :NativeEntryStatus);
  setPermissions @3 (mode :UInt32) -> ();
  prepareAtomicSave @4 () -> (pending :PendingReplacement, status :NativeEntryStatus);
}

# Source-resident readable regular inode, no byte or mutation methods.
interface HistoryReadSubject {
  observe @0 () -> (observation :HistoryReadObservation);
  checkScope @1 () -> ();
  check @2 () -> ();
  # Route genuine original Read custody, never a record/enrollment grant.
  armDaemonHandoff @3 (challenge :Data) -> (ticket :Data, status :NativeEntryStatus);
  # Original controller only: exact accepted Read realm join and callback drain.
  retireDaemonHandoff @4 (ticket :Data) -> ();
}
struct HistoryReadObservation {
  sourceOwner @0 :Data;
  rootDevice @1 :UInt64; rootInode @2 :UInt64;
  parentDevice @3 :UInt64; parentInode @4 :UInt64;
  parents @5 :List(Data); name @6 :Data;
  device @7 :UInt64; inode @8 :UInt64;
}
