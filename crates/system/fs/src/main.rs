#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod primary_workspace;
mod session_supervisor;
mod workspace_startup;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use collab_types::Principal;
use fs::clock::unix_now_ms;
use fs::full_replica::{
    HomePeerBundle, HomePeerSet, aggregate_status, hydrate_full_workspace,
    prune_obsolete_workspace_content, save_status, validate_home_peer_set,
};
use fs::namespace::ROOT_INO;
use fs::reachability;
use fs::share_refresh::{
    ShareRefresh, ShareRefresher, SharedCredential, SharedFetchError, fetch_shared_snapshot,
};
use fs::sync::{self, SyncHandle};
use fs::workspace_authority::{
    AuthorityRequest, WorkspaceAuthority, WorkspaceAuthorityEnvelope, load_persisted_content_peers,
    local_authority_request, local_authority_socket_path, local_content_socket_path,
    register_local_content_peer, remove_local_content_peers, serve_local_authority,
    serve_local_content_peer,
};
use fs::{
    AclHandle, CapabilityFile, ContentStore, FileHistoryStore, FileShareLinkV2,
    FsTransportIdentity, Heuristics, Hydrator, IdentityKey, Namespace, QuickFS,
    RecipientControlServer, RecipientMountSupervisor, RecordAccessRegistry, RecordMaterializer,
    RecordProtocolClient, RecordProtocolOperationV1, RecordProtocolResponseV1,
    RecordProtocolSnapshotV1, RecordPublisher, RecordRepository, RecordServices,
    RecordSubscriptionFailureCode, RecordSubscriptionKey, RecordSubscriptionStore, ShareAuthority,
    ShareControlServer, ShareManagement, Ticket, Workspace, identity_paths,
    load_certified_mount_identity_once_valid, load_revocation_list, load_workspace_share_issuer,
};
use fuser::{Config, MountOption, SessionACL};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tilo_lock::MutexRecover as _;

#[derive(Parser, Debug)]
#[command(author, version, about = "Tilo collaborative filesystem", long_about = None)]
struct Cli {
    /// Deterministic owner-only clock used by release tests. The file contains
    /// canonical decimal Unix milliseconds and is re-read for every decision.
    #[arg(long, global = true, hide = true, value_name = "ABSOLUTE_PATH")]
    test_clock_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[cfg(test)]
mod share_mount_tests {
    use super::*;
    use collab_types::Ed25519PublicKey;
    use fs_protocol::{
        ContentHash, FileActor, FileMetadataV1, FileProvenance, FileRecordId, FileVersionId,
        FileVersionV1, IndexStatus, PreviewStatus, PreviewVariant,
    };

    fn actor() -> FileActor {
        FileActor {
            person: Ed25519PublicKey([0x11; 32]),
            device: Ed25519PublicKey([0x22; 32]),
        }
    }

    #[test]
    fn share_mounts_take_secret_free_bounded_input_and_durable_generations() {
        {
            let cli = Cli::try_parse_from([
                "fs",
                "mount-share",
                "--mount-point",
                "/var/workspace/shared",
                "--link-stdin",
                "--cache-dir",
                "/var/cache/shared",
            ])
            .unwrap();
            assert!(matches!(
                cli.command,
                Command::MountShare {
                    link: None,
                    link_stdin: true,
                    ..
                }
            ));
        }
        {
            assert_eq!(
                parse_share_uri_input(b"tilo-file://v2/example#secret\n".to_vec(), 64).unwrap(),
                "tilo-file://v2/example#secret"
            );
            assert!(parse_share_uri_input(vec![b'x'; 65], 64).is_err());
            assert!(parse_share_uri_input(b"\r\n".to_vec(), 64).is_err());
            assert!(parse_share_uri_input(vec![0xff], 64).is_err());
        }
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("namespace.bin");
            let namespace = Arc::new(Mutex::new(Namespace::new("replica-checkpoint").unwrap()));
            let written = persist_namespace_generation(&namespace, &path).unwrap();
            assert_eq!(
                written,
                *blake3::hash(&std::fs::read(&path).unwrap()).as_bytes(),
                "the reported generation must be the exact durable primary"
            );
            let mut reopened = load_namespace(&path, "replica-restart".into()).unwrap();
            assert_eq!(
                reopened.save(),
                namespace.lock().unwrap().save(),
                "a restart reopens the generation that was checkpointed"
            );
        }
        {
            let cli = Cli::try_parse_from([
                "fs",
                "recipient-service",
                "--socket",
                "/run/user/1000/tilo/services/tilo-fs.sock",
                "--mounts-root",
                "/home/alice/Shared",
                "--cache-root",
                "/home/alice/.cache/tilo/received-files",
            ])
            .unwrap();
            assert!(matches!(cli.command, Command::RecipientService { .. }));
        }
    }

    /// Every `fsync` asks for the namespace to be durable, so asking costs a
    /// write only when the document changed since this process last wrote
    /// it, by any path: the barrier itself or the full generation write.
    #[test]
    fn the_namespace_barrier_writes_only_a_changed_document() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("namespace.bin");
        let namespace = Arc::new(Mutex::new(Namespace::new("barrier").unwrap()));
        persist_namespace_changes(&namespace, &path).unwrap();
        assert!(path.exists(), "the first barrier writes the document");

        std::fs::remove_file(&path).unwrap();
        persist_namespace_changes(&namespace, &path).unwrap();
        assert!(!path.exists(), "an unchanged document is not written again");

        namespace
            .lock()
            .unwrap()
            .create_dir(fs::ROOT_INO, "placed", 1)
            .unwrap();
        persist_namespace_changes(&namespace, &path).unwrap();
        let mut reopened = load_namespace(&path, "restarted".into()).unwrap();
        assert!(
            reopened.lookup(fs::ROOT_INO, "placed").is_some(),
            "a changed document is written before the barrier returns"
        );

        namespace
            .lock()
            .unwrap()
            .create_dir(fs::ROOT_INO, "checkpointed", 2)
            .unwrap();
        persist_namespace_generation(&namespace, &path).unwrap();
        std::fs::remove_file(&path).unwrap();
        persist_namespace_changes(&namespace, &path).unwrap();
        assert!(
            !path.exists(),
            "a generation written another way already satisfies the barrier"
        );
    }

    /// #4113. The persister writes the sync progress with each peer
    /// beside the namespace document, and a restart resumes it; damaged
    /// progress, or progress beside a document recovered from an older
    /// generation, is ignored, so every exchange starts with a full sync.
    #[tokio::test(start_paused = true)]
    async fn sync_progress_persists_beside_the_namespace_and_is_never_trusted() {
        const PEER: [u8; 32] = [9; 32];
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("namespace.bin");
        let progress = peer_sync_path(&path);
        let namespace = Arc::new(Mutex::new(Namespace::new("persisted").unwrap()));
        let agree_on_heads = |namespace: &Arc<Mutex<Namespace>>| {
            let mut namespace = namespace.lock().unwrap();
            let mut state = automerge::sync::State::new();
            state.shared_heads = namespace.heads();
            namespace.finish_sync_with(PEER, &state);
        };
        agree_on_heads(&namespace);
        let persister = spawn_persist(Arc::clone(&namespace), path.clone());
        tokio::time::sleep(Duration::from_secs(5)).await;
        persister.abort();
        assert!(progress.exists(), "the persister wrote the sync progress");
        assert_eq!(
            std::fs::metadata(&progress).unwrap().permissions(),
            std::fs::metadata(&path).unwrap().permissions(),
            "the progress has the document's permissions"
        );
        let mut reopened = load_namespace(&path, "persisted".into()).unwrap();
        assert!(
            reopened.in_sync_with(&PEER),
            "a restart resumes the progress"
        );

        let mut damaged = std::fs::read(&progress).unwrap();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        std::fs::write(&progress, &damaged).unwrap();
        let mut reopened = load_namespace(&path, "persisted".into()).unwrap();
        assert!(
            !reopened.in_sync_with(&PEER),
            "damaged progress was resumed"
        );

        persist_peer_sync_states(&namespace, &progress);
        namespace
            .lock()
            .unwrap()
            .create_dir(fs::ROOT_INO, "newer", 1)
            .unwrap();
        persist_namespace_generation(&namespace, &path).unwrap();
        std::fs::write(&path, b"damaged primary").unwrap();
        let mut recovered = load_namespace(&path, "persisted".into()).unwrap();
        assert!(
            recovered.lookup(fs::ROOT_INO, "newer").is_none(),
            "the older generation was recovered"
        );
        assert!(
            !recovered.in_sync_with(&PEER),
            "a recovered document resumed progress"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recipient_service_replaces_only_a_verified_stale_socket() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tilo-fs.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let listener = bind_recipient_service_socket(&socket).unwrap();
        drop(listener);

        std::fs::remove_file(&socket).unwrap();
        std::fs::write(&socket, b"not a socket").unwrap();
        assert!(bind_recipient_service_socket(&socket).is_err());
    }

    fn version(
        record_id: FileRecordId,
        id: FileVersionId,
        parents: Vec<FileVersionId>,
        hash: u8,
    ) -> FileVersionV1 {
        FileVersionV1 {
            id,
            record_id,
            parents,
            actor: actor(),
            timestamp_ms: 10,
            content_hash: Some(ContentHash([hash; 32])),
            metadata_hash: ContentHash([hash.wrapping_add(1); 32]),
            provenance: FileProvenance::Edit,
        }
    }

    fn snapshot(record_id: FileRecordId) -> RecordProtocolSnapshotV1 {
        let parent = FileVersionId::new();
        let child = FileVersionId::new();
        RecordProtocolSnapshotV1 {
            record_id,
            binding: None,
            metadata: Some(FileMetadataV1 {
                schema_version: 1,
                record_id,
                content_hash: Some(ContentHash([0x33; 32])),
                byte_len: 123,
                mime: Some("image/png".into()),
                previews: vec![PreviewVariant {
                    max_edge: 512,
                    hash: Some(ContentHash([0x44; 32])),
                    status: PreviewStatus::Ready,
                }],
                latest_actor: Some(actor()),
                observed_at_ms: 42,
                index_status: IndexStatus::Indexed,
                properties: None,
            }),
            content_revision: Some(fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id,
                version_id: child,
                content_hash: Some(ContentHash([0x33; 32])),
                byte_len: 123,
                metadata_hash: ContentHash([0x34; 32]),
            }),
            // Deliberately child-first: admission must topologically stage it.
            versions: vec![
                version(record_id, child, vec![parent], 0x33),
                version(record_id, parent, Vec::new(), 0x22),
            ],
            conflicts: Vec::new(),
        }
    }

    #[test]
    fn shared_snapshots_project_exactly_one_record() {
        {
            let dir = tempfile::tempdir().unwrap();
            let history = FileHistoryStore::open(dir.path().join("history.json")).unwrap();
            let namespace = Arc::new(Mutex::new(Namespace::new("recipient").unwrap()));
            let record_id = FileRecordId::new();
            let first = snapshot(record_id);

            apply_shared_snapshot(&namespace, &history, &first).unwrap();
            let mut guard = namespace.lock().unwrap();
            validate_shared_namespace(&mut guard, record_id, false).unwrap();
            let entries: Vec<_> = guard
                .readdir(ROOT_INO)
                .unwrap()
                .into_iter()
                .filter(|(_, _, name)| name != "." && name != "..")
                .collect();
            assert_eq!(entries.len(), 1);
            let ino = entries[0].0;
            assert_eq!(guard.record_id_for(ino), Some(record_id));
            assert_eq!(guard.getattr(ino).unwrap().size, 123);
            assert_eq!(
                guard.thumb_hash(ino),
                Some(iroh_blobs::Hash::from([0x44; 32]))
            );
            drop(guard);
            assert_eq!(history.snapshot(record_id).unwrap().versions.len(), 2);

            let mut second = first;
            let metadata = second.metadata.as_mut().unwrap();
            metadata.content_hash = Some(ContentHash([0x55; 32]));
            metadata.byte_len = 456;
            metadata.observed_at_ms = 43;
            let prior = second.content_revision.unwrap().version_id;
            let next = FileVersionId::new();
            second
                .versions
                .push(version(record_id, next, vec![prior], 0x55));
            second.content_revision = Some(fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id,
                version_id: next,
                content_hash: Some(ContentHash([0x55; 32])),
                byte_len: 456,
                metadata_hash: ContentHash([0x56; 32]),
            });
            apply_shared_snapshot(&namespace, &history, &second).unwrap();
            let guard = namespace.lock_recover();
            assert_eq!(guard.ino_for_record_id(record_id), Some(ino));
            assert_eq!(guard.getattr(ino).unwrap().size, 456);
            drop(guard);
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let history = FileHistoryStore::open(dir.path().join("history.json")).unwrap();
            let namespace = Arc::new(Mutex::new(Namespace::new("recipient").unwrap()));
            let record_id = FileRecordId::new();
            let mut wrong_metadata = snapshot(record_id);
            wrong_metadata.metadata.as_mut().unwrap().record_id = FileRecordId::new();
            assert!(apply_shared_snapshot(&namespace, &history, &wrong_metadata).is_err());

            let mut wrong_history = snapshot(record_id);
            wrong_history.versions[0].record_id = FileRecordId::new();
            assert!(apply_shared_snapshot(&namespace, &history, &wrong_history).is_err());
        }
        {
            let expected = FileRecordId::new();
            let mut namespace = Namespace::new("recipient").unwrap();
            namespace
                .create_projected_file(ROOT_INO, "shared.txt", expected, None, 0, 0)
                .unwrap();
            namespace
                .create_file(ROOT_INO, "unrelated.txt", None, 0, 0)
                .unwrap();
            assert!(validate_shared_namespace(&mut namespace, expected, false).is_err());
        }
    }
}

/// How the mount enforces per-identity file access.
#[derive(Args, Debug, Clone, Default)]
struct AclArgs {
    /// Enforce this capability list (JSON array of `collab_types::CapOverride`); a load failure aborts the mount.
    #[arg(long, value_name = "FILE")]
    acl: Option<PathBuf>,
    /// The identity this mount acts as: 64 hex chars (Ed25519 public key). Defaults to the certified human root.
    #[arg(long, value_name = "HEX")]
    identity: Option<String>,
    /// A principal this mount can prove beyond its own person id (`room:<id>`, `app:<id>`, `agent:<id>`, `token:<id>`, `person:<id>`); repeatable.
    #[arg(long = "principal", value_name = "KIND:ID")]
    principals: Vec<String>,
    /// `0` reads `--acl` once, at mount time. Otherwise it is re-read on
    /// every change, and every this many seconds only when the file cannot
    /// be watched (default 1).
    #[arg(long, value_name = "SECONDS", default_value_t = 1)]
    acl_reload_secs: u64,
}

/// Which local data plane serves the workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum DataPlaneChoice {
    /// The native tree (Tier 1) when the store volume and kernel support
    /// it, else FUSE.
    #[default]
    Auto,
    /// Always FUSE (Tier 2/3).
    Fuse,
    /// The native tree or fail.
    Native,
}

/// Where and how the Tier 1 data plane runs.
#[derive(Args, Debug, Clone)]
struct DataPlaneArgs {
    /// Local data plane: `auto` serves the native tree when the store volume and kernel support it.
    #[arg(long, value_enum, default_value_t = DataPlaneChoice::Auto)]
    data_plane: DataPlaneChoice,
    /// Root of the dedicated Btrfs store volume holding the native room trees.
    #[arg(long, value_name = "DIR", default_value = "/var/lib/tilo/store")]
    store: PathBuf,
    /// Owner (`UID:GID`) of files the native data plane materialises; defaults to the room directory's owner.
    #[arg(long, value_name = "UID:GID")]
    native_owner: Option<String>,
    /// Remote files up to this many bytes are fetched before they appear; larger ones start as placeholders.
    #[arg(long, value_name = "BYTES", default_value_t = 1024 * 1024)]
    native_eager_bytes: u64,
    /// Arbitrate the workspace's write leases: grant a free lease to one device at a time (the always-on host).
    #[arg(long)]
    lease_authority: bool,
}

impl Default for DataPlaneArgs {
    fn default() -> Self {
        Self {
            data_plane: DataPlaneChoice::Fuse,
            store: PathBuf::from("/var/lib/tilo/store"),
            native_owner: None,
            native_eager_bytes: 1024 * 1024,
            lease_authority: false,
        }
    }
}

fn parse_owner(spec: &str) -> Result<(u32, u32)> {
    let (uid, gid) = spec
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("--native-owner must be UID:GID, got {spec:?}"))?;
    Ok((
        uid.parse().context("--native-owner UID")?,
        gid.parse().context("--native-owner GID")?,
    ))
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Inspect saved workspace bindings without starting transport or exposing tickets.
    WorkspaceStatus {
        #[arg(long)]
        cache_dir: PathBuf,
    },
    /// Start `serve` for the saved primary Files workspace of a host desktop
    /// session, after its identity and permission check; exits 78 when not ready.
    ServePrimary(primary_workspace::PrimaryArgs),
    /// Host a collaborative workspace and mount it locally.
    Serve {
        /// Refuse a fresh namespace; resume saved state or an authenticated discovered workspace only.
        #[arg(long)]
        require_existing_workspace: bool,
        /// Where to mount the workspace.
        #[arg(short, long)]
        mount_point: PathBuf,
        /// Persistent content cache directory (the iroh-blobs store root).
        #[arg(short, long)]
        cache_dir: Option<PathBuf>,
        /// Seed the workspace from this directory's current contents.
        #[arg(short, long)]
        seed: Option<PathBuf>,
        /// Serve the document control socket here.
        #[arg(long)]
        control_sock: Option<PathBuf>,
        /// Serve typed hydration accounting and safe eviction here.
        #[arg(long)]
        storage_sock: Option<PathBuf>,
        /// Serve the scoped document-media capability here.
        #[arg(long)]
        blob_sock: Option<PathBuf>,
        #[command(flatten)]
        acl: AclArgs,
        #[command(flatten)]
        data_plane: DataPlaneArgs,
    },
    /// Join a workspace shared by a peer (via its ticket) and mount it.
    Mount {
        /// Where to mount the workspace.
        #[arg(short, long)]
        mount_point: PathBuf,
        /// The ticket printed by the hosting peer.
        #[arg(short, long)]
        ticket: String,
        /// Persistent content cache directory (the iroh-blobs store root).
        #[arg(short, long)]
        cache_dir: Option<PathBuf>,
        /// Serve the document control socket here.
        #[arg(long)]
        control_sock: Option<PathBuf>,
        /// Serve typed hydration accounting and safe eviction here.
        #[arg(long)]
        storage_sock: Option<PathBuf>,
        /// Serve the scoped document-media capability here.
        #[arg(long)]
        blob_sock: Option<PathBuf>,
        #[command(flatten)]
        acl: AclArgs,
        #[command(flatten)]
        data_plane: DataPlaneArgs,
    },
    /// Mount the single exact record addressed by an endpoint-bound share link.
    MountShare {
        /// Where to mount the read-only shared record.
        #[arg(short, long)]
        mount_point: PathBuf,
        /// V2 tilo-file URI. The fragment contains a bearer secret; prefer
        /// launching this through Files rather than shell history.
        #[arg(
            short,
            long,
            required_unless_present = "link_stdin",
            conflicts_with = "link_stdin"
        )]
        link: Option<String>,
        /// Read the bearer URI from standard input, keeping it out of process
        /// arguments and shell history. Input is strictly size-bounded.
        #[arg(long)]
        link_stdin: bool,
        /// Persistent recipient cache and subscription state.
        #[arg(short, long)]
        cache_dir: PathBuf,
    },
    /// Mount one named-recipient share delivered through the authenticated inbox.
    MountNamedShare {
        #[arg(short, long)]
        mount_point: PathBuf,
        /// Canonical hex envelope read from stdin; never accepted in argv.
        #[arg(long)]
        envelope_stdin: bool,
        #[arg(short, long)]
        cache_dir: PathBuf,
    },
    /// Run the per-user recipient-share service used by sandboxed Files.
    RecipientService {
        /// Launcher service socket (`%t/tilo/services/tilo-fs.sock`).
        #[arg(long)]
        socket: PathBuf,
        /// Private root containing isolated read-only received mounts.
        #[arg(long)]
        mounts_root: PathBuf,
        /// Private root containing subscriptions and per-record caches.
        #[arg(long)]
        cache_root: PathBuf,
        /// Filesystem executable used for supervised `mount-share` children.
        #[arg(long)]
        fs_binary: Option<PathBuf>,
    },
    /// Run the per-user cloud account service: one mount per enabled
    /// cloud storage account under `--mounts-root` (`~/Remote`).
    #[cfg(target_os = "linux")]
    CloudService {
        /// Where account folders appear.
        #[arg(long)]
        mounts_root: PathBuf,
        /// Private per-account metadata, queues and content caches.
        #[arg(long)]
        state_root: PathBuf,
        /// Settings' config root; defaults to `$XDG_CONFIG_HOME/tilo`.
        #[arg(long)]
        config_root: Option<PathBuf>,
        /// Filesystem executable used for supervised `mount-cloud` children.
        #[arg(long)]
        fs_binary: Option<PathBuf>,
        /// Where to serve account control (list, sync now, pause, resume;
        /// no file requests); defaults to
        /// `$XDG_RUNTIME_DIR/tilo-fs-cloud/control.sock`.
        #[arg(long)]
        control_socket: Option<PathBuf>,
        /// Where to serve file requests (revisions, restore, share links)
        /// as well as account control; defaults to
        /// `$XDG_RUNTIME_DIR/tilo-fs-cloud-files/files.sock`. Keep it out
        /// of `--control-socket`'s directory.
        #[arg(long)]
        file_socket: Option<PathBuf>,
    },
    /// Serve one cloud account at `--mount-point`. The mount specification
    /// and credential arrive on stdin from `cloud-service`, never in argv.
    #[cfg(target_os = "linux")]
    MountCloud {
        #[arg(long)]
        mount_point: PathBuf,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Keep a complete, authenticated workspace replica without mounting FUSE.
    Replica {
        /// A home-peer bundle exported by an existing workspace member.
        #[arg(long)]
        config: PathBuf,
    },
    /// Export the one-file home-peer bundle for an always-on Tilo host.
    ExportHomePeer {
        /// Cache directory of the workspace currently served by this device.
        #[arg(long)]
        cache_dir: PathBuf,
        /// Existing capability list which the home peer must enforce.
        #[arg(long)]
        acl: PathBuf,
        /// Destination bundle to transfer to the home peer.
        #[arg(long)]
        output: PathBuf,
        /// Persistent cache directory on the home peer.
        #[arg(long, default_value = "/var/lib/tilo/full-replica")]
        replica_cache_dir: PathBuf,
        /// Atomic readiness proof read by tilo-relay.
        #[arg(long, default_value = "/var/lib/tilo/full-replica/status.json")]
        status_file: PathBuf,
        /// Additional capability principals held by the host identity.
        #[arg(long = "principal")]
        principals: Vec<String>,
        /// Relay workspace shard backed by this filesystem workspace.
        #[arg(long = "relay-workspace-id")]
        relay_workspace_ids: Vec<u64>,
    },
    /// Sign and atomically publish the current workspace capability policy.
    /// Exits with status 77 on a device that holds no governor key.
    PublishAuthority {
        #[arg(long)]
        acl: PathBuf,
    },
    /// Register a direct filesystem endpoint obtained from authenticated relay
    /// discovery with the running local filesystem daemon.
    RegisterContentPeer {
        #[arg(long)]
        cache_dir: PathBuf,
        /// Read one JSON-serialized `EndpointAddr` from standard input.
        #[arg(long)]
        endpoint_stdin: bool,
    },
    /// Remove direct filesystem endpoints previously nominated by an enrolled
    /// home-peer relay. This goes through the daemon's authenticated local
    /// control socket; callers never edit its durable catalog directly.
    UnregisterContentPeer {
        #[arg(long)]
        cache_dir: PathBuf,
        /// Hex-encoded relay service `NodeId` from the removed home-peer record.
        #[arg(long)]
        relay_service_id: String,
    },
    /// Ask the origins of this device's public catalogue copies that are due
    /// whether they changed, through the running daemon.
    RefreshPublicContent {
        /// The daemon's document-media socket.
        #[arg(long, default_value = "/run/tilo/fs-blobs.sock")]
        blob_sock: PathBuf,
    },
}

/// Exit status of `publish-authority` when this device holds no governor key:
/// only the person's primary device signs the workspace's authority, so a
/// joined device has nothing to publish and its desktop stops retrying.
const PUBLISH_AUTHORITY_NOT_GOVERNOR_EXIT: i32 = 77;

/// `publish-authority` found no governor key on this device.
#[derive(Debug)]
struct NotGovernor(std::io::Error);

impl std::fmt::Display for NotGovernor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "this device holds no workspace governor key ({}); only the primary device publishes \
             workspace authority",
            self.0
        )
    }
}

impl std::error::Error for NotGovernor {}

fn main() -> Result<()> {
    // Before the runtime starts any thread: this executable also serves as
    // the Automerge worker the conflict merge decodes peer versions in.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    let cli = Cli::parse();
    if let Command::ServePrimary(args) = &cli.command {
        std::process::exit(primary_workspace::run(args));
    }
    // FUSE callbacks poll their futures on the fuser threads via the
    // runtime handle; the workers only serve iroh, sockets and timers.
    let result = tilo_rt::daemon("tilo-fs").run(run_command(cli))?;
    if let Err(error) = &result
        && error.downcast_ref::<NotGovernor>().is_some()
    {
        eprintln!("{error}");
        std::process::exit(PUBLISH_AUTHORITY_NOT_GOVERNOR_EXIT);
    }
    result
}

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch over every subcommand the daemon offers"
)]
async fn run_command(cli: Cli) -> Result<()> {
    if let Some(path) = &cli.test_clock_file {
        fs::clock::install_test_clock_file(path).context("configuring deterministic test clock")?;
    }
    match cli.command {
        Command::WorkspaceStatus { cache_dir } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&workspace_startup::inspect(&cache_dir)?)?
            );
            Ok(())
        }
        Command::ServePrimary(_) => bail!("serve-primary runs before the runtime starts"),
        Command::Serve {
            require_existing_workspace,
            mount_point,
            cache_dir,
            seed,
            control_sock,
            storage_sock,
            blob_sock,
            acl,
            data_plane,
        } => {
            if require_existing_workspace {
                let cache = cache_dir
                    .as_deref()
                    .context("recovery requires --cache-dir")?;
                workspace_startup::inspect(cache)?.require_existing()?;
            }
            // Boxed: serving holds every service's state across one await.
            Box::pin(serve(
                mount_point,
                cache_dir,
                seed,
                control_sock,
                storage_sock,
                blob_sock,
                acl,
                data_plane,
            ))
            .await
        }
        Command::Mount {
            mount_point,
            ticket,
            cache_dir,
            control_sock,
            storage_sock,
            blob_sock,
            acl,
            data_plane,
        } => {
            mount(
                mount_point,
                ticket,
                cache_dir,
                control_sock,
                storage_sock,
                blob_sock,
                acl,
                data_plane,
            )
            .await
        }
        Command::MountShare {
            mount_point,
            link,
            link_stdin,
            cache_dir,
        } => mount_bearer_share(mount_point, link, link_stdin, cache_dir).await,
        Command::MountNamedShare {
            mount_point,
            envelope_stdin,
            cache_dir,
        } => mount_named_share(mount_point, envelope_stdin, cache_dir).await,
        Command::RecipientService {
            socket,
            mounts_root,
            cache_root,
            fs_binary,
        } => match recipient_service(socket, mounts_root, cache_root, fs_binary).await {
            Ok(()) => Ok(()),
            Err(error) => {
                // Keep the activation socket. Restart=on-failure plus a
                // missing identity, inbox bind, or cache root is a loop;
                // P03 saw tilo-fs-recipient hit start-limit on a fresh boot.
                eprintln!("recipient-service idle: {error}");
                std::future::pending::<()>().await;
                Ok(())
            }
        },
        #[cfg(target_os = "linux")]
        Command::CloudService {
            mounts_root,
            state_root,
            config_root,
            fs_binary,
            control_socket,
            file_socket,
        } => {
            let control_socket = control_socket.or_else(runtime_fs::cloud::default_socket);
            let file_socket = file_socket.or_else(runtime_fs::cloud::default_files_socket);
            let executable = match fs_binary {
                Some(path) => path,
                None => std::env::current_exe()?,
            };
            let result = tokio::task::spawn_blocking(move || {
                fs::cloud::CloudService::new(
                    fs::cloud::ServiceConfig {
                        // Settings' credential helper, the only process that
                        // opens the account vault, publishes fresh handoff
                        // credentials.
                        refresher: std::sync::Arc::new(accounts::CommandRefresher::new(
                            config_root.clone(),
                        )),
                        config_root,
                        mounts_root,
                        state_root,
                        control_socket,
                        file_socket,
                    },
                    Box::new(fs::cloud::ProcessLauncher {
                        executable,
                        ready_timeout: std::time::Duration::from_secs(30),
                    }),
                )
                .run()
            })
            .await?;
            if let Err(error) = result {
                eprintln!("cloud-service idle: {error}");
                std::future::pending::<()>().await;
            }
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Command::MountCloud {
            mount_point,
            state_dir,
        } => {
            tokio::task::spawn_blocking(move || {
                fs::cloud::service::run_mount(&mount_point, &state_dir)
            })
            .await??;
            Ok(())
        }
        Command::Replica { config } => replica(config).await,
        Command::ExportHomePeer {
            cache_dir,
            acl,
            output,
            replica_cache_dir,
            status_file,
            principals,
            relay_workspace_ids,
        } => export_home_peer(
            &cache_dir,
            &acl,
            &output,
            &replica_cache_dir,
            status_file,
            principals,
            relay_workspace_ids,
        ),
        Command::PublishAuthority { acl } => publish_authority(acl).await,
        Command::RegisterContentPeer {
            cache_dir,
            endpoint_stdin,
        } => register_content_peer(cache_dir, endpoint_stdin).await,
        Command::UnregisterContentPeer {
            cache_dir,
            relay_service_id,
        } => unregister_content_peer(cache_dir, &relay_service_id).await,
        Command::RefreshPublicContent { blob_sock } => {
            let changed = tokio::task::spawn_blocking(move || {
                runtime_fs::blobs::BlobClient::connect_session(&blob_sock)?.refresh_public()
            })
            .await??;
            println!("{changed} public copies changed");
            Ok(())
        }
    }
}

/// Sign `acl` as the workspace governor and hand it to the local daemon.
/// Runs in the person's session: it reads only the person's governor key and
/// the group-reachable socket directory, never the daemon's root-only cache.
async fn publish_authority(acl: PathBuf) -> Result<()> {
    let paths = identity_paths();
    let governor = match tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey) {
        Ok(governor) => governor,
        Err(tilo_user_identity::persist::PersistError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Err(NotGovernor(error).into());
        }
        Err(error) => return Err(error).context("read workspace governor key"),
    };
    let capabilities: collab_types::FsAuthority = serde_json::from_slice(
        &std::fs::read(&acl).with_context(|| format!("read capability list {}", acl.display()))?,
    )?;
    let (workspace, socket) = fs::workspace_authority::find_local_authority().await?;
    let current = local_authority_request(
        &socket,
        AuthorityRequest {
            workspace,
            publish: None,
        },
    )
    .await?
    .authority;
    let revision = current.map_or(1, |current| current.revision.saturating_add(1));
    let envelope =
        WorkspaceAuthorityEnvelope::sign(workspace, revision, capabilities, false, &governor)?;
    let accepted = local_authority_request(
        &socket,
        AuthorityRequest {
            workspace,
            publish: Some(envelope.clone()),
        },
    )
    .await?
    .authority;
    anyhow::ensure!(
        accepted.as_ref() == Some(&envelope),
        "workspace authority publication lost a concurrent revision; retry"
    );
    Ok(())
}

async fn mount_bearer_share(
    mount_point: PathBuf,
    link: Option<String>,
    link_stdin: bool,
    cache_dir: PathBuf,
) -> Result<()> {
    Box::pin(mount_share(
        mount_point,
        SharedCredential::Bearer(FileShareLinkV2::decode_uri(&read_share_uri(
            link, link_stdin,
        )?)?),
        cache_dir,
    ))
    .await
}

async fn mount_named_share(
    mount_point: PathBuf,
    envelope_stdin: bool,
    cache_dir: PathBuf,
) -> Result<()> {
    anyhow::ensure!(envelope_stdin, "named-share envelope stdin is required");
    let encoded = read_bounded_stdin(fs::named_share_delivery::MAX_NAMED_SHARE_ENVELOPE_BYTES * 2)?;
    let bytes = hex::decode(encoded).context("named-share envelope is not canonical hex")?;
    let envelope: fs::NamedShareEnvelopeV1 = collab_types::canonical::from_canonical_bytes(&bytes)?;
    Box::pin(mount_share(
        mount_point,
        SharedCredential::Named(envelope),
        cache_dir,
    ))
    .await
}

async fn register_content_peer(cache_dir: PathBuf, endpoint_stdin: bool) -> Result<()> {
    anyhow::ensure!(endpoint_stdin, "content endpoint must be supplied on stdin");
    let mut encoded = Vec::new();
    std::io::stdin().take(64 * 1024).read_to_end(&mut encoded)?;
    anyhow::ensure!(
        !encoded.is_empty() && encoded.len() < 64 * 1024,
        "content endpoint input is empty or too large"
    );
    let registration =
        serde_json::from_slice::<fs::workspace_authority::ContentPeerRequest>(&encoded)
            .or_else(|_| {
                // The original CLI accepted a raw EndpointAddr. Preserve that
                // wire contract while allowing relay discovery to carry the NAS
                // ticket needed by a fresh stock `fs serve` daemon.
                serde_json::from_slice::<iroh_base::EndpointAddr>(&encoded).map(|endpoint| {
                    fs::workspace_authority::ContentPeerRequest {
                        endpoint,
                        ticket: None,
                        relay_service_id: None,
                    }
                })
            })
            .context("parse discovered content endpoint registration")?;
    let ticket = Ticket::decode(&std::fs::read_to_string(workspace_ticket_path(&cache_dir))?)?;
    let response = register_local_content_peer(
        &local_content_socket_path(ticket.workspace_id()),
        registration.endpoint,
        registration.ticket,
        registration.relay_service_id,
    )
    .await?;
    if response.rebind_required && !response.restart_scheduled {
        bail!(
            "content peer was saved, but this ad-hoc fs daemon must be restarted to join its workspace"
        );
    }
    Ok(())
}

async fn unregister_content_peer(cache_dir: PathBuf, relay_service_id: &str) -> Result<()> {
    let decoded = hex::decode(relay_service_id).context("decode relay service id")?;
    let relay_service_id: [u8; 32] = decoded
        .try_into()
        .map_err(|_: Vec<u8>| anyhow::anyhow!("relay service id must be 32 bytes"))?;
    let ticket = Ticket::decode(&std::fs::read_to_string(workspace_ticket_path(&cache_dir))?)?;
    remove_local_content_peers(
        &local_content_socket_path(ticket.workspace_id()),
        relay_service_id,
    )
    .await?;
    Ok(())
}

fn read_share_uri(link: Option<String>, link_stdin: bool) -> Result<String> {
    use std::io::Read;

    if let Some(link) = link {
        return Ok(link);
    }
    if !link_stdin {
        bail!("a share link is required");
    }
    let maximum = fs::MAX_RECORD_SUBSCRIPTION_URI_BYTES;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(u64::try_from(maximum.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    parse_share_uri_input(bytes, maximum)
}

fn read_bounded_stdin(maximum: usize) -> Result<String> {
    use std::io::Read;

    let mut bytes = Vec::new();
    std::io::stdin()
        .take(u64::try_from(maximum.saturating_add(2)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    let value = std::str::from_utf8(&bytes)?.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.len() > maximum {
        bail!("stdin payload is empty or too large");
    }
    Ok(value.to_owned())
}

fn parse_share_uri_input(bytes: Vec<u8>, maximum: usize) -> Result<String> {
    if bytes.len() > maximum {
        bail!("share link from standard input exceeds its bound");
    }
    let uri = String::from_utf8(bytes).context("share link from standard input is not UTF-8")?;
    let uri = uri.trim_end_matches(['\r', '\n']);
    if uri.is_empty() {
        bail!("share link from standard input is empty");
    }
    Ok(uri.to_owned())
}

fn parse_identity(hex_str: &str) -> Result<IdentityKey> {
    let raw =
        hex::decode(hex_str.trim()).with_context(|| format!("identity {hex_str:?} is not hex"))?;
    let key: IdentityKey = raw.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "identity must be 32 bytes (64 hex chars), got {}",
            raw.len()
        )
    })?;
    Ok(key)
}

fn parse_principal(spec: &str) -> Result<Principal> {
    let (kind, id) = spec
        .split_once(':')
        .with_context(|| format!("principal {spec:?} must be KIND:ID"))?;
    let id = id.to_string();
    Ok(match kind {
        "person" => Principal::Person(id),
        "app" => Principal::App(id),
        "agent" => Principal::Agent(id),
        "token" => Principal::Token(id),
        "room" => Principal::Room(id),
        other => bail!("unknown principal kind {other:?} (person|app|agent|token|room)"),
    })
}

/// Resolve `--acl`/`--identity`/`--principal` into the ACL the mount enforces
/// (`None` if none requested). Fail-closed: any resolution failure refuses
/// the mount rather than falling back to unenforced; the certified human root
/// is the production default subject.
fn resolve_acl(
    args: &AclArgs,
    default_identity: IdentityKey,
) -> Result<Option<Arc<CapabilityFile>>> {
    let Some(path) = &args.acl else {
        if args.identity.is_some() || !args.principals.is_empty() {
            bail!(
                "--identity/--principal require --acl; without a capability list they enforce nothing"
            );
        }
        return Ok(None);
    };

    let identity = match &args.identity {
        Some(hex_str) => parse_identity(hex_str)?,
        None => default_identity,
    };
    let principals = args
        .principals
        .iter()
        .map(|s| parse_principal(s))
        .collect::<Result<Vec<_>>>()?;

    let file = CapabilityFile::load(path, identity, principals)
        .with_context(|| format!("binding the capability list {}", path.display()))?;
    println!(
        "Enforcing {} capabilities from {} as identity {}",
        file.count(),
        path.display(),
        hex::encode(identity)
    );
    Ok(Some(Arc::new(file)))
}

/// Keep the live mount's capability list current (`every == 0` disables
/// reload): re-read on every change of the file, and poll every `every`
/// seconds only when it cannot be watched.
fn spawn_acl_reload(file: Arc<CapabilityFile>, every: u64) -> Option<tokio::task::JoinHandle<()>> {
    if every == 0 {
        println!(
            "Capability list {} will NOT be re-read; a later grant needs a remount",
            file.path().display()
        );
        return None;
    }
    Some(
        file.spawn_reload(fs::file_watch::ReloadCadence::new(Duration::from_secs(
            every,
        ))),
    )
}

/// Sign each new version of the capability list into the workspace
/// authority; polls once a second only when the file cannot be watched.
fn spawn_workspace_authority_reload(
    path: PathBuf,
    authority: Arc<WorkspaceAuthority>,
    governor: ed25519_dalek::SigningKey,
) -> tokio::task::JoinHandle<()> {
    fs::workspace_authority::spawn_capability_file_reload(
        path,
        authority,
        governor,
        fs::file_watch::ReloadCadence::new(Duration::from_secs(1)),
    )
}

/// Serve document media from the content store (best-effort, like the other
/// local sockets). Content of another device's records reaches this store on
/// the record's route, through the session.
fn spawn_document_media(
    socket: &Path,
    store: &Arc<ContentStore>,
    cache_dir: Option<&Path>,
) -> Option<std::thread::JoinHandle<()>> {
    // The device's one document-media service also keeps the device's copies
    // of public files, beside the content store whose budget evicts them.
    let public = cache_dir.and_then(|dir| {
        let origin = fs::public_content::NetOrigin::new()
            .map_err(|error| eprintln!("public content cannot be fetched here: {error}"))
            .ok()?;
        fs::public_content::PublicContent::open(
            &dir.join("public-content"),
            Arc::clone(store.files()),
            origin,
        )
        .map_err(|error| eprintln!("public content cannot be kept here: {error}"))
        .ok()
    });
    let media = match public {
        Some(public) => fs::document_media::DocumentMedia::with_public(Arc::clone(store), public),
        None => fs::document_media::DocumentMedia::new(Arc::clone(store)),
    };
    match fs::document_media::serve_socket(socket, media) {
        Ok(handle) => Some(handle),
        Err(error) => {
            eprintln!(
                "could not serve document media at {}: {error}",
                socket.display()
            );
            None
        }
    }
}

/// An owner-only socket at `path` for the tokio runtime, replacing a stale one.
fn bind_private_socket(path: &Path) -> std::io::Result<tokio::net::UnixListener> {
    let listener = tilo_files::native::private::bind_socket(path)?;
    listener.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(listener)
}

/// Bind and serve the document control socket (best-effort), using the same ACL/identity as the mount.
fn spawn_control(
    sock: &Path,
    namespace: Arc<Mutex<Namespace>>,
    sync: Arc<SyncHandle>,
    enforcement: fs::control::Enforcement,
) -> tokio::task::JoinHandle<()> {
    // The socket can read and edit every document — restrict to the owner.
    match bind_private_socket(sock) {
        Ok(listener) => {
            println!("Document control socket at {}", sock.display());
            tokio::spawn(fs::control::serve(
                listener,
                namespace,
                Some(sync),
                enforcement,
            ))
        }
        Err(e) => {
            eprintln!("could not bind control socket {}: {e}", sock.display());
            tokio::spawn(async {})
        }
    }
}

fn spawn_share_control(
    socket: &Path,
    server: Arc<ShareControlServer>,
) -> Result<tokio::task::JoinHandle<()>> {
    let listener = bind_private_socket(socket)
        .with_context(|| format!("binding share socket {}", socket.display()))?;
    println!("Files share control socket at {}", socket.display());
    Ok(tokio::spawn(async move {
        if let Err(error) = server.serve(listener).await {
            eprintln!("share control service stopped: {error}");
        }
    }))
}

async fn open_store(cache_dir: Option<PathBuf>) -> Result<ContentStore> {
    match cache_dir {
        Some(dir) => ContentStore::open(dir).await,
        None => Ok(ContentStore::memory()),
    }
}

/// [`open_store`] for a mount that may serve the native data plane: its
/// file-backed content lives on the store volume next to the room, so
/// publishing a file and filling a placeholder share extents with it
/// instead of copying them.
async fn open_workspace_store(
    cache_dir: Option<PathBuf>,
    data_plane: &DataPlaneArgs,
    workspace: uuid::Uuid,
) -> Result<ContentStore> {
    let Some(dir) = cache_dir else {
        return Ok(ContentStore::memory());
    };
    match native_content_dir(data_plane, workspace) {
        Some(files) => ContentStore::open_with_files(dir, &files).await,
        None => ContentStore::open(dir).await,
    }
}

/// `<store>/.content/<room>` when the native data plane may run on a
/// dedicated store volume.
#[cfg(target_os = "linux")]
fn native_content_dir(data_plane: &DataPlaneArgs, workspace: uuid::Uuid) -> Option<PathBuf> {
    if data_plane.data_plane == DataPlaneChoice::Fuse
        || fs_native::store::check_dedicated(&data_plane.store).is_err()
    {
        return None;
    }
    Some(
        data_plane
            .store
            .join(".content")
            .join(workspace.to_string()),
    )
}

#[cfg(not(target_os = "linux"))]
fn native_content_dir(_data_plane: &DataPlaneArgs, _workspace: uuid::Uuid) -> Option<PathBuf> {
    None
}

fn workspace_ticket_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("workspace-ticket")
}

/// The peers a mount of `ticket`'s workspace may take its namespace from:
/// the ticket's node, then persisted content peers registered for this same
/// workspace. A persisted peer of another workspace, or one registered
/// without a ticket, still serves content by hash, but never the tree.
fn bootstrap_peers(
    ticket: &Ticket,
    cache_dir: Option<&Path>,
) -> Result<Vec<iroh_base::EndpointAddr>> {
    let mut peers = vec![ticket.node.clone()];
    if let Some(cache_dir) = cache_dir {
        for peer in load_persisted_content_peers(cache_dir)? {
            let same_workspace = peer
                .decoded_ticket()?
                .is_some_and(|bound| bound.workspace_id() == ticket.workspace_id());
            if same_workspace && peers.iter().all(|known| known.id != peer.endpoint.id) {
                peers.push(peer.endpoint);
            }
        }
    }
    Ok(peers)
}

/// A daemon can serve either its own workspace or exactly one discovered
/// remote workspace. Refuse ambiguous persisted state instead of combining
/// record planes: records are signed against a workspace UUID, so selecting an
/// arbitrary ticket could make a collaborator read the wrong namespace.
fn persisted_remote_workspace_ticket(cache_dir: Option<&Path>) -> Result<Option<Ticket>> {
    let Some(cache_dir) = cache_dir else {
        return Ok(None);
    };
    let mut selected: Option<Ticket> = None;
    for peer in load_persisted_content_peers(cache_dir)? {
        let Some(ticket) = peer.decoded_ticket()? else {
            continue;
        };
        if let Some(existing) = &selected {
            anyhow::ensure!(
                existing.workspace == ticket.workspace,
                "content peer cache contains more than one remote workspace; use a separate cache"
            );
        } else {
            selected = Some(ticket);
        }
    }
    Ok(selected)
}

fn personal_continuity_workspace_id(user_public_key: &[u8; 32]) -> u64 {
    let hash = blake3::derive_key("tilo-personal-continuity-v1\0", user_public_key);
    let mut first8 = [0_u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    if id == 0 { 1 } else { id }
}

fn managed_system_service() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
}

/// Export a transfer-ready home-peer bundle from the workspace this device is
/// already serving. The ticket is written by `fs serve`, so no terminal ticket
/// or per-file configuration needs to be copied by hand.
fn export_home_peer(
    cache_dir: &Path,
    acl: &Path,
    output: &Path,
    replica_cache_dir: &Path,
    status_file: PathBuf,
    principals: Vec<String>,
    mut relay_workspace_ids: Vec<u64>,
) -> Result<()> {
    let ticket_path = workspace_ticket_path(cache_dir);
    let ticket = std::fs::read_to_string(&ticket_path)
        .with_context(|| format!("read active workspace ticket {}", ticket_path.display()))?;
    let decoded_ticket =
        Ticket::decode(ticket.trim()).context("active workspace ticket is invalid")?;
    if !acl.is_file() {
        bail!("capability list {} is not a regular file", acl.display());
    }
    let capability_json = std::fs::read_to_string(acl)
        .with_context(|| format!("read capability list {}", acl.display()))?;
    let _: serde_json::Value =
        serde_json::from_str(&capability_json).context("capability list is not valid JSON")?;
    let capabilities: collab_types::FsAuthority = serde_json::from_str(&capability_json)?;
    let paths = identity_paths();
    let governor = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)
        .context("read workspace governor key")?;
    relay_workspace_ids.push(personal_continuity_workspace_id(
        &governor.verifying_key().to_bytes(),
    ));
    relay_workspace_ids.sort_unstable();
    relay_workspace_ids.dedup();
    let authority = WorkspaceAuthorityEnvelope::sign(
        decoded_ticket.workspace_id(),
        1,
        capabilities,
        false,
        &governor,
    )?;
    let workspace = HomePeerBundle {
        ticket: ticket.trim().to_owned(),
        cache_dir: replica_cache_dir.join(decoded_ticket.workspace_id().to_string()),
        capability_json,
        authority: Some(authority),
        principals,
        relay_workspace_ids,
    };
    let mut bundle = if output.exists() {
        toml::from_str::<HomePeerSet>(&std::fs::read_to_string(output)?)
            .context("parse existing home-peer bundle")?
    } else {
        HomePeerSet {
            status_file,
            workspaces: Vec::new(),
        }
    };
    // Re-exporting refreshes the ticket, ACL and cache assignment for this
    // workspace instead of leaving duplicate workers in the bundle.
    bundle.workspaces.retain(|entry| {
        Ticket::decode(&entry.ticket)
            .map_or(true, |ticket| ticket.workspace != decoded_ticket.workspace)
    });
    bundle.workspaces.push(workspace);
    tilo_files::native::atomic::write(output, toml::to_string_pretty(&bundle)?.as_bytes())?;
    println!("exported home-peer bundle to {}", output.display());
    Ok(())
}

/// Serve this replica's small file versions to the compute worker beside it
/// ([`fs::replica_content`]), owner-only, from the workspace's own cache
/// directory. Best-effort: without it the worker fetches those inputs.
fn spawn_replica_content(
    cache_dir: &Path,
    store: Arc<ContentStore>,
) -> Option<tokio::task::JoinHandle<()>> {
    let socket = fs::replica_content::socket_path(cache_dir);
    match bind_private_socket(&socket) {
        Ok(listener) => Some(tokio::spawn(fs::replica_content::serve(listener, store))),
        Err(error) => {
            eprintln!(
                "could not serve replica content at {}: {error}",
                socket.display()
            );
            None
        }
    }
}

/// Join a workspace as a durable, serving peer without a FUSE mount. The
/// normal workspace router keeps the supplied live capability policy in front
/// of its blob provider, so a home peer never becomes an unenforced source.
async fn replica(config_path: PathBuf) -> Result<()> {
    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("read home-peer bundle {}", config_path.display()))?;
    let bundle: HomePeerSet = toml::from_str(&config_text)
        .with_context(|| format!("parse home-peer bundle {}", config_path.display()))?;
    validate_home_peer_set(&bundle)?;
    // Held for the life of the process: the relay honours a ready status
    // only while this lock is held (or its stamp is a few seconds old), so
    // the status is written when it changes rather than every second.
    // Taken before the status is touched, so a second replica
    // refused here cannot clobber a running one's status.
    let _alive = fs::full_replica::ReplicaLiveness::hold(&bundle.status_file)?;
    save_status(
        &bundle.status_file,
        &fs::full_replica::FullReplicaStatus::syncing(),
    )?;
    let workspace_count = bundle.workspaces.len();
    let (status_tx, mut status_rx) = tokio::sync::mpsc::channel(workspace_count.saturating_mul(2));
    for (index, workspace) in bundle.workspaces.into_iter().enumerate() {
        let status_tx = status_tx.clone();
        tokio::spawn(async move {
            if let Err(error) = replica_workspace(workspace, index, status_tx.clone()).await {
                let mut failed = fs::full_replica::FullReplicaStatus::syncing();
                failed.state = "failed".into();
                failed.error = Some(error.to_string());
                let _ = status_tx.send((index, failed)).await;
            }
        });
    }
    drop(status_tx);
    let mut states = vec![fs::full_replica::FullReplicaStatus::syncing(); workspace_count];
    loop {
        let Some((index, worker_status)) = status_rx.recv().await else {
            bail!("all home-peer workspace workers exited");
        };
        let Some(state) = states.get_mut(index) else {
            bail!("home-peer worker returned an invalid workspace index");
        };
        *state = worker_status;
        let aggregate = aggregate_status(&states);
        save_status(&bundle.status_file, &aggregate)?;
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "The home-peer lifecycle keeps authority setup, record reconciliation, hydration, durable checkpointing, and readiness publication in order."
)]
async fn replica_workspace(
    bundle: HomePeerBundle,
    index: usize,
    status_tx: tokio::sync::mpsc::Sender<(usize, fs::full_replica::FullReplicaStatus)>,
) -> Result<()> {
    let ticket = Ticket::decode(&bundle.ticket).context("decode home-peer ticket")?;
    std::fs::create_dir_all(&bundle.cache_dir)?;

    let (transport_gate, _transport_gate_task) = fs::transport_gate::spawn_from_env()?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity_once_valid(&paths).await?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let transport_identity = FsTransportIdentity::new(credentials.clone(), revocations)
        .with_transport_gate(transport_gate);
    let governor = credentials.user_public_key();
    let authority = WorkspaceAuthority::open(
        bundle.cache_dir.join("workspace-authority.bin"),
        ticket.workspace_id(),
        governor,
        governor,
        bundle
            .principals
            .iter()
            .map(|principal| parse_principal(principal))
            .collect::<Result<Vec<_>>>()?,
    )?;
    let envelope = bundle
        .authority
        .clone()
        .context("home-peer bundle requires signed workspace authority")?;
    // A transferred bundle is bootstrap material, not an authority rollback.
    // A NAS may already hold a newer signed revision after a previous
    // enrollment; retain it across re-provisioning while still rejecting a
    // malformed or wrong-workspace bundle before it reaches the worker.
    envelope.verify(ticket.workspace_id(), governor)?;
    if authority
        .current()
        .is_none_or(|current| envelope.revision > current.revision)
    {
        authority.apply(envelope)?;
    }
    let acl = authority.handle();
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret).await?;
    let store = Arc::new(open_store(Some(bundle.cache_dir.clone())).await?);
    let _replica_content = spawn_replica_content(&bundle.cache_dir, Arc::clone(&store));
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), Arc::clone(&store)));
    let namespace_path = namespace_path(Some(&bundle.cache_dir));
    let bootstrap = bootstrap_peers(&ticket, Some(&bundle.cache_dir))?;
    for peer in &bootstrap {
        hydrator.add_peer(peer.clone());
    }
    let namespace = Arc::new(Mutex::new(
        open_namespace_mount(
            endpoint.id().to_string(),
            namespace_path.as_deref(),
            &endpoint,
            &bootstrap,
            &transport_identity,
            (ticket.workspace_id(), &Some(acl.clone())),
        )
        .await?,
    ));
    // A home peer is a real record replica, not merely a namespace/blob
    // cache. Keep the signed record repository and its materialized projection
    // exactly as a normal mounted collaborator does, so replacements,
    // revocations and signed content revisions converge before hydration.
    let share_authority = open_share_authority(Some(&bundle.cache_dir))?;
    let mut record_services = open_record_services(Some(&bundle.cache_dir), share_authority)?
        .context("home-peer record repository unavailable")?;
    record_services.workspace_authority = Some(Arc::new(authority));
    let history = open_history(Some(&bundle.cache_dir))?;
    let workspace_id = ticket.workspace_id();
    let materializer = Arc::new(RecordMaterializer::new(
        workspace_id,
        Arc::clone(&namespace),
        history.clone(),
    ));
    materializer.reconcile_repository(&record_services.repository)?;
    let replica_repository = Arc::clone(&record_services.repository);
    record_services.mutation_sink = Some(materializer.clone());
    record_services.materializer = Some(Arc::clone(&materializer));
    let workspace = Workspace::join_with_records(
        sync::SyncNode {
            endpoint: endpoint.clone(),
            lookup,
            store,
            hydrator: Arc::clone(&hydrator),
            namespace: Arc::clone(&namespace),
            identity: transport_identity,
            policy: Some(acl),
        },
        ticket,
        record_services,
    )
    .await?;
    materializer.attach_sync(Arc::clone(&workspace.sync));
    let _persist = namespace_path
        .clone()
        .map(|path| spawn_persist(Arc::clone(&namespace), path));
    println!("Home peer joined workspace; hydrating complete content replica.");
    let mut hydrated_namespace = None;
    let mut wait = ReplicaWait::new(&namespace);
    loop {
        // Namespace anti-entropy and signed-record anti-entropy are separate
        // authenticated exchanges. Project the durable record truth before
        // selecting a generation, so a namespace that arrived a few
        // milliseconds earlier can never be advertised as ready with stale
        // record metadata behind it.
        if let Err(error) = materializer.reconcile_repository(&replica_repository) {
            let mut failed = fs::full_replica::FullReplicaStatus::syncing();
            failed.state = "failed".into();
            failed.error = Some(format!("reconcile signed replica projection: {error}"));
            let _ = status_tx.send((index, failed)).await;
            wait.retry().await;
            continue;
        }
        let before = replica_namespace_fingerprint(&namespace);
        if hydrated_namespace.as_ref() == Some(&before) {
            // The proof stands for as long as this process holds its
            // liveness lock: nothing to restamp. Any namespace delta wakes
            // the next pass, which withdraws it first.
            wait.settled().await;
            continue;
        }
        // Withdraw first. A partial or stalled pass must never leave the
        // previous workspace revision advertised as a complete replica.
        let _ = status_tx
            .send((index, fs::full_replica::FullReplicaStatus::syncing()))
            .await;
        let mut status = hydrate_full_workspace(&namespace, &hydrator).await;
        if status.full_content_replication {
            status.filesystem_endpoints.push(endpoint.addr());
        }
        let after = replica_namespace_fingerprint(&namespace);
        if status.full_content_replication && before != after {
            // A reconciliation landed while blobs were downloading. Start a
            // fresh pass against that exact namespace rather than claiming a
            // hybrid of two revisions.
            hydrated_namespace = None;
            continue;
        }
        if status.full_content_replication {
            // The workspace did not change across this hydration pass. Remove
            // only blobs absent from its current authenticated root set, then
            // force a fresh pass if a concurrent update arrives.
            if let Err(error) =
                prune_obsolete_workspace_content(&namespace, &hydrator, history.as_deref()).await
            {
                status = fs::full_replica::FullReplicaStatus::syncing();
                status.state = "failed".into();
                status.error = Some(format!("prune obsolete replica content: {error}"));
            } else if replica_namespace_fingerprint(&namespace) != after {
                // A new root arrived while obsolete blobs were being released.
                // The initial syncing status remains in force; hydrate that
                // new root before publishing another ready proof.
                hydrated_namespace = None;
                continue;
            }
        }
        if status.full_content_replication {
            if let Err(error) = materializer.reconcile_repository(&replica_repository) {
                status = fs::full_replica::FullReplicaStatus::syncing();
                status.state = "failed".into();
                status.error = Some(format!("verify signed replica projection: {error}"));
            } else if replica_namespace_fingerprint(&namespace) != after {
                // Signed records changed (or corrected a namespace-first
                // arrival) during hydration. Verify and hydrate that exact
                // projection on the next pass.
                hydrated_namespace = None;
                continue;
            }
        }
        if status.full_content_replication {
            // Do not advertise a content-complete generation until its
            // namespace roots are durable too.  The blob store survives a
            // restart already; without this checkpoint it could not prove
            // which retained blobs are live after the owner goes offline.
            let Some(path) = namespace_path.as_ref() else {
                status = fs::full_replica::FullReplicaStatus::syncing();
                status.state = "failed".into();
                status.error = Some("home peer has no durable namespace path".into());
                let _ = status_tx.send((index, status)).await;
                wait.retry().await;
                continue;
            };
            // Unchanged heads before and after the save prove it persisted
            // exactly the generation just verified.
            let unchanged_before = replica_namespace_fingerprint(&namespace) == after;
            match persist_namespace_generation(&namespace, path) {
                Ok(persisted)
                    if unchanged_before && replica_namespace_fingerprint(&namespace) == after =>
                {
                    status.namespace_generation = Some(hex::encode(persisted));
                }
                Ok(_) => {
                    // A namespace delta raced this pass.  Its checkpoint is
                    // durable, but it has not yet had its blobs verified.
                    hydrated_namespace = None;
                    continue;
                }
                Err(error) => {
                    status = fs::full_replica::FullReplicaStatus::syncing();
                    status.state = "failed".into();
                    status.error = Some(format!("persist replica namespace: {error}"));
                }
            }
        }
        let ready = status.full_content_replication;
        let _ = status_tx.send((index, status)).await;
        hydrated_namespace = ready.then_some(after);
        if ready {
            wait.settled().await;
        } else {
            // Content a peer could not serve yet: try again, backing off,
            // or at once when the namespace moves.
            wait.retry().await;
        }
        let _ = &workspace;
    }
}

/// What the home-peer loop sleeps on between passes. A ready
/// replica waits for the namespace to change; an incomplete or failed pass
/// is retried after 1 s, doubling to a minute, and at once on a namespace
/// change. It used to wake every second either way to restamp its proof.
struct ReplicaWait {
    changes: tokio::sync::watch::Receiver<()>,
    retry: Duration,
}

impl ReplicaWait {
    const FIRST_RETRY: Duration = Duration::from_secs(1);
    const MAX_RETRY: Duration = Duration::from_secs(60);
    /// A ready replica re-projects the signed records this often even with
    /// no namespace change: a bound for anything that did not announce.
    const BACKSTOP: Duration = Duration::from_secs(300);

    fn new(namespace: &Arc<Mutex<Namespace>>) -> Self {
        Self {
            changes: namespace.lock_recover().subscribe_changes(),
            retry: Self::FIRST_RETRY,
        }
    }

    /// After a pass that left the replica ready.
    async fn settled(&mut self) {
        self.retry = Self::FIRST_RETRY;
        self.until_change(Self::BACKSTOP).await;
    }

    /// After a pass that could not.
    async fn retry(&mut self) {
        let delay = self.retry;
        self.retry = delay.saturating_mul(2).min(Self::MAX_RETRY);
        self.until_change(delay).await;
    }

    async fn until_change(&mut self, at_most: Duration) {
        tokio::select! {
            changed = self.changes.changed() => {
                if changed.is_err() {
                    // The namespace is gone; only the timer is left.
                    tokio::time::sleep(at_most).await;
                }
            }
            () = tokio::time::sleep(at_most) => {}
        }
    }
}

/// The namespace's generation for change detection: its Automerge heads
/// (every change moves them), not a hash of a full save every heartbeat.
fn replica_namespace_fingerprint(namespace: &Arc<Mutex<Namespace>>) -> Vec<automerge::ChangeHash> {
    namespace.lock_recover().heads()
}

#[allow(
    clippy::too_many_lines,
    reason = "Serving assembles certified transport, ACLs, durable record services, namespace recovery, and mount task lifetimes in one ordered lifecycle."
)] // Linear CLI assembly keeps resource lifetimes explicit.
async fn serve(
    mount_point: PathBuf,
    cache_dir: Option<PathBuf>,
    seed: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
    blob_sock: Option<PathBuf>,
    acl_args: AclArgs,
    data_plane: DataPlaneArgs,
) -> Result<()> {
    let (transport_gate, _transport_gate_task) = fs::transport_gate::spawn_from_env()?;
    let np = namespace_path(cache_dir.as_deref());
    let replacement_transactions = cache_dir
        .as_ref()
        .zip(np.as_ref())
        .map(|(cache, namespace_path)| {
            fs::RecordTransactionJournal::open(cache.join("record-transactions"))
                .map(|journal| (Arc::new(Mutex::new(journal)), namespace_path.clone()))
        })
        .transpose()?;
    let thumb_dir = thumb_path(cache_dir.as_deref());
    let share_socket = share_control_path(cache_dir.as_deref());
    let availability = open_availability(cache_dir.as_deref())?;
    let eviction_queue = open_eviction_queue(cache_dir.as_deref())?;
    let share_authority = open_share_authority(cache_dir.as_deref())?;
    let mut record_services = open_record_services(cache_dir.as_deref(), share_authority.clone())?;
    if let Some(records) = record_services.as_mut() {
        if let Some((journal, _)) = &replacement_transactions {
            records.repository.restore_admission_fences(
                &*journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("transaction journal poisoned"))?,
            )?;
        }
        records.availability.clone_from(&availability);
    }
    let history = open_history(cache_dir.as_deref())?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity_once_valid(&paths).await?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let share_issuer = load_workspace_share_issuer(&paths, &credentials)?;
    let transport_identity =
        FsTransportIdentity::new(credentials.clone(), Arc::clone(&revocations))
            .with_transport_gate(transport_gate);
    let history_actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    // The relays wait until something means another party may dial this
    // mount (#4224, `fs::reachability`): each trace of one wakes them.
    let (endpoint, lookup, reachability) =
        sync::bind_dormant_endpoint_with_key(device.secret.clone()).await?;
    if let Some(authority) = share_authority.as_ref()
        && authority.has_unrevoked_grants()
    {
        reachability::wake(&reachability, "a share grant is live");
    }
    // Files act as the certified person by default; transport authorship and
    // device-specific grants continue to use the endpoint/device key.
    let caps = resolve_acl(&acl_args, credentials.user_public_key())?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _acl_reachability = if let Some(caps) = &caps {
        reachability::wake_when_acl_admits_others(
            &reachability,
            &caps.handle(),
            "the capability list names someone else",
        )
    } else {
        reachability::wake(&reachability, "the mount enforces no capability list");
        None
    };
    let _reload = caps.map(|c| spawn_acl_reload(c, acl_args.acl_reload_secs));
    let named_outbox = cache_dir
        .as_ref()
        .map(|cache| {
            fs::NamedShareOutbox::open(cache.join("named-share-outbox.json")).map(Arc::new)
        })
        .transpose()?;
    // A registered NAS ticket changes the *workspace identity*, not merely a
    // blob source.  Joining it here keeps the record mesh on the NAS UUID
    // after `fs.service` restarts instead of deriving a fresh local UUID.
    let remote_ticket = persisted_remote_workspace_ticket(cache_dir.as_deref())?;
    let workspace_id = remote_ticket.as_ref().map_or_else(
        || Ticket::workspace_id_for_topic(*endpoint.id().as_bytes()),
        Ticket::workspace_id,
    );
    if remote_ticket.is_some() {
        reachability::wake(
            &reachability,
            "this device joined another device's workspace",
        );
    }
    let store = Arc::new(open_workspace_store(cache_dir.clone(), &data_plane, workspace_id).await?);
    let author = endpoint.id().to_string();
    let hydrator = Arc::new(
        Hydrator::new(endpoint.clone(), store.clone()).with_reachability(reachability.clone()),
    );
    if let Some(cache_dir) = cache_dir.as_deref() {
        for peer in load_persisted_content_peers(cache_dir)? {
            hydrator.add_peer(peer.endpoint);
        }
    }
    let (ns_value, fresh) = match &remote_ticket {
        Some(ticket) => {
            let bootstrap = bootstrap_peers(ticket, cache_dir.as_deref())?;
            (
                open_namespace_mount(
                    author.clone(),
                    np.as_deref(),
                    &endpoint,
                    &bootstrap,
                    &transport_identity,
                    (
                        ticket.workspace_id(),
                        &acl.as_ref().map(|(handle, _)| handle.clone()),
                    ),
                )
                .await?,
                !np.as_ref().is_some_and(|path| path.exists()),
            )
        }
        None => match np.as_ref().filter(|p| p.exists()) {
            Some(p) => (load_namespace(p, author.clone())?, false),
            None => (Namespace::new(author.clone())?, true),
        },
    };
    if ns_value.has_synced_with_peers() {
        reachability::wake(
            &reachability,
            "this workspace has synced with a peer before",
        );
    }
    let namespace = Arc::new(Mutex::new(ns_value));
    let record_materializer = if let Some(records) = record_services.as_mut() {
        let materializer =
            RecordMaterializer::new(workspace_id, namespace.clone(), history.clone());
        let materializer = match &replacement_transactions {
            Some((journal, namespace_path)) => {
                materializer.with_transaction_recovery(Arc::clone(journal), namespace_path.clone())
            }
            None => materializer,
        };
        let materializer = Arc::new(materializer);
        let recovery = materializer.reconcile_repository(&records.repository)?;
        if recovery.operations_replayed > 0 || recovery.unknown_or_tombstoned_records > 0 {
            println!(
                "Reconciled {} record operation(s); {} deleted/unknown record(s) skipped",
                recovery.operations_replayed, recovery.unknown_or_tombstoned_records
            );
        }
        records.mutation_sink = Some(materializer.clone());
        records.materializer = Some(materializer.clone());
        Some(materializer)
    } else {
        None
    };

    if fresh {
        if let Some(seed) = seed {
            seed_from_dir(&namespace, &store, &seed).await?;
            println!("Seeded workspace from {}", seed.display());
        }
    } else if let Some(p) = &np {
        println!("Restored workspace from {}", p.display());
    }

    let record_repository = record_services
        .as_ref()
        .map(|records| Arc::clone(&records.repository));
    // The serving owner is the authority publisher. A capability file may
    // bootstrap its first signed revision, but every peer thereafter enforces
    // the durable governor-signed state exposed by the authority ALPN.
    if let (Some(records), Some(cache_dir)) = (record_services.as_mut(), cache_dir.as_ref()) {
        let principals = acl_args
            .principals
            .iter()
            .map(|principal| parse_principal(principal))
            .collect::<Result<Vec<_>>>()?;
        let bootstrap = acl_args
            .acl
            .as_ref()
            .filter(|_| remote_ticket.is_none())
            .zip(share_issuer.as_ref())
            .map(|(path, signer)| -> Result<_> {
                let authority: collab_types::FsAuthority =
                    serde_json::from_slice(&std::fs::read(path)?)
                        .context("parse workspace authority bootstrap capabilities")?;
                Ok((authority.capabilities, signer))
            })
            .transpose()?;
        records.workspace_authority = workspace_startup::owner_authority(
            cache_dir,
            workspace_id,
            credentials.user_public_key(),
            principals,
            bootstrap,
        )?;
    }
    let _authority_reachability = record_services
        .as_ref()
        .and_then(|records| records.workspace_authority.as_ref())
        .and_then(|authority| {
            reachability::wake_when_acl_admits_others(
                &reachability,
                &authority.handle(),
                "the workspace authority names someone else",
            )
        });
    let record_publisher = record_repository
        .clone()
        .map(|repository| {
            RecordPublisher::new(
                repository,
                workspace_id,
                history_actor,
                credentials.signing_key().clone(),
            )
            .map(Arc::new)
        })
        .transpose()?;
    #[cfg(target_os = "linux")]
    let external_history = if storage_sock.is_some() {
        match (
            record_services
                .as_ref()
                .and_then(|records| records.workspace_authority.as_ref()),
            record_publisher.as_ref(),
        ) {
            (Some(authority), Some(publisher)) => {
                match fs::external_history::ExternalHistoryFactory::for_mount(
                    credentials.clone(),
                    Arc::clone(&revocations),
                    Arc::clone(authority),
                    Arc::clone(publisher),
                    &paths,
                    history.as_ref().zip(replacement_transactions.as_ref()).map(
                        |(history, (journal, path))| {
                            fs::external_history::ExternalHistoryRuntime::from_mount(
                                Arc::clone(history),
                                Arc::clone(journal),
                                Arc::clone(&namespace),
                                path.clone(),
                            )
                        },
                    ),
                ) {
                    Ok(factory) => factory,
                    Err(error) => {
                        eprintln!("external-source enrollment unavailable: {error}");
                        None
                    }
                }
            }
            _ => None,
        }
    } else {
        None
    };
    let allow_legacy_record_migration = remote_ticket.is_none()
        && record_repository
            .as_ref()
            .is_some_and(|repository| repository.is_empty());
    let initialized_record_authority = if allow_legacy_record_migration {
        match (&history, &record_publisher) {
            (Some(history), Some(publisher)) => {
                initialize_owner_record_authority(
                    namespace.clone(),
                    hydrator.clone(),
                    history.clone(),
                    history_actor,
                    publisher.clone(),
                )?;
                true
            }
            _ => false,
        }
    } else {
        false
    };
    let authority_reload = record_services.as_ref().and_then(|records| {
        records
            .workspace_authority
            .as_ref()
            .zip(share_issuer.as_ref())
            .zip(acl_args.acl.as_ref())
            .map(|((authority, governor), path)| {
                spawn_workspace_authority_reload(
                    path.clone(),
                    Arc::clone(authority),
                    governor.clone(),
                )
            })
    });
    // The desktop publishes signed ACL revisions through this group-restricted
    // local socket. It never receives write access to the daemon cache and the
    // daemon remains the single serialising authority writer.
    let authority_socket = record_services
        .as_ref()
        .and_then(|records| records.workspace_authority.as_ref())
        .map(|authority| local_authority_socket_path(authority.workspace()));
    let authority_socket_task = match (
        authority_socket.as_ref(),
        record_services
            .as_ref()
            .and_then(|records| records.workspace_authority.as_ref()),
    ) {
        (Some(socket), Some(authority)) => {
            Some(serve_local_authority(socket, Arc::clone(authority))?)
        }
        _ => None,
    };
    // Content discovery registration is useful to a read-only recipient too;
    // it must not depend on that recipient owning the workspace authority.
    let content_socket = record_services
        .as_ref()
        .map(|_| local_content_socket_path(workspace_id));
    // Only the managed system service may self-restart: an ad-hoc foreground
    // `fs serve` must remain attached to its caller and receives an explicit
    // actionable response from `register-content-peer` instead.
    let managed_service = managed_system_service();
    let (rebind_tx, rebind_rx) = tokio::sync::mpsc::unbounded_channel();
    let content_socket_task = match content_socket.as_ref() {
        Some(socket) => Some(serve_local_content_peer(
            socket,
            hydrator.clone(),
            cache_dir.clone(),
            workspace_id,
            managed_service.then_some(rebind_tx),
        )?),
        None => None,
    };
    // A discovered NAS ticket changes the namespace and record-plane UUID.
    // FUSE/router state cannot safely swap that identity in place, so the
    // managed mount receives a clean rebind outcome after acknowledging the
    // durable registration; systemd then starts the joined service again.
    let workspace_args = record_services
        .as_ref()
        .and_then(|records| {
            records
                .workspace_authority
                .as_ref()
                .map(|authority| authority.handle())
        })
        .or_else(|| acl.as_ref().map(|(handle, _)| handle.clone()));
    let _media = blob_sock
        .as_deref()
        .and_then(|socket| spawn_document_media(socket, &store, cache_dir.as_deref()));
    let (workspace, ticket, joined_remote) = match (record_services, remote_ticket) {
        (Some(records), Some(ticket)) => (
            Workspace::join_with_records(
                sync::SyncNode {
                    endpoint: endpoint.clone(),
                    lookup,
                    store: store.clone(),
                    hydrator: hydrator.clone(),
                    namespace: namespace.clone(),
                    identity: transport_identity,
                    policy: workspace_args,
                },
                ticket.clone(),
                records,
            )
            .await?,
            ticket,
            true,
        ),
        (Some(records), None) => {
            let (workspace, ticket) = Workspace::host_with_records(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
                transport_identity,
                workspace_args,
                records,
            )
            .await?;
            (workspace, ticket, false)
        }
        (None, Some(ticket)) => (
            Workspace::join(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
                ticket.clone(),
                transport_identity,
                workspace_args,
            )
            .await?,
            ticket,
            true,
        ),
        (None, None) => {
            let (workspace, ticket) = Workspace::host(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
                transport_identity,
                workspace_args,
            )
            .await?;
            (workspace, ticket, false)
        }
    };
    if let Some(materializer) = record_materializer {
        materializer.attach_sync(workspace.sync.clone());
    }

    println!("Iroh NodeID: {}", endpoint.id());
    if joined_remote {
        println!("Joined workspace from authenticated content discovery.");
    } else {
        println!(
            "Hosting workspace — share this ticket to invite peers:\n{}",
            ticket.encode()?
        );
    }
    if let Some(cache_dir) = &cache_dir {
        let encoded = ticket.encode()?;
        tilo_files::native::atomic::write(&workspace_ticket_path(cache_dir), encoded.as_bytes())?;
    }
    // Owner operations require the certified human root. Secondary devices
    // never substitute their device key and recipient mounts never host this
    // service.
    let share_task = match (
        (!joined_remote).then_some(share_socket.as_ref()).flatten(),
        &record_repository,
        &share_authority,
        (!joined_remote).then_some(share_issuer).flatten(),
    ) {
        (Some(socket), Some(repository), Some(authority), Some(issuer)) => {
            let issuer_public =
                collab_types::Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
            let mut server = ShareControlServer::new(
                ticket.workspace_id(),
                endpoint.addr(),
                issuer,
                vec![issuer_public],
                Arc::new(ShareManagement::new(Arc::clone(authority))),
                Arc::clone(repository),
            )?
            .with_reachability(reachability.clone());
            if let Some(outbox) = named_outbox.as_ref() {
                server = server.with_named_delivery_outbox(Arc::clone(outbox));
            }
            let server = Arc::new(server);
            Some(spawn_share_control(socket, server)?)
        }
        _ => None,
    };
    let named_delivery_task = named_outbox.map(|outbox| {
        let client = fs::NamedShareDeliveryClient::new(
            endpoint.clone(),
            credentials.clone(),
            Arc::clone(&revocations),
        );
        tokio::spawn(async move {
            let Ok(client) = client else {
                return;
            };
            // Delivers at once whatever is pending, then sleeps until
            // something is enqueued. What could not be delivered (the
            // recipient's devices offline) is retried with backoff from
            // 5 s to a minute, and at once when a new delivery arrives
            // (#4130: an empty outbox no longer wakes every 5 s).
            let mut retry = NAMED_DELIVERY_RETRY;
            loop {
                let now = unix_now_ms();
                let mut undelivered = false;
                for entry in outbox.pending_at(now) {
                    let mut delivered = None;
                    for device in &entry.recipient_devices {
                        let Ok(endpoint_id) = iroh::EndpointId::from_bytes(device.as_bytes())
                        else {
                            continue;
                        };
                        if client
                            .deliver(
                                iroh_base::EndpointAddr::new(endpoint_id),
                                entry.envelope.clone(),
                            )
                            .await
                            .is_ok()
                        {
                            delivered = Some(*device);
                            break;
                        }
                    }
                    undelivered |= delivered.is_none();
                    let _ =
                        outbox.record_attempt(entry.envelope.invite.id, delivered, unix_now_ms());
                }
                if undelivered {
                    tokio::select! {
                        () = tokio::time::sleep(retry) => {}
                        () = outbox.arrival() => {}
                    }
                    retry = retry.saturating_mul(2).min(NAMED_DELIVERY_RETRY_MAX);
                } else {
                    retry = NAMED_DELIVERY_RETRY;
                    outbox.arrival().await;
                }
            }
        })
    });

    let _control = control_sock
        .map(|s| spawn_control(&s, namespace.clone(), workspace.sync.clone(), acl.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    let mount_outcome = run_mount(
        MountConfig {
            checked_history_workspace: workspace_id,
            mount_point,
            namespace_path: np.clone(),
            predictive_cache_root: cache_dir.clone(),
            thumb_dir,
            share_control_path: share_socket.clone().filter(|_| share_task.is_some()),
            allow_legacy_record_migration: allow_legacy_record_migration
                && !initialized_record_authority,
            storage_sock,
            read_only: false,
            signal_recipient_ready: false,
            data_plane: Some((data_plane, *history_actor.device.as_bytes())),
        },
        MountServices {
            namespace: namespace.clone(),
            hydrator,
            sync: Some(workspace.sync.clone()),
            acl,
            availability,
            eviction_queue,
            share_authority,
            history: history.map(|history| (history, history_actor)),
            record_publisher,
            replacement_transactions,
            #[cfg(target_os = "linux")]
            external_history,
            rebind: managed_service.then_some(rebind_rx),
        },
    )
    .await?;
    final_flush(persist, &namespace, np.as_deref()).await;
    if let Some(task) = named_delivery_task {
        task.abort();
    }
    if let Some(task) = authority_reload {
        task.abort();
    }
    if let Some(task) = authority_socket_task {
        task.abort();
    }
    if let Some(task) = content_socket_task {
        task.abort();
    }
    if let Some(socket) = authority_socket {
        let _ = std::fs::remove_file(socket);
    }
    if let Some(socket) = content_socket {
        let _ = std::fs::remove_file(socket);
    }
    if let Some(task) = share_task {
        task.abort();
    }
    if let Some(socket) = share_socket {
        let _ = std::fs::remove_file(socket);
    }
    drop(workspace);
    match mount_outcome {
        MountOutcome::Unmounted => Ok(()),
        MountOutcome::RebindRequested => {
            anyhow::bail!("content peer registration requires a managed fs.service rebind")
        }
    }
}

fn initialize_owner_record_authority(
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    history: Arc<FileHistoryStore>,
    actor: fs_protocol::FileActor,
    publisher: Arc<RecordPublisher>,
) -> Result<()> {
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let initialized = QuickFS::new(namespace, hydrator, heuristics)
        .with_history(history, actor)?
        .with_record_publisher(publisher)?;
    drop(initialized);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "Joining a workspace keeps ticket validation, certified transport, record recovery, synchronization, mounting, and final persistence in order."
)] // Linear CLI assembly keeps resource lifetimes explicit.
async fn mount(
    mount_point: PathBuf,
    ticket: String,
    cache_dir: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
    blob_sock: Option<PathBuf>,
    acl_args: AclArgs,
    data_plane: DataPlaneArgs,
) -> Result<()> {
    let (transport_gate, _transport_gate_task) = fs::transport_gate::spawn_from_env()?;
    let ticket = Ticket::decode(&ticket)?;
    if let Some(cache_dir) = &cache_dir {
        tilo_files::native::atomic::write(
            &workspace_ticket_path(cache_dir),
            ticket.encode()?.as_bytes(),
        )?;
    }
    let np = namespace_path(cache_dir.as_deref());
    let replacement_transactions = cache_dir
        .as_ref()
        .zip(np.as_ref())
        .map(|(cache, namespace_path)| {
            fs::RecordTransactionJournal::open(cache.join("record-transactions"))
                .map(|journal| (Arc::new(Mutex::new(journal)), namespace_path.clone()))
        })
        .transpose()?;
    let thumb_dir = thumb_path(cache_dir.as_deref());
    let availability = open_availability(cache_dir.as_deref())?;
    let eviction_queue = open_eviction_queue(cache_dir.as_deref())?;
    let share_authority = open_share_authority(cache_dir.as_deref())?;
    let mut record_services = open_record_services(cache_dir.as_deref(), share_authority.clone())?;
    if let Some(records) = record_services.as_mut() {
        if let Some((journal, _)) = &replacement_transactions {
            records.repository.restore_admission_fences(
                &*journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("transaction journal poisoned"))?,
            )?;
        }
        records.availability.clone_from(&availability);
    }
    let history = open_history(cache_dir.as_deref())?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity_once_valid(&paths).await?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let transport_identity = FsTransportIdentity::new(credentials.clone(), revocations)
        .with_transport_gate(transport_gate);
    let history_actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    let caps = resolve_acl(&acl_args, credentials.user_public_key())?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _reload = caps.map(|c| spawn_acl_reload(c, acl_args.acl_reload_secs));
    let store = Arc::new(
        open_workspace_store(cache_dir.clone(), &data_plane, ticket.workspace_id()).await?,
    );
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let author = endpoint.id().to_string();
    let bootstrap = bootstrap_peers(&ticket, cache_dir.as_deref())?;
    for peer in &bootstrap {
        hydrator.add_peer(peer.clone());
    }

    let ns_value = open_namespace_mount(
        author,
        np.as_deref(),
        &endpoint,
        &bootstrap,
        &transport_identity,
        (
            ticket.workspace_id(),
            &acl.as_ref().map(|(handle, _)| handle.clone()),
        ),
    )
    .await?;
    let namespace = Arc::new(Mutex::new(ns_value));

    let workspace_id = ticket.workspace_id();
    let record_materializer = if let Some(records) = record_services.as_mut() {
        let materializer =
            RecordMaterializer::new(workspace_id, namespace.clone(), history.clone());
        let materializer = match &replacement_transactions {
            Some((journal, namespace_path)) => {
                materializer.with_transaction_recovery(Arc::clone(journal), namespace_path.clone())
            }
            None => materializer,
        };
        let materializer = Arc::new(materializer);
        let recovery = materializer.reconcile_repository(&records.repository)?;
        if recovery.operations_replayed > 0 || recovery.unknown_or_tombstoned_records > 0 {
            println!(
                "Reconciled {} record operation(s); {} deleted/unknown record(s) skipped",
                recovery.operations_replayed, recovery.unknown_or_tombstoned_records
            );
        }
        records.mutation_sink = Some(materializer.clone());
        records.materializer = Some(materializer.clone());
        Some(materializer)
    } else {
        None
    };
    let record_repository = record_services
        .as_ref()
        .map(|records| Arc::clone(&records.repository));
    let workspace_args = acl.as_ref().map(|(handle, _)| handle.clone());
    let _media = blob_sock
        .as_deref()
        .and_then(|socket| spawn_document_media(socket, &store, cache_dir.as_deref()));
    let workspace = match record_services {
        Some(records) => {
            Workspace::join_with_records(
                sync::SyncNode {
                    endpoint: endpoint.clone(),
                    lookup,
                    store: store.clone(),
                    hydrator: hydrator.clone(),
                    namespace: namespace.clone(),
                    identity: transport_identity,
                    policy: workspace_args,
                },
                ticket,
                records,
            )
            .await?
        }
        None => {
            Workspace::join(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
                ticket,
                transport_identity,
                workspace_args,
            )
            .await?
        }
    };
    if let Some(materializer) = record_materializer {
        materializer.attach_sync(workspace.sync.clone());
    }

    println!("Iroh NodeID: {}", endpoint.id());
    println!("Joined workspace.");
    let record_publisher = record_repository
        .map(|repository| {
            RecordPublisher::new(
                repository,
                workspace_id,
                history_actor,
                credentials.signing_key().clone(),
            )
            .map(Arc::new)
        })
        .transpose()?;

    let _control = control_sock
        .map(|s| spawn_control(&s, namespace.clone(), workspace.sync.clone(), acl.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    run_mount(
        MountConfig {
            checked_history_workspace: workspace_id,
            mount_point,
            namespace_path: np.clone(),
            predictive_cache_root: cache_dir,
            thumb_dir,
            share_control_path: None,
            allow_legacy_record_migration: false,
            storage_sock,
            read_only: false,
            signal_recipient_ready: false,
            data_plane: Some((data_plane, *history_actor.device.as_bytes())),
        },
        MountServices {
            namespace: namespace.clone(),
            hydrator,
            sync: Some(workspace.sync.clone()),
            acl,
            availability,
            eviction_queue,
            share_authority,
            history: history.map(|history| (history, history_actor)),
            record_publisher,
            replacement_transactions,
            #[cfg(target_os = "linux")]
            external_history: None,
            rebind: None,
        },
    )
    .await?;
    final_flush(persist, &namespace, np.as_deref()).await;
    drop(workspace);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "Exact-record mounting keeps credential checks, snapshot validation, scoped namespace projection, refresh tasks, and mount cleanup in one trust sequence."
)] // Shared-mount bootstrap is one ordered trust sequence.
async fn mount_share(
    mount_point: PathBuf,
    credential: SharedCredential,
    cache_dir: PathBuf,
) -> Result<()> {
    std::fs::create_dir_all(&cache_dir)?;
    let expected_record = credential.invite().record_id;
    let subscription_key = RecordSubscriptionKey {
        workspace_id: credential.invite().workspace_id,
        record_id: expected_record,
    };
    let subscriptions = match &credential {
        SharedCredential::Bearer(link) => {
            let store = Arc::new(RecordSubscriptionStore::open(
                cache_dir.join("record-subscriptions.json"),
            )?);
            store.insert(link.clone())?;
            Some(store)
        }
        SharedCredential::Named(_) => None,
    };
    let paths = identity_paths();
    let certified = load_certified_mount_identity_once_valid(&paths).await?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    credential.validate_at(actor.person, unix_now_ms())?;
    let replica_signing_key = credentials.signing_key().clone();
    let (endpoint, _lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    let client =
        RecordProtocolClient::new(endpoint.clone(), credentials, Arc::clone(&revocations))?;
    let snapshot = match fetch_shared_snapshot(&client, &credential).await {
        Ok(snapshot) => {
            if let Some(subscriptions) = &subscriptions {
                subscriptions.record_success(subscription_key, unix_now_ms())?;
            }
            snapshot
        }
        Err(error) => {
            if let Some(subscriptions) = &subscriptions {
                subscriptions.record_error(
                    subscription_key,
                    error.failure_code(),
                    unix_now_ms(),
                )?;
            }
            return Err(error.into());
        }
    };
    if snapshot.record_id != expected_record {
        bail!("share host returned a different stable record");
    }

    let namespace_path = cache_dir.join("shared-record-namespace.bin");
    let mut namespace = match fs::durable_backup::read_or_recover(&namespace_path, |bytes| {
        Namespace::from_bytes("shared-record-backup-validator", bytes).is_ok()
    })? {
        Some(outcome) => {
            report_namespace_recovery(&namespace_path, outcome.recovery.as_ref());
            Namespace::from_bytes(endpoint.id().to_string(), &outcome.bytes)?
        }
        None => Namespace::new(endpoint.id().to_string())?,
    };
    validate_shared_namespace(&mut namespace, expected_record, true)?;
    let namespace = Arc::new(Mutex::new(namespace));
    let cache = Some(cache_dir.clone());
    let history = open_history(cache.as_deref())?.context("recipient history store unavailable")?;
    apply_shared_snapshot(&namespace, &history, &snapshot)?;
    persist_now(&namespace, &namespace_path);

    let store = Arc::new(open_store(Some(cache_dir.join("blobs"))).await?);
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), Arc::clone(&store)));
    hydrator.add_peer(credential.endpoint().clone());
    let availability = open_availability(cache.as_deref())?;
    let eviction_queue = open_eviction_queue(cache.as_deref())?;
    let mut observed = false;
    if let Some(revision) = snapshot.content_revision {
        track_shared_revision(availability.as_ref(), revision)?;
        if let SharedCredential::Bearer(link) = &credential {
            match refresh_host_replica_observation(
                &client,
                link,
                revision,
                availability.as_ref(),
                actor.device,
            )
            .await
            {
                Ok(()) => observed = true,
                Err(error) => {
                    eprintln!("shared-record host replica is not yet available: {error}");
                }
            }
        }
    }
    let thumb_dir = thumb_path(cache.as_deref());

    // Keep the snapshot current and the host-side live blob session
    // registered: one held, authenticated stream on which the host answers
    // when the record changes, or a 5 s poll of a host that
    // predates that. Same-actor bearer redemption is idempotent, while
    // expiry/revocation still stops subsequent refreshes and fetch
    // authorization.
    let refresh = ShareRefreshTask {
        refresher: ShareRefresher::new(client, credential.clone(), actor.person, &snapshot),
        namespace: Arc::clone(&namespace),
        history: Arc::clone(&history),
        namespace_path: namespace_path.clone(),
        subscriptions: subscriptions.map(|store| (store, subscription_key)),
        store: Arc::clone(&store),
        availability: availability.clone(),
        replica_signing_key,
        actor,
        workspace_id: credential.invite().workspace_id,
        expected_record,
        revision: snapshot.content_revision,
        observed_at: observed.then(tokio::time::Instant::now),
        receipt: None,
    };
    let poll_task = tokio::spawn(refresh.run());

    println!(
        "Mounted exact shared record {} from certified endpoint {}",
        expected_record.0,
        credential.endpoint().id
    );
    // Keep local device controls writable (pin/retry/evict) while denying
    // every content or namespace mutation through the same QuickFS access
    // gate used by normal collaborators. A kernel-wide RO mount would reject
    // the control xattrs before QuickFS can distinguish them from data writes.
    let recipient_identity = *actor.person.as_bytes();
    let mut recipient_acl = fs::WorkspaceAcl::new();
    recipient_acl.grant_read(recipient_identity, "/");
    run_mount(
        MountConfig {
            checked_history_workspace: credential.invite().workspace_id,
            mount_point,
            namespace_path: Some(namespace_path.clone()),
            predictive_cache_root: Some(cache_dir.clone()),
            thumb_dir,
            share_control_path: None,
            allow_legacy_record_migration: false,
            storage_sock: None,
            read_only: false,
            signal_recipient_ready: true,
            data_plane: None,
        },
        MountServices {
            namespace: Arc::clone(&namespace),
            hydrator,
            sync: None,
            acl: Some((AclHandle::new(recipient_acl), recipient_identity)),
            availability,
            eviction_queue,
            share_authority: None,
            history: Some((history, actor)),
            record_publisher: None,
            replacement_transactions: None,
            #[cfg(target_os = "linux")]
            external_history: None,
            rebind: None,
        },
    )
    .await?;
    poll_task.abort();
    persist_now(&namespace, &namespace_path);
    endpoint.close().await;
    Ok(())
}

async fn recipient_service(
    socket: PathBuf,
    mounts_root: PathBuf,
    cache_root: PathBuf,
    fs_binary: Option<PathBuf>,
) -> Result<()> {
    prepare_owner_service_parent(&socket)?;
    // Consume and close-on-exec the activation descriptor before starting any
    // router tasks or supervised mount children that could otherwise inherit it.
    let listener = recipient_service_listener(&socket)?;
    std::fs::create_dir_all(&cache_root)?;
    let paths = identity_paths();
    let certified = match load_certified_mount_identity_once_valid(&paths).await {
        Ok(certified) => certified,
        Err(error) => {
            // Unconfigured session: keep the activation socket and wait for
            // stop. Exiting with an error crash-loops under Restart=on-failure.
            eprintln!("recipient-service idle: no certified mount identity ({error})");
            std::future::pending::<()>().await;
            return Ok(());
        }
    };
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let local_person = collab_types::Ed25519PublicKey::from_bytes(credentials.user_public_key());
    let inbox = Arc::new(fs::NamedShareInbox::open_at(
        cache_root.join("named-share-inbox.json"),
        local_person,
        unix_now_ms(),
    )?);
    let (inbox_endpoint, _inbox_lookup) = sync::bind_endpoint_with_key(device.secret).await?;
    let inbox_protocol = fs::NamedShareInboxProtocol::new(
        inbox_endpoint.clone(),
        credentials,
        Arc::clone(&revocations),
        Arc::clone(&inbox),
    )?;
    let inbox_router = iroh::protocol::Router::builder(inbox_endpoint.clone())
        .accept(fs::TILO_FS_NAMED_SHARE_INBOX_ALPN, inbox_protocol)
        .spawn();
    let subscriptions = Arc::new(RecordSubscriptionStore::open(
        cache_root.join("record-subscriptions.json"),
    )?);
    let executable = fs_binary.unwrap_or(std::env::current_exe()?);
    let mounts = Arc::new(RecipientMountSupervisor::new(
        executable,
        mounts_root,
        cache_root.join("records"),
    ));

    // Restart every still-valid admitted subscription. Failures remain in the
    // durable status store and do not prevent other records from mounting.
    for subscription in subscriptions.subscriptions() {
        if let Err(error) = mounts.start(&subscription) {
            eprintln!(
                "could not restart received record {:?}: {error}",
                subscription.key
            );
        }
    }
    for envelope in inbox.entries_at(unix_now_ms()) {
        if let Err(error) = mounts.start_named(&envelope) {
            eprintln!(
                "could not restart named received record {}: {error}",
                envelope.invite.record_id.0
            );
        }
    }

    let named_mounts = Arc::clone(&mounts);
    let named_inbox = Arc::clone(&inbox);
    // Mounts every accepted envelope at once, and restarts a named mount
    // whose child died. While every one is running the restart pass backs
    // off from 2 s to a minute; one that had to start a mount (or was woken
    // by a new envelope) goes back to 2 s (#4130: it used to run every
    // 2 s for the life of the service).
    let named_mount_task = tokio::spawn(async move {
        let mut every = NAMED_MOUNT_REPAIR;
        loop {
            let mut started = false;
            for envelope in named_inbox.entries_at(unix_now_ms()) {
                let key = RecordSubscriptionKey {
                    workspace_id: envelope.invite.workspace_id,
                    record_id: envelope.invite.record_id,
                };
                if named_mounts.is_running(key).unwrap_or(false) {
                    continue;
                }
                // A start that fails is retried on the backed-off pass.
                started |= named_mounts.start_named(&envelope).is_ok();
            }
            every = if started {
                NAMED_MOUNT_REPAIR
            } else {
                every.saturating_mul(2).min(NAMED_MOUNT_REPAIR_MAX)
            };
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                () = named_inbox.arrival() => every = NAMED_MOUNT_REPAIR,
            }
        }
    });

    let server = Arc::new(RecipientControlServer::with_mounts(subscriptions, mounts));
    println!("Recipient share service listening at {}", socket.display());
    let result = server.serve(listener).await;
    named_mount_task.abort();
    inbox_router.shutdown().await?;
    result?;
    Ok(())
}

fn recipient_service_listener(socket: &Path) -> Result<tokio::net::UnixListener> {
    // SAFETY: taken at the start of the recipient service, before it spawns
    // any task or child, while the runtime's worker threads are still idle
    // and nothing else reads the environment.
    let Some(listener) = (unsafe { tilo_files::native::private::activated_socket(socket)? }) else {
        return bind_recipient_service_socket(socket);
    };
    listener.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(listener).map_err(Into::into)
}

fn prepare_owner_service_parent(socket: &Path) -> Result<()> {
    let parent = socket
        .parent()
        .context("recipient service socket has no parent")?;
    std::fs::create_dir_all(parent)?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("recipient service parent is not a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn bind_recipient_service_socket(socket: &Path) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::FileTypeExt;
    match RecipientControlServer::bind(socket) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            let metadata = std::fs::symlink_metadata(socket)
                .context("recipient service socket path could not be inspected")?;
            if !metadata.file_type().is_socket() {
                return Err(error.into());
            }
            match std::os::unix::net::UnixStream::connect(socket) {
                Ok(_) => bail!("another recipient service is already listening"),
                Err(connect_error)
                    if matches!(
                        connect_error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    std::fs::remove_file(socket)?;
                    RecipientControlServer::bind(socket).map_err(Into::into)
                }
                Err(connect_error) => Err(connect_error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// How often an idle recipient renews the replica receipts it exchanges
/// with its host: inside [`fs::availability::EvictionRequirements`]'s
/// five-minute bound on a receipt's age, so either side may still evict.
/// A new revision renews them at once.
const SHARE_REPLICA_RENEWAL: Duration = Duration::from_secs(4 * 60);

/// The refresh loop of one mounted shared record.
///
/// Persists, records availability and exchanges replica receipts only when
/// something changed or a receipt is due: an idle record costs one await
/// a minute on a stream already held, and no disk write.
struct ShareRefreshTask {
    refresher: ShareRefresher,
    namespace: Arc<Mutex<Namespace>>,
    history: Arc<FileHistoryStore>,
    namespace_path: PathBuf,
    subscriptions: Option<(Arc<RecordSubscriptionStore>, RecordSubscriptionKey)>,
    store: Arc<ContentStore>,
    availability: Option<Arc<Mutex<fs::AvailabilityRegistry>>>,
    replica_signing_key: ed25519_dalek::SigningKey,
    actor: fs_protocol::FileActor,
    workspace_id: uuid::Uuid,
    expected_record: fs_protocol::FileRecordId,
    /// The revision the recipient holds.
    revision: Option<fs_protocol::ContentRevisionV1>,
    /// When the host last proved it holds `revision`.
    observed_at: Option<tokio::time::Instant>,
    /// The revision the host last accepted this device's receipt for.
    receipt: Option<(fs_protocol::ContentRevisionV1, tokio::time::Instant)>,
}

impl ShareRefreshTask {
    async fn run(mut self) {
        let mut failing = false;
        loop {
            let changed = match self.refresher.next().await {
                Ok(ShareRefresh::Changed(snapshot)) => {
                    if snapshot.record_id != self.expected_record {
                        self.record_error(RecordSubscriptionFailureCode::InvalidResponse);
                        break;
                    }
                    if let Err(error) = self.apply(&snapshot) {
                        eprintln!("shared record refresh rejected: {error}");
                        break;
                    }
                    true
                }
                Ok(ShareRefresh::Unchanged) => false,
                Err(SharedFetchError::Expired) => break,
                Err(error) => {
                    self.record_error(error.failure_code());
                    eprintln!("shared record refresh failed: {error}");
                    failing = true;
                    continue;
                }
            };
            if let Some(revision) = self.revision {
                self.renew_receipts(revision).await;
            }
            if (changed || failing)
                && let Some((subscriptions, key)) = &self.subscriptions
            {
                if let Err(error) = subscriptions.record_success(*key, unix_now_ms()) {
                    eprintln!("could not persist shared-record refresh status: {error}");
                    break;
                }
                failing = false;
            }
        }
    }

    /// Project a changed snapshot and persist it; a new revision clears
    /// the receipts of the old one.
    fn apply(&mut self, snapshot: &RecordProtocolSnapshotV1) -> Result<()> {
        apply_shared_snapshot(&self.namespace, &self.history, snapshot)?;
        persist_now(&self.namespace, &self.namespace_path);
        if snapshot.content_revision != self.revision {
            self.revision = snapshot.content_revision;
            self.observed_at = None;
            self.receipt = None;
            if let Some(revision) = self.revision {
                track_shared_revision(self.availability.as_ref(), revision)
                    .context("could not persist shared-record version")?;
            }
        }
        Ok(())
    }

    /// Ask the host for its receipt, and send this device's once the
    /// content is durable here, when either is missing or due.
    async fn renew_receipts(&mut self, revision: fs_protocol::ContentRevisionV1) {
        let due = |at: tokio::time::Instant| at.elapsed() >= SHARE_REPLICA_RENEWAL;
        if self.observed_at.is_none_or(due) {
            match self.observe_host(revision).await {
                Ok(()) => self.observed_at = Some(tokio::time::Instant::now()),
                Err(error) => {
                    eprintln!("shared-record host replica is not yet available: {error}");
                }
            }
        }
        let receipt_due = self
            .receipt
            .is_none_or(|(acked, at)| acked != revision || due(at));
        if !receipt_due || self.store.verify_durable_revision(&revision).await.is_err() {
            return;
        }
        let receipt =
            fs::ReplicaAckRequestV1::new(self.workspace_id, revision).and_then(|request| {
                fs::ReplicaAckV1::signed(
                    &request,
                    self.actor,
                    unix_now_ms(),
                    &self.replica_signing_key,
                )
            });
        match receipt {
            Ok(receipt) => {
                if matches!(
                    self.refresher
                        .request(RecordProtocolOperationV1::AcknowledgeReplica(receipt))
                        .await,
                    Ok(RecordProtocolResponseV1::ReplicaAcknowledged { .. })
                ) {
                    self.receipt = Some((revision, tokio::time::Instant::now()));
                } else {
                    eprintln!(
                        "shared record is durable locally but host did not accept its replica receipt"
                    );
                }
            }
            Err(error) => {
                eprintln!("could not sign shared-record replica receipt: {error}");
            }
        }
    }

    async fn observe_host(&mut self, revision: fs_protocol::ContentRevisionV1) -> Result<()> {
        let Some(availability) = &self.availability else {
            return Ok(());
        };
        let request = fs::ReplicaAckRequestV1::new(self.workspace_id, revision)?;
        let Some(observation) = self.refresher.host_replica_observation(request).await? else {
            return Ok(());
        };
        let observed_at_ms = observation.observed_at_ms;
        let mut registry = availability.lock_recover();
        registry.observe_replica(
            revision.record_id,
            self.actor.device,
            observation,
            observed_at_ms,
        )?;
        drop(registry);
        Ok(())
    }

    fn record_error(&self, code: RecordSubscriptionFailureCode) {
        if let Some((subscriptions, key)) = &self.subscriptions {
            let _ = subscriptions.record_error(*key, code, unix_now_ms());
        }
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "The availability guard spans reading the prior version, clearing stale replica evidence, and persisting the replacement record."
)] // Registry update and persistence are one mutation.
fn track_shared_revision(
    availability: Option<&Arc<Mutex<fs::AvailabilityRegistry>>>,
    revision: fs_protocol::ContentRevisionV1,
) -> Result<()> {
    let Some(availability) = availability else {
        return Ok(());
    };
    let mut registry = availability.lock_recover();
    let mut record = registry
        .get(revision.record_id)
        .cloned()
        .unwrap_or_else(|| {
            fs::availability::RecordAvailability::new(
                revision.record_id,
                fs::availability::RecordKind::File,
            )
        });
    if record.current_version != Some(revision.version_id) {
        record.current_version = Some(revision.version_id);
        record.replica_observations.clear();
        record.replicas = fs::availability::ReplicaEvidence::default();
        record.freshness.replicas_observed_at_ms = None;
    }
    registry.put(record)?;
    Ok(())
}

async fn refresh_host_replica_observation(
    client: &RecordProtocolClient,
    link: &FileShareLinkV2,
    revision: fs_protocol::ContentRevisionV1,
    availability: Option<&Arc<Mutex<fs::AvailabilityRegistry>>>,
    local_device: collab_types::Ed25519PublicKey,
) -> Result<()> {
    let Some(availability) = availability else {
        return Ok(());
    };
    let request = fs::ReplicaAckRequestV1::new(link.invite.workspace_id, revision)?;
    let observation = client
        .request_bearer_link_replica_ack(link, request)
        .await?;
    let observed_at_ms = observation.observed_at_ms;
    let mut registry = availability.lock_recover();
    registry.observe_replica(
        revision.record_id,
        local_device,
        observation,
        observed_at_ms,
    )?;
    drop(registry);
    Ok(())
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "The namespace guard covers record creation, signed revision and thumbnail projection, incremental save, and exact-share scope validation."
)] // The complete projection is namespace-atomic.
fn apply_shared_snapshot(
    namespace: &Arc<Mutex<Namespace>>,
    history: &FileHistoryStore,
    snapshot: &RecordProtocolSnapshotV1,
) -> Result<()> {
    let metadata = snapshot
        .metadata
        .as_ref()
        .context("shared record has no metadata projection")?;
    metadata.validate()?;
    if metadata.record_id != snapshot.record_id {
        bail!("shared metadata belongs to another record");
    }
    let revision = snapshot
        .content_revision
        .as_ref()
        .context("shared record has no signed content revision")?;
    revision.validate()?;
    let version = snapshot
        .versions
        .iter()
        .find(|version| version.id == revision.version_id)
        .context("shared content revision names an unknown version")?;
    if revision.record_id != snapshot.record_id
        || version.record_id != snapshot.record_id
        || version.content_hash != revision.content_hash
        || version.metadata_hash != revision.metadata_hash
        || metadata.content_hash != revision.content_hash
        || metadata.byte_len != revision.byte_len
    {
        bail!("shared content revision does not match signed record state");
    }
    // Persist immutable history before exposing the namespace pointer. A
    // crash may leave an unprojected version, but never a projected revision
    // whose referenced history node is absent locally.
    append_snapshot_history(history, snapshot)?;
    {
        let mut namespace = namespace.lock_recover();
        namespace.enable_signed_content_authority();
        let ino = match namespace.ino_for_record_id(snapshot.record_id) {
            Some(ino) => ino,
            None => namespace
                .create_projected_file(
                    fs::ROOT_INO,
                    shared_record_name(snapshot.record_id, metadata.mime.as_deref()),
                    snapshot.record_id,
                    None,
                    metadata.byte_len,
                    metadata.observed_at_ms,
                )
                .context("could not project shared record")?,
        };
        let updated = namespace.set_content_revision(ino, revision, metadata.observed_at_ms);
        if !updated {
            bail!("could not update projected shared record");
        }
        if let Some(preview) = metadata
            .previews
            .iter()
            .filter(|preview| preview.status == fs_protocol::PreviewStatus::Ready)
            .filter_map(|preview| preview.hash.map(|hash| (preview.max_edge, hash)))
            .max_by_key(|(max_edge, _)| *max_edge)
        {
            let _ = namespace.install_verified_thumbnail(
                snapshot.record_id,
                Some(iroh_blobs::Hash::from((preview.1).0)),
            );
        }
        namespace.save_incremental();
        validate_shared_namespace(&mut namespace, snapshot.record_id, false)?;
    }
    Ok(())
}

/// A recipient mount is an exact-record capability, so its persisted namespace
/// must never widen that capability. Refuse stale or tampered cache state rather
/// than exposing an unrelated file through the read-only FUSE mount.
fn validate_shared_namespace(
    namespace: &mut Namespace,
    expected_record: fs_protocol::FileRecordId,
    allow_empty: bool,
) -> Result<()> {
    let mut entries = namespace
        .readdir(ROOT_INO)
        .context("shared-record cache has no readable root")?
        .into_iter()
        .filter(|(_, _, name)| name != "." && name != "..");
    let first = entries.next();
    if first.is_none() && allow_empty {
        return Ok(());
    }
    let Some((ino, kind, _name)) = first else {
        bail!("shared-record cache is missing its exact record");
    };
    if entries.next().is_some()
        || kind != fuser::FileType::RegularFile
        || namespace.record_id_for(ino) != Some(expected_record)
    {
        bail!("shared-record cache contains data outside the granted record");
    }
    Ok(())
}

fn append_snapshot_history(
    history: &FileHistoryStore,
    snapshot: &RecordProtocolSnapshotV1,
) -> Result<()> {
    use std::collections::BTreeSet;
    let mut known: BTreeSet<_> = history
        .snapshot(snapshot.record_id)
        .map_or_else(Vec::new, |state| state.versions)
        .into_iter()
        .map(|version| version.id)
        .collect();
    let mut pending = snapshot.versions.clone();
    while !pending.is_empty() {
        let before = pending.len();
        let mut deferred = Vec::new();
        for version in pending {
            version.validate()?;
            if version.record_id != snapshot.record_id {
                bail!("shared history contains another record");
            }
            if version.parents.iter().all(|parent| known.contains(parent)) {
                history.append(version.clone())?;
                known.insert(version.id);
            } else {
                deferred.push(version);
            }
        }
        if deferred.len() == before {
            bail!("shared history contains missing or cyclic parents");
        }
        pending = deferred;
    }
    Ok(())
}

fn shared_record_name(record_id: fs_protocol::FileRecordId, mime: Option<&str>) -> String {
    let extension = match mime.unwrap_or_default() {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/webp" => ".webp",
        "application/pdf" => ".pdf",
        "text/plain" => ".txt",
        "text/markdown" => ".md",
        _ => "",
    };
    let compact = record_id.0.simple().to_string();
    format!(
        "Shared-{}{}",
        compact.get(..12).unwrap_or(&compact),
        extension
    )
}

fn namespace_path(cache_dir: Option<&Path>) -> Option<PathBuf> {
    cache_dir.map(|d| d.join("namespace.bin"))
}

/// Where host-generated thumbnails are cached (`thumbs/` under the content cache); `None` for an in-memory mount.
fn thumb_path(cache_dir: Option<&Path>) -> Option<PathBuf> {
    cache_dir.map(|d| d.join("thumbs"))
}

fn share_control_path(cache_dir: Option<&Path>) -> Option<PathBuf> {
    cache_dir.map(|dir| dir.join("share-control.sock"))
}

fn open_availability(
    cache_dir: Option<&Path>,
) -> Result<Option<Arc<Mutex<fs::AvailabilityRegistry>>>> {
    cache_dir
        .map(|dir| fs::AvailabilityRegistry::open(dir.join("availability.json")))
        .transpose()
        .map(|registry| registry.map(|registry| Arc::new(Mutex::new(registry))))
        .map_err(Into::into)
}

fn open_eviction_queue(
    cache_dir: Option<&Path>,
) -> Result<Option<Arc<tokio::sync::Mutex<fs::eviction::EvictionQueue>>>> {
    cache_dir
        .map(|dir| fs::eviction::EvictionQueue::open(dir.join("evictions.json")))
        .transpose()
        .map(|queue| queue.map(|queue| Arc::new(tokio::sync::Mutex::new(queue))))
        .map_err(Into::into)
}

fn open_share_authority(cache_dir: Option<&Path>) -> Result<Option<Arc<ShareAuthority>>> {
    cache_dir
        .map(|dir| ShareAuthority::open(dir.join("share-authority.json")))
        .transpose()
        .map(|authority| authority.map(Arc::new))
        .map_err(Into::into)
}

fn open_record_services(
    cache_dir: Option<&Path>,
    authority: Option<Arc<ShareAuthority>>,
) -> Result<Option<RecordServices>> {
    let (Some(cache_dir), Some(authority)) = (cache_dir, authority) else {
        return Ok(None);
    };
    let repository = Arc::new(RecordRepository::open(cache_dir.join("records"))?);
    let access_registry = Arc::new(RecordAccessRegistry::new(
        fs::MAX_RECORD_ACCESS_ENTRIES,
        fs::MAX_RECORD_ACCESS_TTL_MS,
    )?);
    Ok(Some(RecordServices {
        authority,
        repository,
        access_registry,
        mutation_sink: None,
        materializer: None,
        availability: None,
        workspace_authority: None,
    }))
}

fn open_history(cache_dir: Option<&Path>) -> Result<Option<Arc<FileHistoryStore>>> {
    cache_dir
        .map(|dir| FileHistoryStore::open(dir.join("file-history.json")))
        .transpose()
        .map(|history| history.map(Arc::new))
        .map_err(Into::into)
}

/// Atomically persist the namespace document and retain bounded known-good
/// generations. A damaged primary is never overwritten by the live daemon.
fn persist_now(namespace: &Arc<Mutex<Namespace>>, path: &Path) {
    if let Err(error) = persist_namespace_generation(namespace, path) {
        eprintln!("namespace persist to {} failed: {error}", path.display());
    }
}

/// Atomically persist one exact namespace generation and return its digest.
/// A full-replica readiness proof is only published after this succeeds, so a
/// ready NAS can always reopen the same namespace after a power loss.
#[allow(
    clippy::significant_drop_tightening,
    reason = "The namespace lock must cover durable replacement so concurrent writers cannot reuse the process-scoped native::atomic temporary path."
)]
fn persist_namespace_generation(
    namespace: &Arc<Mutex<Namespace>>,
    path: &Path,
) -> Result<[u8; 32]> {
    // Keep the namespace lock through the durable replacement. Transactional
    // projections use the same lock, preventing two same-process writers from
    // sharing native::atomic's process-scoped temporary pathname.
    let mut namespace = namespace.lock_recover();
    let bytes = namespace.save();
    let digest = *blake3::hash(&bytes).as_bytes();
    // Bytes this process produced with `save()` (now, or at its last write
    // of this path) are a valid document by construction; parsing them
    // again costs a full document load per save, under the namespace lock.
    // Anything else found on disk is still fully validated.
    let last = persisted_generations()
        .lock_recover()
        .get(path)
        .map(|generation| generation.digest);
    fs::durable_backup::write(path, &bytes, |candidate| {
        let candidate_digest = *blake3::hash(candidate).as_bytes();
        candidate_digest == digest
            || Some(candidate_digest) == last
            || Namespace::from_bytes("namespace-backup-validator", candidate).is_ok()
    })?;
    // After `save()`, which may have completed record ids.
    let heads = namespace.heads();
    persisted_generations()
        .lock_recover()
        .insert(path.to_path_buf(), PersistedGeneration { digest, heads });
    Ok(digest)
}

/// Persist the namespace at `path` unless the last document this process
/// wrote there already holds it: the same heads mean the same document. An
/// idle namespace costs a heads comparison, not a full save, which is what
/// lets every `fsync` ask for it.
fn persist_namespace_changes(namespace: &Arc<Mutex<Namespace>>, path: &Path) -> Result<()> {
    let heads = namespace.lock_recover().heads();
    let written = persisted_generations()
        .lock_recover()
        .get(path)
        .is_some_and(|generation| generation.heads == heads);
    if written {
        return Ok(());
    }
    persist_namespace_generation(namespace, path).map(drop)
}

/// The namespace document this process last wrote to one path.
struct PersistedGeneration {
    digest: [u8; 32],
    heads: Vec<automerge::ChangeHash>,
}

/// What this process last wrote per namespace path.
fn persisted_generations() -> &'static Mutex<std::collections::HashMap<PathBuf, PersistedGeneration>>
{
    static WRITTEN: std::sync::OnceLock<
        Mutex<std::collections::HashMap<PathBuf, PersistedGeneration>>,
    > = std::sync::OnceLock::new();
    WRITTEN.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn load_namespace(path: &Path, author: String) -> Result<Namespace> {
    let outcome = fs::durable_backup::read_or_recover(path, |bytes| {
        Namespace::from_bytes("namespace-backup-validator", bytes).is_ok()
    })?
    .with_context(|| format!("namespace disappeared while opening {}", path.display()))?;
    report_namespace_recovery(path, outcome.recovery.as_ref());
    let mut namespace = Namespace::from_bytes(author, &outcome.bytes)?;
    // A document recovered from an older generation resumes no exchange:
    // what peers agreed with it may be beyond what it now holds.
    if outcome.recovery.is_none() {
        restore_peer_sync_states(&mut namespace, &peer_sync_path(path));
    }
    Ok(namespace)
}

/// Where the sync progress with each peer is kept, beside the namespace
/// document at `namespace_path` and with the same permissions.
fn peer_sync_path(namespace_path: &Path) -> PathBuf {
    namespace_path.with_file_name("peer-sync.bin")
}

/// Resumes the sync progress persisted at `path`, if any. Progress that
/// cannot be read or decoded is discarded whole: every exchange then
/// starts with a full sync, as after a first contact.
fn restore_peer_sync_states(namespace: &mut Namespace, path: &Path) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!("ignoring sync progress at {}: {error}", path.display());
            return;
        }
    };
    if let Err(error) = namespace.restore_peer_sync_states(&bytes) {
        eprintln!(
            "ignoring sync progress at {}: {error:#}; peers sync in full",
            path.display()
        );
    }
}

/// Writes the sync progress beside the namespace document, returning
/// whether it did. Encoded under the namespace lock, written outside it.
fn persist_peer_sync_states(namespace: &Arc<Mutex<Namespace>>, path: &Path) -> bool {
    let bytes = namespace.lock_recover().encode_peer_sync_states();
    if let Err(error) = tilo_files::native::atomic::write(path, &bytes) {
        eprintln!(
            "sync progress persist to {} failed: {error}",
            path.display()
        );
        return false;
    }
    true
}

fn report_namespace_recovery(path: &Path, recovery: Option<&fs::durable_backup::RecoveryEvidence>) {
    if let Some(recovery) = recovery {
        eprintln!(
            "RECOVERED damaged namespace {} from {}; preserved evidence at {}; receipt at {}",
            path.display(),
            recovery.backup_path.display(),
            recovery.damaged_evidence_path.display(),
            recovery.receipt_path.display()
        );
    }
}

/// Persists the namespace whenever it changes and settles
/// ([`fs::namespace_persist`]): the document when its heads moved, the sync
/// progress beside it when that moved, in that order. Nothing runs while
/// the namespace is idle. Progress is skipped while the document's write
/// fails, so it never runs ahead of the document on disk (progress naming
/// heads the document lacks is dropped at load anyway; this only saves the
/// write).
fn spawn_persist(namespace: Arc<Mutex<Namespace>>, path: PathBuf) -> tokio::task::JoinHandle<()> {
    let progress_path = peer_sync_path(&path);
    fs::namespace_persist::spawn_persister(namespace, move |namespace, pending| {
        let mut failed = fs::namespace_persist::Pending::default();
        // A `fsync` may have written this document already.
        if pending.document
            && let Err(error) = persist_namespace_changes(namespace, &path)
        {
            eprintln!("namespace persist to {} failed: {error}", path.display());
            failed.document = true;
        }
        if pending.progress {
            failed.progress =
                failed.document || !persist_peer_sync_states(namespace, &progress_path);
        }
        failed
    })
}

/// First retry of a named-share delivery whose recipient was unreachable.
const NAMED_DELIVERY_RETRY: Duration = Duration::from_secs(5);
/// Longest wait between retries of an undelivered named share.
const NAMED_DELIVERY_RETRY_MAX: Duration = Duration::from_secs(60);
/// The recipient service's restart pass right after it started a mount.
const NAMED_MOUNT_REPAIR: Duration = Duration::from_secs(2);
/// Longest wait between restart passes while every named mount runs.
const NAMED_MOUNT_REPAIR_MAX: Duration = Duration::from_secs(60);

/// The last write of a mount, on shutdown and unmount: stops its persister
/// (waiting, so the two never write at once), then writes the document and
/// the sync progress as they are now.
async fn final_flush(
    persister: Option<tokio::task::JoinHandle<()>>,
    namespace: &Arc<Mutex<Namespace>>,
    path: Option<&Path>,
) {
    if let Some(persister) = persister {
        persister.abort();
        let _ = persister.await;
    }
    if let Some(path) = path {
        persist_now(namespace, path);
        persist_peer_sync_states(namespace, &peer_sync_path(path));
    }
}

/// Open a joiner's namespace: load any persisted copy, then merge the
/// host's doc if reachable. Only a document bound to `workspace_id` by a
/// peer `policy` lets read the whole workspace is merged
/// ([`sync::fetch_remote_doc`]).
async fn open_namespace_mount(
    author: String,
    np: Option<&Path>,
    endpoint: &iroh::endpoint::Endpoint,
    bootstrap: &[iroh_base::EndpointAddr],
    identity: &FsTransportIdentity,
    (workspace_id, policy): (uuid::Uuid, &sync::PeerPolicy),
) -> Result<Namespace> {
    let persisted = match np.as_ref().filter(|p| p.exists()) {
        Some(path) => Some(load_namespace(path, author.clone())?),
        None => None,
    };

    let mut last_error = None;
    for host in bootstrap {
        match sync::fetch_remote_doc(endpoint, host.clone(), identity, workspace_id, policy).await {
            Ok(doc) => {
                return match persisted {
                    Some(mut ns) => {
                        ns.merge_remote_and_resolve(&doc)?;
                        Ok(ns)
                    }
                    None => Ok(Namespace::from_bytes(author, &doc)?),
                };
            }
            Err(error) => last_error = Some(error),
        }
    }
    match persisted {
        Some(ns) => {
            if let Some(error) = last_error {
                eprintln!(
                    "bootstrap peers unreachable ({error}); starting from persisted workspace"
                );
            }
            Ok(ns)
        }
        None => Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no bootstrap peers configured"))),
    }
}

/// FUSE worker threads: `TILO_FS_FUSE_THREADS` when set, else one per CPU
/// between 2 and 16.
fn fuse_worker_threads() -> usize {
    std::env::var("TILO_FS_FUSE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(4, std::num::NonZeroUsize::get)
                .clamp(2, 16)
        })
}

/// Where and how [`run_mount`] mounts a workspace: the workspace its history
/// is checked against, the mount point and cache root, optional thumbnail,
/// share-control and storage-accounting endpoints, and the data-plane choice.
struct MountConfig {
    checked_history_workspace: uuid::Uuid,
    mount_point: PathBuf,
    /// Where the namespace is persisted, if anywhere: `fsync`, `fsyncdir`
    /// and native changesets write it there before they return.
    namespace_path: Option<PathBuf>,
    predictive_cache_root: Option<PathBuf>,
    thumb_dir: Option<PathBuf>,
    share_control_path: Option<PathBuf>,
    allow_legacy_record_migration: bool,
    storage_sock: Option<PathBuf>,
    read_only: bool,
    signal_recipient_ready: bool,
    data_plane: Option<(DataPlaneArgs, [u8; 32])>,
}

/// The live services [`run_mount`] attaches to the filesystem before it goes
/// live, plus the receiver that asks a managed mount to rebind.
struct MountServices {
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sync: Option<Arc<SyncHandle>>,
    acl: Option<(AclHandle, IdentityKey)>,
    availability: Option<Arc<Mutex<fs::AvailabilityRegistry>>>,
    eviction_queue: Option<Arc<tokio::sync::Mutex<fs::eviction::EvictionQueue>>>,
    share_authority: Option<Arc<ShareAuthority>>,
    history: Option<(Arc<FileHistoryStore>, fs_protocol::FileActor)>,
    record_publisher: Option<Arc<RecordPublisher>>,
    replacement_transactions: Option<(Arc<Mutex<fs::RecordTransactionJournal>>, PathBuf)>,
    #[cfg(target_os = "linux")]
    external_history: Option<Arc<fs::external_history::ExternalHistoryFactory>>,
    rebind: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
}

/// Mount the FUSE filesystem and run until interrupted (or the mount exits).
#[allow(
    clippy::too_many_lines,
    reason = "Mount setup wires independent sync, ACL, storage, history, sharing, recovery, and readiness services before supervising FUSE shutdown."
)]
async fn run_mount(config: MountConfig, services: MountServices) -> Result<MountOutcome> {
    let MountConfig {
        checked_history_workspace,
        mount_point,
        namespace_path: durable_namespace_path,
        predictive_cache_root,
        thumb_dir,
        share_control_path,
        allow_legacy_record_migration,
        storage_sock,
        read_only,
        signal_recipient_ready,
        data_plane,
    } = config;
    let MountServices {
        namespace,
        hydrator,
        sync,
        acl,
        availability,
        eviction_queue,
        share_authority,
        history,
        record_publisher,
        replacement_transactions,
        #[cfg(target_os = "linux")]
        external_history,
        mut rebind,
    } = services;
    // Before anything is served: from here on a stop signal ends the mount
    // through the final save instead of killing the process.
    let mut stop = StopSignals::install()?;
    if !mount_point.exists() {
        std::fs::create_dir_all(&mount_point)?;
    }

    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    // The person's prefetch switch and cache budget, applied before the
    // mount is served.
    let (cache_policies, kept_policy) = fs::cache_policy::CachePolicies::load(
        predictive_cache_root
            .as_ref()
            .map(|root| root.join(fs::cache_policy::CACHE_POLICY_FILE)),
        Arc::clone(&heuristics),
        Arc::clone(hydrator.store.files()),
    );
    if let Err(error) = kept_policy {
        eprintln!("ignoring the kept cache policy: {error:#}");
    }
    let cache_policies = Arc::new(cache_policies);
    let predictive_policy_task =
        fs::smart_monitor::start(Arc::clone(&heuristics), predictive_cache_root.clone()).await;
    // Settings' socket receives these same durable services below; it must
    // never fall back to Hydrator-only proof or direct blob deletion.
    let storage_availability = availability.clone();
    let storage_eviction_queue = eviction_queue.clone();
    let storage_history = history.as_ref().map(|(history, _)| Arc::clone(history));
    let durable_namespace = Arc::clone(&namespace);
    #[cfg(target_os = "linux")]
    let signalled_namespace = Arc::clone(&namespace);
    #[cfg(target_os = "linux")]
    let signalled_availability = availability.clone();
    let fs = QuickFS::new(namespace, hydrator, heuristics)
        .with_checked_history_workspace(checked_history_workspace)
        .with_document_content(tilo_content_device::for_app());
    let fs = if allow_legacy_record_migration {
        fs
    } else {
        fs.without_legacy_record_migration()
    };
    let native_sync = sync.clone();
    let fs = match sync {
        Some(sync) => fs.with_sync(sync),
        None => fs,
    };
    let fs = match availability {
        Some(registry) => fs.with_availability(registry),
        None => fs,
    };
    let fs = match eviction_queue {
        Some(queue) => fs.with_eviction_queue(queue),
        None => fs,
    };
    let fs = match share_authority {
        Some(authority) => fs.with_share_authority(authority),
        None => fs,
    };
    let fs = match history {
        Some((history, actor)) => fs.with_history(history, actor)?,
        None => fs,
    };
    let fs = match record_publisher {
        Some(publisher) => fs.with_record_publisher(publisher)?,
        None => fs,
    };
    let fs = match replacement_transactions {
        Some((journal, namespace_path)) => {
            fs.with_replacement_transactions(journal, namespace_path)?
        }
        None => fs,
    };
    let fs = match share_control_path {
        Some(path) => fs.with_share_control_path(path),
        None => fs,
    };
    // Bind the capability list, if given, before the mount goes live, so every op is checked from the start.
    let fs = match acl {
        Some((acl, identity)) => fs.with_acl(acl, identity),
        None => fs,
    };
    // Durable pins become an active residency promise only after the live ACL
    // is attached, so inherited folder hydration cannot cross the mount view.
    let fs = fs.with_pinned_hydration();
    // Host-side thumbnails: generate previews as metadata, served via the `user.tilo.thumb` xattr.
    let fs = match thumb_dir {
        Some(dir) => match fs::ThumbProvider::open(&dir) {
            Ok(provider) => fs.with_thumbs(provider),
            Err(e) => {
                eprintln!(
                    "thumbnail cache unavailable at {} ({e}); previews disabled",
                    dir.display()
                );
                fs
            }
        },
        None => fs,
    };
    // What this device learned about files opened together survives the
    // restart, per workspace, in the cache directory.
    let fs = match predictive_cache_root.as_ref() {
        Some(root) => fs.with_kept_prefetch_history(root, checked_history_workspace),
        None => fs,
    };
    fs.resume_pending_evictions();
    // Bytes accepted by fsync, or whose commit failed, are staged per
    // workspace so they survive a crash or unmount and are replayed here.
    let unsaved_dir = predictive_cache_root
        .as_ref()
        .filter(|_| !read_only)
        .map(|root| {
            root.join("unsaved-writes")
                .join(checked_history_workspace.to_string())
        });
    let fs = match unsaved_dir {
        Some(dir) => match fs::unsaved::UnsavedStore::open(&dir) {
            Ok(store) => {
                let fs = fs.with_unsaved_store(store);
                let placed = fs.recover_unsaved_writes();
                if placed > 0 {
                    println!("Recovered {placed} staged unsaved file(s)");
                }
                fs
            }
            Err(error) => {
                eprintln!(
                    "unsaved-write staging unavailable at {} ({error}); fsync commits directly",
                    dir.display()
                );
                fs
            }
        },
        None => fs,
    };

    // Written bytes of open files are staged in sparse files beside the
    // cache rather than in memory, so a large write never has to fit in RAM.
    let fs = match predictive_cache_root.as_ref().filter(|_| !read_only) {
        Some(root) => fs.with_write_stage_dir(
            root.join("write-stage")
                .join(checked_history_workspace.to_string()),
        ),
        None => fs,
    };

    let storage_authority = storage_sock.as_ref().map(|_| {
        let authority = fs::storage::StorageAuthority::new(
            Arc::clone(&fs.namespace),
            Arc::clone(&fs.hydrator),
            fs.write_buffers_handle(),
            storage_availability,
            storage_eviction_queue,
            storage_history,
            // The name the person's devices tell this workspace by: its
            // mount's directory, or its id where the mount has none.
            mount_point
                .file_name()
                .and_then(|name| name.to_str())
                .map_or_else(|| checked_history_workspace.to_string(), str::to_owned),
        )
        .with_cache_policy(Arc::clone(&cache_policies));
        #[cfg(target_os = "linux")]
        let authority = authority.with_external_history(external_history.clone());
        authority
    });

    let _storage = storage_sock
        .zip(storage_authority)
        .map(|(sock, authority)| {
            let _ = std::fs::remove_file(&sock);
            match tokio::net::UnixListener::bind(&sock) {
                Ok(listener) => {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o660));
                    println!("Storage accounting socket at {}", sock.display());
                    tokio::spawn(fs::storage::serve(listener, authority))
                }
                Err(error) => {
                    eprintln!("could not bind storage socket {}: {error}", sock.display());
                    tokio::spawn(async {})
                }
            }
        });

    // Both planes: the native plane after each changeset, FUSE on `fsync`
    // and `fsyncdir`.
    if let Some(path) = durable_namespace_path {
        fs.set_durability_hook(Arc::new(move || {
            persist_namespace_changes(&durable_namespace, &path).map_err(std::io::Error::other)
        }));
    }
    #[cfg(target_os = "linux")]
    if let Some((args, device)) = data_plane.as_ref()
        && let Some(cfg) = native_config(
            args,
            *device,
            checked_history_workspace,
            &mount_point,
            predictive_cache_root.as_deref(),
            read_only,
        )?
    {
        let outcome = run_native(fs, cfg, native_sync, rebind, &mut stop).await;
        if let Some(task) = predictive_policy_task {
            task.abort();
        }
        return outcome;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (&data_plane, &native_sync);

    // Kernel passthrough for fully local read-only opens is opt-in while the
    // backing files are a per-open materialisation of the chunk store.
    let fs = match predictive_cache_root
        .as_ref()
        .filter(|_| std::env::var_os("TILO_FS_PASSTHROUGH").is_some())
    {
        Some(root) => fs.with_passthrough(root.join("passthrough")),
        None => fs,
    };
    // Watchers on the mount hear of the changes the kernel did not make
    // (a peer's edit) instead of polling for them. A read-only mount cannot
    // take the signal, so its watchers keep polling.
    #[cfg(target_os = "linux")]
    let change_signal = if read_only {
        None
    } else {
        match fs::change_signal::ChangeSignal::new() {
            Ok(signal) => Some(Arc::new(signal)),
            Err(error) => {
                eprintln!("folders on the mount will not signal their changes: {error}");
                None
            }
        }
    };
    #[cfg(target_os = "linux")]
    let fs = match &change_signal {
        Some(signal) => fs.with_change_signal(Arc::clone(signal)),
        None => fs,
    };

    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("quickfs".to_string())];
    // Several workers, each with its own /dev/fuse descriptor, so one slow
    // request (a first write seeding a large file, a commit) does not stall
    // every other process using the mount.
    config.n_threads = Some(fuse_worker_threads());
    config.clone_fd = true;
    if read_only {
        config.mount_options.push(MountOption::RO);
    }
    // The installed service uses auto-unmount and the system-wide Tilo mount
    // policy. The mandatory process harness runs all peers under one test UID
    // and opts into the kernel's owner-only mode, which does not require
    // mutating host `/etc/fuse.conf`.
    if std::env::var_os("TILO_FS_FUSE_OWNER_ONLY").is_some() {
        config.acl = SessionACL::Owner;
    } else {
        config.acl = SessionACL::All;
        config.mount_options.push(MountOption::AutoUnmount);
    }

    let mp = mount_point.clone();
    let (unmount_tx, session_events) = std::sync::mpsc::channel();
    let session_ended = unmount_tx.clone();
    // Keep the BackgroundSession under a supervisor instead of immediately
    // consuming it in `join()`: a rebind must unmount the old FUSE session and
    // wait for its worker before this process can return to systemd. The
    // supervisor sleeps until one of the two happens.
    let mut mount_task = tokio::task::spawn_blocking(move || {
        let session = fuser::spawn_mount(fs, &mp, &config)?;
        // Held until the session ends: dropping it stops the signals.
        #[cfg(target_os = "linux")]
        let _signals = change_signal.and_then(|signal| {
            signal
                .start(
                    signalled_namespace,
                    signalled_availability,
                    &mp,
                    session.notifier(),
                    &tokio::runtime::Handle::current(),
                )
                .map_err(|error| {
                    eprintln!(
                        "folders on {} will not signal their changes; watchers poll: {error}",
                        mp.display()
                    );
                })
                .ok()
        });
        if signal_recipient_ready {
            use std::io::Write;
            println!("{}", fs::recipient_mount::RECIPIENT_MOUNT_READY_LINE);
            std::io::stdout().flush()?;
        }
        session_supervisor::supervise(session, &session_events, session_ended)
    });
    println!("Mounting QuickFS at {}", mount_point.display());

    // Exit cleanly on a stop signal or mount exit (don't hang a
    // crash-looping service forever).
    let outcome = tokio::select! {
        res = &mut mount_task => {
            match res {
            Ok(Ok(())) => println!("Filesystem unmounted."),
            Ok(Err(e)) => eprintln!("FUSE mount exited with error: {e}"),
            Err(e) => eprintln!("mount task failed: {e}"),
            }
            MountOutcome::Unmounted
        },
        signal = stop.recv() => {
            println!("{signal}, exiting...");
            let _ = unmount_tx.send(session_supervisor::SessionEvent::Unmount);
            let _ = (&mut mount_task).await;
            MountOutcome::Unmounted
        },
        () = async {
            match rebind.as_mut() {
                Some(receiver) => { let _ = receiver.recv().await; }
                None => std::future::pending::<()>().await,
            }
        } => {
            println!("Content peer registration requested a clean workspace rebind.");
            let _ = unmount_tx.send(session_supervisor::SessionEvent::Unmount);
            let _ = (&mut mount_task).await;
            MountOutcome::RebindRequested
        },
    };
    if let Some(task) = predictive_policy_task {
        task.abort();
    }
    Ok(outcome)
}

/// Chooses the data plane and, for Tier 1, its configuration. `None` means
/// the FUSE data plane serves the workspace.
#[cfg(target_os = "linux")]
fn native_config(
    args: &DataPlaneArgs,
    device: [u8; 32],
    workspace: uuid::Uuid,
    mount_point: &Path,
    cache_dir: Option<&Path>,
    read_only: bool,
) -> Result<Option<fs::native::NativeConfig>> {
    use fs::native::DataPlane;
    let choice = match args.data_plane {
        DataPlaneChoice::Auto => DataPlane::Auto,
        DataPlaneChoice::Fuse => DataPlane::Fuse,
        DataPlaneChoice::Native => DataPlane::Native,
    };
    let selection = match (cache_dir, read_only) {
        (_, true) => fs::native::TierSelection {
            native: false,
            store: args.store.clone(),
            reasons: vec!["read-only mounts are served by FUSE".into()],
            report: None,
            probe_time: Duration::ZERO,
        },
        (None, _) => fs::native::TierSelection {
            native: false,
            store: args.store.clone(),
            reasons: vec!["no cache directory to keep the native plane's state in".into()],
            report: None,
            probe_time: Duration::ZERO,
        },
        (Some(_), false) => fs::native::tier::select(choice, &args.store),
    };
    println!("{}", selection.summary());
    if let Some(report) = &selection.report {
        eprint!("{report}");
    }
    if !selection.native {
        if choice == DataPlane::Native {
            bail!(
                "the native data plane was requested but is unavailable: {}",
                selection.reasons.join("; ")
            );
        }
        return Ok(None);
    }
    let Some(cache_dir) = cache_dir else {
        return Ok(None);
    };
    let mut cfg = fs::native::NativeConfig::new(
        args.store.clone(),
        workspace.to_string(),
        mount_point.to_path_buf(),
        cache_dir.join("native"),
        device,
    );
    cfg.owner = args.native_owner.as_deref().map(parse_owner).transpose()?;
    cfg.eager_bytes = args.native_eager_bytes;
    cfg.lease_authority = args.lease_authority;
    Ok(Some(cfg))
}

/// Serves the workspace from the native tree until interrupted, terminated
/// or asked to rebind. The bind mount lives exactly as long as this call.
#[cfg(target_os = "linux")]
async fn run_native(
    fs: QuickFS,
    cfg: fs::native::NativeConfig,
    sync: Option<Arc<SyncHandle>>,
    mut rebind: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
    stop: &mut StopSignals,
) -> Result<MountOutcome> {
    let fs = Arc::new(fs);
    let content = Arc::clone(&fs.hydrator);
    let hydrator = Arc::clone(&fs.hydrator);
    let runtime = tokio::runtime::Handle::current();
    let plane = tokio::task::spawn_blocking(move || {
        fs::native::NativePlane::start(cfg, fs, content, sync, runtime)
    })
    .await??;
    // Every way out is an event: nothing here wakes until one happens.
    let outcome = tokio::select! {
        signal = stop.recv() => {
            println!("{signal}, exiting...");
            MountOutcome::Unmounted
        }
        () = async {
            match rebind.as_mut() {
                Some(receiver) => { let _ = receiver.recv().await; }
                None => std::future::pending::<()>().await,
            }
        } => {
            println!("Content peer registration requested a clean workspace rebind.");
            MountOutcome::RebindRequested
        }
        () = plane.finished() => {
            eprintln!("the native data plane stopped");
            MountOutcome::Unmounted
        }
    };
    tokio::task::spawn_blocking(move || plane.stop()).await??;
    let (ranged, blobs) = hydrator.received_bytes();
    println!("Content received from peers: {ranged} bytes in ranges, {blobs} bytes as blobs");
    println!("Workspace unmounted.");
    Ok(outcome)
}

/// The signals that end a mount cleanly: SIGINT from a terminal, SIGTERM
/// from systemd's stop (every shutdown, update reboot and service restart),
/// SIGHUP when the controlling terminal goes away. The default action of
/// each kills the process at once, before the caller's final namespace save,
/// so a write acknowledged in the last [`fs::namespace_persist::QUIET`] was
/// lost. A handler, once installed, stays installed for the life of the
/// process: a signal that arrives after the mount already ended (an external
/// unmount, then systemd's stop) no longer cuts that save short.
struct StopSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// Waits for the first stop signal since [`Self::install`] and names it.
    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "Interrupted",
            _ = self.terminate.recv() => "Terminated",
            _ = self.hangup.recv() => "Hung up",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MountOutcome {
    Unmounted,
    RebindRequested,
}

/// Recursively ingest a directory's contents into the workspace.
async fn seed_from_dir(
    namespace: &Arc<Mutex<Namespace>>,
    store: &Arc<ContentStore>,
    root: &Path,
) -> Result<()> {
    let document_content = tilo_content_device::for_app();
    let mut stack = vec![(ROOT_INO, root.to_path_buf())];
    while let Some((parent_ino, dir)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                let ino = namespace
                    .lock_recover()
                    .create_dir(parent_ino, &name, unix_now_ms())
                    .ok_or_else(|| {
                        anyhow::anyhow!("seeding {name:?}: parent inode {parent_ino} vanished")
                    })?;
                stack.push((ino, path));
            } else if ft.is_file() {
                let data = std::fs::read(&path)?;
                let codec = fs::fs::document_codec_for(&name);
                if let Some(c) = codec {
                    // Recognized rich-text format → seed as a CRDT document.
                    let mut ns = namespace.lock_recover();
                    if let Some(ino) = ns.create_document(parent_ino, &name, &c, unix_now_ms()) {
                        ns.import_document(ino, &data, unix_now_ms(), &document_content);
                    }
                } else {
                    let size = data.len() as u64;
                    let hash = store.write_content(&data).await?;
                    namespace.lock_recover().create_file(
                        parent_ino,
                        &name,
                        hash.as_ref(),
                        size,
                        unix_now_ms(),
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files that ship in the OS image, addressed from this crate.
    fn os_file(rel: &str) -> PathBuf {
        // This crate lives in crates/system/fs; shipped image inputs remain
        // at the workspace root rather than alongside the relocated crate.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(rel)
    }

    /// The `ExecStart=` argv from the shipped unit, so the test proves the shipped image runs, not a copy.
    fn service_exec_start() -> Vec<String> {
        let unit = std::fs::read_to_string(os_file("os/usr/lib/systemd/system/fs.service"))
            .expect("the shipped fs.service must exist");
        let mut argv: Vec<String> = Vec::new();
        let mut lines = unit.lines();
        while let Some(line) = lines.next() {
            let Some(rest) = line.strip_prefix("ExecStart=") else {
                continue;
            };
            let mut chunk = rest.to_string();
            // systemd folds a trailing backslash onto the next line.
            while let Some(head) = chunk.strip_suffix('\\') {
                let next = lines.next().expect("continuation line");
                chunk = format!("{head} {}", next.trim());
            }
            argv = chunk.split_whitespace().map(str::to_string).collect();
            break;
        }
        assert!(!argv.is_empty(), "fs.service has no ExecStart=");
        // systemd splits an unquoted `$VAR` into words; resolve it from the
        // unit's own `Environment=` default, which is what a boot without an
        // override file runs.
        let defaults: std::collections::HashMap<&str, &str> = unit
            .lines()
            .filter_map(|line| line.strip_prefix("Environment="))
            .filter_map(|assignment| assignment.split_once('='))
            .collect();
        argv.into_iter()
            .flat_map(|word| match word.strip_prefix('$') {
                Some(name) => defaults
                    .get(name)
                    .unwrap_or_else(|| panic!("fs.service uses ${name} without a default"))
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                None => vec![word],
            })
            .collect()
    }

    #[test]
    fn host_files_unit_runs_the_primary_launch_check() {
        // The host package's user unit, so a renamed flag fails here, not at login.
        let unit = std::fs::read_to_string(os_file("dist/session/tilo-files-workspace.service"))
            .expect("the host Files unit must exist");
        let exec: Vec<&str> = unit
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .expect("ExecStart=")
            .split_whitespace()
            .collect();
        assert_eq!(
            exec[0], "/usr/bin/fs",
            "unexpected ExecStart binary: {exec:?}"
        );
        let cli = Cli::try_parse_from(std::iter::once("fs").chain(exec[1..].iter().copied()))
            .expect("the host Files unit command line must parse");
        assert!(
            matches!(cli.command, Command::ServePrimary(_)),
            "the host unit must pass the saved-state and authority check before serving"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn shipped_units_serve_with_a_seeded_enforced_capability_list() {
        {
            let unit = std::fs::read_to_string(os_file("os/usr/lib/systemd/system/fs.service"))
                .expect("the shipped fs.service must exist");
            assert!(
                unit.lines().any(|line| line.trim() == "Restart=always"),
                "a daemon-owned rebind must be restarted by the shipped service"
            );
            assert!(
                service_exec_start()
                    .windows(2)
                    .any(|pair| { pair[0].ends_with("fs") && pair[1] == "serve" }),
                "the shipped unit must enter the serve startup path that consumes persisted tickets"
            );
        }
        {
            // The exact line fs.service runs, so a bad flag fails here, not on boot.
            let argv = service_exec_start();
            assert_eq!(
                argv[0], "/usr/bin/fs",
                "unexpected ExecStart binary: {argv:?}"
            );
            let cli =
                Cli::try_parse_from(std::iter::once("fs".to_string()).chain(argv[1..].to_vec()))
                    .expect("fs.service command line must parse");
            match cli.command {
                Command::Serve {
                    mount_point,
                    cache_dir,
                    seed,
                    ..
                } => {
                    assert_eq!(mount_point, PathBuf::from("/var/workspace"));
                    assert_eq!(cache_dir, Some(PathBuf::from("/var/cache/quick-fs")));
                    assert!(seed.is_none());
                }
                _ => panic!("expected serve subcommand"),
            }
        }
        // Phase `the_shipped_service_enforces_a_capability_list`.
        // An enforcement point shipped switched off is not an enforcement point, so the unit is asserted on directly.
        {
            let argv = service_exec_start();
            let cli =
                Cli::try_parse_from(std::iter::once("fs".to_string()).chain(argv[1..].to_vec()))
                    .expect("fs.service command line must parse");
            let Command::Serve { acl, .. } = cli.command else {
                panic!("expected serve");
            };
            assert_eq!(
                acl.acl.as_deref(),
                Some(Path::new("/var/lib/tilo/fs-capabilities.json")),
                "fs.service must pass --acl or the shipped mount enforces nothing"
            );
            assert!(
                acl.acl_reload_secs > 0,
                "the shipped mount must re-read its capability list; /var/workspace is mounted once \
                 at boot, so a list read only at mount time can never carry a later grant"
            );
            assert_eq!(
                acl.principals,
                vec!["person:local".to_string()],
                "the shipped list is written against a principal, because the mount's own identity \
                 is the device key minted on first boot, which no shipped file could name in advance"
            );
        }
        // Phase `tmpfiles_seeds_the_capability_list_the_service_names`.
        // tmpfiles.d must place the file the unit names, or the mount is fail-closed-refused on every fresh boot.
        {
            let conf = std::fs::read_to_string(os_file("os/usr/lib/tmpfiles.d/tilo.conf"))
                .expect("the shipped tmpfiles snippet must exist");
            let seeding = conf
                .lines()
                .find(|l| l.starts_with("C ") && l.contains("/var/lib/tilo/fs-capabilities.json"))
                .unwrap_or_else(|| {
                    panic!("no tmpfiles line creates /var/lib/tilo/fs-capabilities.json:\n{conf}")
                });
            assert!(
                seeding.contains("/usr/share/tilo/fs-capabilities.json"),
                "the seed must come from the shipped default: {seeding}"
            );

            // ...and the image must contain the file tmpfiles copies from, or the seed does nothing and the mount is refused on every boot.
            let containerfile = std::fs::read_to_string(os_file("os/Containerfile"))
                .expect("the production Containerfile must exist");
            assert!(
                containerfile.lines().any(|l| l.starts_with("COPY ")
                    && l.contains("/usr/share/tilo/fs-capabilities.json")),
                "os/Containerfile must ship the default capability list that tmpfiles.d seeds from"
            );
        }
        // Phase `the_shipped_default_list_grants_the_local_session_its_workspace`.
        // ...and the seeded default must be a capability list this binary can parse, or the mount is refused on first boot.
        {
            use fs::Access;

            let path = os_file("os/usr/share/tilo/fs-capabilities.json");
            let identity: IdentityKey = [0x5A; 32]; // stands in for the per-start node key
            let file = CapabilityFile::load(
                &path,
                identity,
                vec![parse_principal("person:local").unwrap()],
            )
            .expect("the shipped default capability list must load");

            let handle = file.handle();
            assert!(
                handle.permits(&identity, "/", Access::Read),
                "the shipped default must let the local session read its own workspace"
            );
            assert!(
                handle.permits(&identity, "/notes/today.md", Access::Write),
                "the shipped default must let the local session write its own workspace"
            );

            // The grant is the principal's, not the mount's — a mount not presenting `person:local` gets nothing, which narrows a peer-joined mount.
            let outsider = CapabilityFile::load(&path, identity, Vec::new())
                .expect("loads")
                .handle();
            assert!(
                !outsider.permits(&identity, "/notes/today.md", Access::Read),
                "the shipped list must grant its named principal, not whoever mounts"
            );
        }
    }

    /// tmpfiles' `C` never overwrites, so a machine keeps the capability list
    /// of the image it was installed with across every update. Each version
    /// the image ever seeded must still mount with the grant it gave, through
    /// the same loader fs.service uses. `tools/policy/check_seeded_state_history.py`
    /// requires today's default to be among them.
    #[test]
    fn every_capability_list_an_image_ever_seeded_still_mounts_with_its_grant() {
        use fs::Access;

        let history = os_file("os/tests/seeded-state/var/lib/tilo/fs-capabilities.json");
        let mut versions: Vec<_> = std::fs::read_dir(&history)
            .expect("the seeded capability list history must exist")
            .map(|entry| entry.expect("history entry").path())
            .collect();
        versions.sort();
        assert!(
            versions.len() >= 2,
            "the history must hold the pre-authority list and today's: {versions:?}"
        );
        let identity: IdentityKey = [0x5A; 32];
        for path in &versions {
            let local = vec![parse_principal("person:local").unwrap()];
            let file = CapabilityFile::load(path, identity, local).unwrap_or_else(|error| {
                panic!(
                    "{} must still load on an upgraded machine: {error}",
                    path.display()
                )
            });
            let handle = file.handle();
            assert!(
                handle.permits(&identity, "/notes/today.md", Access::Write),
                "{} must still let the local session write its workspace",
                path.display()
            );
            let outsider = CapabilityFile::load(path, identity, Vec::new())
                .expect("loads")
                .handle();
            assert!(
                !outsider.permits(&identity, "/notes/today.md", Access::Read),
                "{} must grant only its named principal",
                path.display()
            );
        }
    }

    #[test]
    fn cli_parses_mount_lines_principals_and_persisted_tickets() {
        {
            let cli = Cli::try_parse_from([
                "fs",
                "mount",
                "--mount-point",
                "/var/workspace",
                "--ticket",
                "deadbeef",
                "--cache-dir",
                "/var/cache/quick-fs",
            ])
            .expect("mount command line must parse");
            assert!(matches!(cli.command, Command::Mount { .. }));
        }
        {
            assert_eq!(
                parse_principal("room:team").unwrap(),
                Principal::Room("team".into())
            );
            assert_eq!(
                parse_principal("app:org.tilo.files").unwrap(),
                Principal::App("org.tilo.files".into())
            );
            assert!(parse_principal("wizard:merlin").is_err());
            assert!(parse_principal("noseparator").is_err());
        }
        {
            let temp = tempfile::tempdir().unwrap();
            let owner =
                iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x81; 32]).public());
            let nas =
                iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x82; 32]).public());
            let ticket = Ticket {
                node: owner,
                workspace: [0x83; 32],
            }
            .encode()
            .unwrap();
            let peers = vec![fs::workspace_authority::PersistedContentPeer {
                endpoint: nas,
                ticket: Some(ticket),
                relay_service_id: None,
            }];
            tilo_files::native::atomic::write(
                &fs::workspace_authority::content_peers_path(temp.path()),
                &serde_json::to_vec(&peers).unwrap(),
            )
            .unwrap();
            let selected = persisted_remote_workspace_ticket(Some(temp.path()))
                .unwrap()
                .expect("NAS ticket is retained for the next fs.service start");
            assert_eq!(selected.workspace, [0x83; 32]);
            assert_ne!(selected.node.id, peers[0].endpoint.id);
        }
    }

    const NODE_KEY: IdentityKey = [0x77; 32];

    fn acl_args_from(argv: &[&str]) -> AclArgs {
        let mut full = vec!["fs", "serve", "--mount-point", "/var/workspace"];
        full.extend_from_slice(argv);
        let cli = Cli::try_parse_from(full).expect("command line must parse");
        match cli.command {
            Command::Serve { acl, .. } => acl,
            _ => panic!("expected serve"),
        }
    }

    #[test]
    fn acl_flags_bind_an_enforced_list_or_refuse_the_mount() {
        // Phase `without_the_acl_flag_no_capability_list_is_bound`.
        // No `--acl` means Unix-veneer-only behaviour — the documented opt-out, and why fs.service still works unchanged.
        {
            let resolved =
                resolve_acl(&acl_args_from(&[]), NODE_KEY).expect("no ACL is not an error");
            assert!(
                resolved.is_none(),
                "omitting --acl must leave the mount unbound, not bind an empty ACL"
            );
        }
        // Phase `a_missing_capability_file_refuses_the_mount`.
        // Asking for enforcement and not getting it must never be silent.
        {
            let args = acl_args_from(&["--acl", "/nonexistent/tilo-caps-does-not-exist.json"]);
            let err = resolve_acl(&args, NODE_KEY)
                .expect_err("a missing capability list must abort the mount, not fall back");
            assert!(
                format!("{err:#}").contains("could not be read"),
                "expected a read failure, got: {err:#}"
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            std::fs::write(&path, b"{ not json ]").unwrap();
            let args = acl_args_from(&["--acl", path.to_str().unwrap()]);
            let err = resolve_acl(&args, NODE_KEY)
                .expect_err("a malformed capability list must abort the mount");
            assert!(
                format!("{err:#}").contains("not the governor's file authority"),
                "expected a parse failure, got: {err:#}"
            );
        }
        // Phase `an_empty_capability_file_binds_a_denying_acl`.
        // An empty capability list is a valid, total denial — not an error, and not a fallback to unenforced.
        {
            use fs::Access;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            std::fs::write(&path, b"[]").unwrap();
            let args = acl_args_from(&["--acl", path.to_str().unwrap()]);
            let caps = resolve_acl(&args, NODE_KEY)
                .expect("an empty list is valid")
                .expect("--acl was given, so an ACL must be bound");
            let (acl, identity) = (caps.handle(), caps.identity());
            assert_eq!(identity, NODE_KEY, "identity defaults to this node's key");
            assert!(!acl.permits(&identity, "/", Access::Read));
            assert!(!acl.permits(&identity, "/anything", Access::Write));
        }
        {
            use collab_types::{
                Access as CapAccess, CapOverride, Ed25519PublicKey, Effect, Scope, Subject,
            };
            use fs::Access;

            let who = [0x0A; 32];
            let caps = vec![CapOverride::new(
                Subject::User(Ed25519PublicKey(who)),
                Scope::FsPath("/w/docs".into()),
                Effect::Grant,
                CapAccess::Read,
            )];
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            std::fs::write(
                &path,
                serde_json::to_vec(&collab_types::FsAuthority::from(caps)).unwrap(),
            )
            .unwrap();

            let args = acl_args_from(&[
                "--acl",
                path.to_str().unwrap(),
                "--identity",
                &hex::encode(who),
            ]);
            let caps = resolve_acl(&args, NODE_KEY).unwrap().unwrap();
            let (acl, identity) = (caps.handle(), caps.identity());
            assert_eq!(identity, who, "--identity overrides the node key");
            assert!(acl.permits(&who, "/w/docs/a.md", Access::Read));
            assert!(!acl.permits(&who, "/w/docs/a.md", Access::Write));
            assert!(!acl.permits(&who, "/w/other", Access::Read));
        }
        // Phase `identity_without_a_capability_list_is_rejected`.
        // `--identity` alone enforces nothing; accepting it silently would read as identity-scoped when it isn't.
        {
            let args = acl_args_from(&["--identity", &hex::encode([0x0A; 32])]);
            assert!(resolve_acl(&args, NODE_KEY).is_err());
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            std::fs::write(&path, b"[]").unwrap();
            let args = acl_args_from(&["--acl", path.to_str().unwrap(), "--identity", "abcd"]);
            let err = resolve_acl(&args, NODE_KEY).expect_err("a 2-byte identity is not a key");
            assert!(format!("{err:#}").contains("32 bytes"), "got: {err:#}");
        }
    }

    /// systemd stops `fs.service` with SIGTERM; SIGHUP arrives when a
    /// terminal running the daemon closes. Once a mount installed its stop
    /// signals, each of them, like SIGINT, reaches the stop path, which
    /// unmounts and makes the final namespace save, instead of killing the
    /// process before it. (A signal whose default action still applied would
    /// end this test binary here.)
    #[tokio::test]
    async fn stop_signals_reach_the_stop_path_instead_of_killing_the_process() {
        let mut stop = StopSignals::install().unwrap();
        for (signal, name) in [
            (libc::SIGTERM, "Terminated"),
            (libc::SIGHUP, "Hung up"),
            (libc::SIGINT, "Interrupted"),
        ] {
            // SAFETY: raising a signal in this process, whose handler the
            // install above put in place.
            assert_eq!(unsafe { libc::raise(signal) }, 0);
            let received = tokio::time::timeout(tilo_test_env::hang_bound(), stop.recv())
                .await
                .unwrap_or_else(|_| panic!("{name} never reached the stop path"));
            assert_eq!(received, name);
        }
    }
}
