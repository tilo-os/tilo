@0xd833c1f47930d736;

# qOS workspace filesystem — public Cap'n Proto contract.
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
}

# What kind of access to open the file with. `read` and `write` may
# not be combined in v1; upgrade to a bitfield on schema v2 if the
# use case emerges.
enum OpenMode {
  read @0;
  # Truncate + write. Creates the file if missing.
  write @1;
  # Append. Creates the file if missing.
  append @2;
}

# A file handle. Bytes are addressed by explicit offset (no server-
# side seek pointer): the SDK layer tracks position for the caller
# to keep the wire idempotent and the server simple.
interface FileHandle {
  # Read up to `maxLen` bytes starting at `offset`. Returns fewer
  # bytes at EOF; returns an empty blob when offset ≥ length. The
  # server caps `maxLen` at an internal maximum (~1 MiB per read);
  # callers requesting more receive a truncated response.
  read @0 (offset :UInt64, maxLen :UInt32) -> (bytes :Data);

  # Write `bytes` starting at `offset`. Returns the number of bytes
  # written; on a successful call this equals `bytes.size()`.
  write @1 (offset :UInt64, bytes :Data) -> (written :UInt32);

  # Persist any buffered writes. Idempotent.
  flush @2 () -> ();

  # Stat this open handle. Cheaper than a separate WorkspaceFs.stat
  # for the same path when you already hold the handle.
  metadata @3 () -> (metadata :Metadata);
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
