@0xc7f8a8b3c4d54e21;

# qOS app-host control-socket RPC schema. The host exposes a single
# AppHost capability over a Unix-domain stream socket at
# $XDG_RUNTIME_DIR/qos/services/app-host.sock . The launcher and a
# future App Tray UI call list/resolve/pin/unpin/install/etc. against
# this interface.
#
# All `id` fields carry the raw 32-byte BLAKE3 package id (`qos_pkg::PackageId`).
# The host echoes these unchanged; presentation (`qpkg:<hex>`) is the
# caller's concern.

enum Pinning {
    tried  @0;
    pinned @1;
}

struct PackageEntry {
    # 32-byte PackageId (raw, not `qpkg:` prefixed).
    id            @0 :Data;
    pinning       @1 :Pinning;
    # Absolute path of the eagerly-extracted payload directory the
    # launcher can pass as `--payload-root`.
    payloadRoot   @2 :Text;
    # Raw TOML text of the package manifest.
    manifestText  @3 :Text;
    # On-disk size including blob store + payload tree.
    sizeBytes     @4 :UInt64;
    # Unix epoch seconds of the last `markUsed` (or install time).
    lastUsedUnix  @5 :UInt64;
}

struct ByPinning {
    pinnedBytes @0 :UInt64;
    triedBytes  @1 :UInt64;
}

interface AppHost {
    # All currently-known package entries.
    list       @0 () -> (entries :List(PackageEntry));
    # Look up a single entry. Errors if the id is unknown.
    resolve    @1 (id :Data) -> (entry :PackageEntry);
    # Promote Tried → Pinned (idempotent).
    pin        @2 (id :Data) -> ();
    # Demote Pinned → Tried (idempotent).
    unpin      @3 (id :Data) -> ();
    # Forcibly remove if Tried; refuses to remove Pinned packages.
    remove     @4 (id :Data) -> ();
    # LRU touch — updates `lastUsedUnix` to now.
    markUsed   @5 (id :Data) -> ();
    # Parse + ingest a `.qpkg`-shaped byte blob (the wire form is a
    # zero-overhead concatenation of qos-pkg blobs; see install.rs).
    # Returns the resolved 32-byte PackageId.
    install    @6 (qpkgBytes :Data, pinning :Pinning) -> (id :Data);
    # Sum of every package's `sizeBytes`, broken down by pinning.
    diskUsage  @7 () -> (totalBytes :UInt64, byPinning :ByPinning);
}
