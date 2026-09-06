@0xc7f8a8b3c4d54e21;

# qOS app-host control-socket RPC schema. The host exposes a single
# AppHost capability over a Unix-domain stream socket at
# $XDG_RUNTIME_DIR/qos/services/app-host.sock . The launcher and a
# future App Tray UI call list/resolve/pin/unpin/install/etc. against
# this interface.
#
# All `id` fields carry the raw 32-byte BLAKE3 package id (`pkg::PackageId`).
# The host echoes these unchanged; presentation (`qpkg:<hex>`) is the
# caller's concern.

enum Pinning {
    tried  @0;
    pinned @1;
}

enum FlatpakAction {
    install @0;
    update  @1;
    remove  @2;
}

enum RemoteAction {
    add    @0;
    remove @1;
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
    # Parse + ingest a `.qpkg`-shaped byte blob (a tar archive
    # containing signed-root/manifest/collection/blobs — see the
    # `install.rs` handler for the accepted layout).
    # Returns the resolved 32-byte PackageId.
    # Install only when the bytes match the registry-authenticated identity
    # supplied by the Store capability. None of these fields are advisory.
    install    @6 (qpkgBytes :Data, pinning :Pinning,
                   expectedRoot :Data, expectedPublisher :Data,
                   expectedAppId :Text, expectedVersion :Text,
                   expectedSizeBytes :UInt64) -> (id :Data);
    # Sum of every package's `sizeBytes`, broken down by pinning.
    diskUsage  @7 () -> (totalBytes :UInt64, byPinning :ByPinning);
    # Launch through the host-owned launcher. The app only supplies a package
    # identity already resolved by this service; it cannot add debug bypasses.
    launch     @8 (id :Data) -> (status :Text);
    # Explicit host integration operations. Possession of this interface is a
    # launcher-issued capability restricted to the signed Store app.
    launchDesktop @9 (desktopId :Text) -> (status :Text);
    flatpak    @10 (action :FlatpakAction, appId :Text, reference :Data) -> (status :Text);
    remote     @11 (action :RemoteAction, name :Text, url :Text) -> (status :Text);
}
