@0xc7f8a8b3c4d54e21;

# Tilo app-host control-socket RPC schema. The host exposes a single
# AppHost capability over a Unix-domain stream socket at
# $XDG_RUNTIME_DIR/tilo/services/app-host.sock . The launcher and a
# future App Tray UI call list/resolve/pin/unpin/install/etc. against
# this interface.
#
# All `id` fields carry the raw 32-byte BLAKE3 package id (`tilo_app_package::PackageId`).
# The host echoes these unchanged; presentation (`tapp:<hex>`) is the
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
    enable @2;
    disable @3;
}

# Why the host refused a catalogue read. A refused read returns no rows and
# names the refusal here, with `detail` for logs only: the Store decides what
# to say from the refusal alone, so rewording a detail never changes it.
# Failures that are not one of these are raised as errors.
enum CatalogueRefusal {
    none                  @0;
    # The system's interface declaration cannot be read by this host: the
    # system needs an update before any repository can be judged.
    systemUpdateNeeded    @1;
    # The repository's catalogue is authentic but expired, not renewed.
    expired               @2;
    # The repository did not answer within the read's deadline.
    timedOut              @3;
    # A device-provided Flatpak repository has no OCI summary yet; the
    # device must refresh it.
    deviceRepositoryStale @4;
}

struct PackageEntry {
    # 32-byte PackageId (raw, not `tapp:` prefixed).
    id            @0 :Data;
    pinning       @1 :Pinning;
    # Absolute path of the materialised payload tree the launcher passes
    # as `--payload-root`.
    payloadRoot   @2 :Text;
    # Raw TOML text of the package manifest.
    manifestText  @3 :Text;
    # Bytes of the payload tree.
    sizeBytes     @4 :UInt64;
    # Unix epoch seconds of the last `markUsed` (or install time).
    lastUsedUnix  @5 :UInt64;
    # True when this entry belongs to the per-user mutable package store.
    # False denotes a read-only system package from /usr/share/tilo-apps;
    # system entries may be launched but cannot be pinned or removed here.
    userManaged   @6 :Bool;
    # Absolute path of the signed manifest the launcher reads.
    manifestPath  @7 :Text;
}

struct ByPinning {
    pinnedBytes @0 :UInt64;
    triedBytes  @1 :UInt64;
}

# Connection-owned best-effort progress. One latest value and at most one
# outstanding notification; losing the observer does not roll back installation.
interface RuntimeProgress {
    update @0 (installing :Bool, received :UInt64, total :UInt64, totalKnown :Bool) -> ();
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
    # Install a `.tapp` into the package store: its payload goes into the
    # content store and the tree is materialised from there. A thin
    # `.tapp` installs when the store already holds its payload.
    # Returns the resolved 32-byte PackageId.
    # Install only when the bytes match the registry-authenticated identity
    # supplied by the Store capability. None of these fields are advisory.
    install    @6 (tappBytes :Data, pinning :Pinning,
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
    # The app repositories this person configured, as `app repo list --json`
    # reports them: the one list `app`, `tilo update` and the Store share.
    repositories @12 () -> (json :Text);
    # Fixed read-only host probes. The confined Store never executes host tools.
    flatpakStatus @13 () -> (available :Bool, installed :Text, remotes :Text);
    # Resolve the chosen identity against this device's signed catalogue, then
    # use the shared installer, including thin payload and exact runtime setup.
    installOffer @14 (registry :Text, appId :Text, version :Text,
                      expectedRoot :Data, expectedPublisher :Data,
                      expectedSizeBytes :UInt64) -> (id :Data);
    # Cached AppStream rows from one configured repository, isolated from failures
    # in other remotes. Queries have fixed arguments and bounded output/time.
    flatpakCatalogue @15 (remote :Text, userManaged :Bool)
        -> (rows :Text, refusal :CatalogueRefusal, detail :Text);
    # Install only from an existing repository in this account's Flatpak scope.
    flatpakInstallRemote @16 (remote :Text, appId :Text, branch :Text) -> (status :Text);
    # Verified entries from one enabled host-configured Tilo repository. The
    # host owns durable rollback checkpoints; callers cannot supply keys/URLs.
    registryCatalogue @17 (registry :Text)
        -> (entries :Text, refusal :CatalogueRefusal, detail :Text);

    # Review using this person's existing scoped attestors and revocations.
    # No caller publisher, trust key, host path or download source is accepted.
    reviewLocal @18 (tappBytes :Data) -> (reviewJson :Text);
    # Exact archive/root from review, reverified against current host trust.
    installLocal @19 (tappBytes :Data, reviewedRoot :Text,
                      reviewedArchiveHash :Text) -> (id :Data);
    # Metadata-only exact installed consumer/runtime review. No payload is fetched.
    inspectOptionalRuntime @22 (appId :Text, consumerRoot :Data, runtime :Text)
        -> (reviewJson :Text);
    # Explicit consent to the exact review. Revalidates signed declaration and
    # configured offer identity, then uses the shared bounded signed installer.
    # Disconnect/Back does not roll back or necessarily interrupt installation.
    acquireOptionalRuntime @23 (reviewJson :Text, progress :RuntimeProgress) -> ();

    # The catalogue the host last accepted from one enabled repository, read
    # without the network so a slow or unreachable repository still describes
    # what it listed. The stored snapshot is verified again against the pinned
    # owner key, the rollback checkpoint and its expiry; fails when none does.
    lastVerifiedCatalogue @25 (registry :Text)
        -> (entries :Text, refusal :CatalogueRefusal, detail :Text);
    # Resolve a precise runtime from enabled, pinned repositories, including retained versions.
    runtimeOffer @24 (registry :Text, runtime :Text) -> (offer :Text);
    # Optional-runtime inspection originally used @20, before catalogue lookup
    # independently used it. Neither may dispatch through that ambiguous slot.
    retiredInspectionCatalogueOrdinalCollision @20 () -> ();
    # Two incompatible methods occupied @21: optional-runtime acquisition and
    # runtime-offer lookup. Neither may dispatch through that ambiguous slot.
    retiredRuntimeOrdinalCollision @21 () -> ();
}
