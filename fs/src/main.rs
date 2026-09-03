#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use collab_types::Principal;
use fs::full_replica::{
    HomePeerBundle, HomePeerSet, aggregate_status, hydrate_full_workspace, save_status,
};
use fs::namespace::ROOT_INO;
use fs::sync::{self, SyncHandle};
use fs::{
    AclHandle, CapabilityFile, ContentStore, FileHistoryStore, FileShareLinkV2,
    FsTransportIdentity, Heuristics, Hydrator, IdentityKey, Namespace, QuickFS,
    RecipientControlServer, RecipientMountSupervisor, RecordAccessRegistry, RecordMaterializer,
    RecordProtocolClient, RecordProtocolClientError, RecordProtocolOperationV1,
    RecordProtocolResponseV1, RecordProtocolSnapshotV1, RecordPublisher, RecordRepository,
    RecordServices, RecordSubscriptionFailureCode, RecordSubscriptionKey, RecordSubscriptionStore,
    Refresh, ShareAuthority, ShareControlServer, ShareManagement, Ticket, Workspace,
    identity_paths, load_certified_mount_identity, load_revocation_list,
    load_workspace_share_issuer,
};
use fuser::{Config, MountOption, SessionACL};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(author, version, about = "qOS collaborative filesystem", long_about = None)]
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
    use collab_types::{
        ContentHash, Ed25519PublicKey, FileActor, FileMetadataV1, FileProvenance, FileRecordId,
        FileVersionId, FileVersionV1, IndexStatus, PreviewStatus, PreviewVariant,
    };

    fn actor() -> FileActor {
        FileActor {
            person: Ed25519PublicKey([0x11; 32]),
            device: Ed25519PublicKey([0x22; 32]),
        }
    }

    #[test]
    fn mount_share_accepts_stdin_without_putting_a_bearer_in_argv() {
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

    #[test]
    fn stdin_share_link_is_trimmed_and_strictly_bounded() {
        assert_eq!(
            parse_share_uri_input(b"qos-file://v2/example#secret\n".to_vec(), 64).unwrap(),
            "qos-file://v2/example#secret"
        );
        assert!(parse_share_uri_input(vec![b'x'; 65], 64).is_err());
        assert!(parse_share_uri_input(b"\r\n".to_vec(), 64).is_err());
        assert!(parse_share_uri_input(vec![0xff], 64).is_err());
    }

    #[test]
    fn recipient_service_command_line_is_explicit_and_secret_free() {
        let cli = Cli::try_parse_from([
            "fs",
            "recipient-service",
            "--socket",
            "/run/user/1000/qos/services/qos-fs.sock",
            "--mounts-root",
            "/home/alice/Shared",
            "--cache-root",
            "/home/alice/.cache/qos/received-files",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::RecipientService { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recipient_service_replaces_only_a_verified_stale_socket() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("qos-fs.sock");
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
            content_revision: Some(collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
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
    fn shared_snapshot_projects_one_exact_record_and_updates_it_in_place() {
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
        second.content_revision = Some(collab_types::ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: next,
            content_hash: Some(ContentHash([0x55; 32])),
            byte_len: 456,
            metadata_hash: ContentHash([0x56; 32]),
        });
        apply_shared_snapshot(&namespace, &history, &second).unwrap();
        let guard = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(guard.ino_for_record_id(record_id), Some(ino));
        assert_eq!(guard.getattr(ino).unwrap().size, 456);
        drop(guard);
    }

    #[test]
    fn shared_snapshot_rejects_cross_record_metadata_and_history() {
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

    #[test]
    fn shared_namespace_refuses_any_extra_visible_node() {
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
    /// How often to re-read `--acl`, in seconds (`0` = once, at mount time; default 1).
    #[arg(long, value_name = "SECONDS", default_value_t = 1)]
    acl_reload_secs: u64,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Host a collaborative workspace and mount it locally.
    Serve {
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
        #[command(flatten)]
        acl: AclArgs,
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
        #[command(flatten)]
        acl: AclArgs,
    },
    /// Mount the single exact record addressed by an endpoint-bound share link.
    MountShare {
        /// Where to mount the read-only shared record.
        #[arg(short, long)]
        mount_point: PathBuf,
        /// V2 qos-file URI. The fragment contains a bearer secret; prefer
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
        /// Launcher service socket (`%t/qos/services/qos-fs.sock`).
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
    /// Keep a complete, authenticated workspace replica without mounting FUSE.
    Replica {
        /// A home-peer bundle exported by an existing workspace member.
        #[arg(long)]
        config: PathBuf,
    },
    /// Export the one-file home-peer bundle for an always-on qOS host.
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
        #[arg(long, default_value = "/var/lib/qos/full-replica")]
        replica_cache_dir: PathBuf,
        /// Atomic readiness proof read by qos-relay.
        #[arg(long, default_value = "/var/lib/qos/full-replica/status.json")]
        status_file: PathBuf,
        /// Additional capability principals held by the host identity.
        #[arg(long = "principal")]
        principals: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(path) = &cli.test_clock_file {
        fs::clock::install_test_clock_file(path).context("configuring deterministic test clock")?;
    }
    match cli.command {
        Command::Serve {
            mount_point,
            cache_dir,
            seed,
            control_sock,
            storage_sock,
            acl,
        } => {
            serve(
                mount_point,
                cache_dir,
                seed,
                control_sock,
                storage_sock,
                acl,
            )
            .await
        }
        Command::Mount {
            mount_point,
            ticket,
            cache_dir,
            control_sock,
            storage_sock,
            acl,
        } => {
            mount(
                mount_point,
                ticket,
                cache_dir,
                control_sock,
                storage_sock,
                acl,
            )
            .await
        }
        Command::MountShare {
            mount_point,
            link,
            link_stdin,
            cache_dir,
        } => {
            Box::pin(mount_share(
                mount_point,
                SharedCredential::Bearer(FileShareLinkV2::decode_uri(&read_share_uri(
                    link, link_stdin,
                )?)?),
                cache_dir,
            ))
            .await
        }
        Command::MountNamedShare {
            mount_point,
            envelope_stdin,
            cache_dir,
        } => {
            if !envelope_stdin {
                bail!("named-share envelope stdin is required");
            }
            let encoded =
                read_bounded_stdin(fs::named_share_delivery::MAX_NAMED_SHARE_ENVELOPE_BYTES * 2)?;
            let bytes =
                hex::decode(encoded).context("named-share envelope is not canonical hex")?;
            let envelope: fs::NamedShareEnvelopeV1 =
                collab_types::canonical::from_canonical_bytes(&bytes)?;
            Box::pin(mount_share(
                mount_point,
                SharedCredential::Named(envelope),
                cache_dir,
            ))
            .await
        }
        Command::RecipientService {
            socket,
            mounts_root,
            cache_root,
            fs_binary,
        } => recipient_service(socket, mounts_root, cache_root, fs_binary).await,
        Command::Replica { config } => replica(config).await,
        Command::ExportHomePeer {
            cache_dir,
            acl,
            output,
            replica_cache_dir,
            status_file,
            principals,
        } => export_home_peer(
            cache_dir,
            acl,
            output,
            replica_cache_dir,
            status_file,
            principals,
        ),
    }
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

/// Re-read the capability list every `every` seconds and swap it into the live mount (`0` disables reload).
fn spawn_acl_reload(file: Arc<CapabilityFile>, every: u64) -> Option<tokio::task::JoinHandle<()>> {
    if every == 0 {
        println!(
            "Capability list {} will NOT be re-read; a later grant needs a remount",
            file.path().display()
        );
        return None;
    }
    Some(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(every));
        loop {
            tick.tick().await;
            match file.refresh() {
                Refresh::Unchanged => {}
                Refresh::Reloaded(n) => {
                    println!(
                        "Capability list {} reloaded: {n} in force",
                        file.path().display()
                    );
                }
                Refresh::Revoked => eprintln!(
                    "Capability list {} is gone; denying every access until it returns",
                    file.path().display()
                ),
                Refresh::Failed(e) => eprintln!(
                    "Capability list {} could not be re-read ({e}); the previously loaded list \
                     stays in force",
                    file.path().display()
                ),
            }
        }
    }))
}

/// Bind and serve the document control socket (best-effort), using the same ACL/identity as the mount.
fn spawn_control(
    sock: &Path,
    namespace: Arc<Mutex<Namespace>>,
    sync: Arc<SyncHandle>,
    enforcement: fs::control::Enforcement,
) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(sock); // clear a stale socket
    match tokio::net::UnixListener::bind(sock) {
        Ok(listener) => {
            // The socket can read and edit every document — restrict to the owner.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600));
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
    use std::os::unix::fs::PermissionsExt;

    if socket.exists() {
        std::fs::remove_file(socket)
            .with_context(|| format!("removing stale share socket {}", socket.display()))?;
    }
    let listener = tokio::net::UnixListener::bind(socket)
        .with_context(|| format!("binding share socket {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
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

fn workspace_ticket_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("workspace-ticket")
}

/// Export a transfer-ready home-peer bundle from the workspace this device is
/// already serving. The ticket is written by `fs serve`, so no terminal ticket
/// or per-file configuration needs to be copied by hand.
fn export_home_peer(
    cache_dir: PathBuf,
    acl: PathBuf,
    output: PathBuf,
    replica_cache_dir: PathBuf,
    status_file: PathBuf,
    principals: Vec<String>,
) -> Result<()> {
    let ticket_path = workspace_ticket_path(&cache_dir);
    let ticket = std::fs::read_to_string(&ticket_path)
        .with_context(|| format!("read active workspace ticket {}", ticket_path.display()))?;
    let decoded_ticket =
        Ticket::decode(ticket.trim()).context("active workspace ticket is invalid")?;
    if !acl.is_file() {
        bail!("capability list {} is not a regular file", acl.display());
    }
    let capability_json = std::fs::read_to_string(&acl)
        .with_context(|| format!("read capability list {}", acl.display()))?;
    let _: serde_json::Value =
        serde_json::from_str(&capability_json).context("capability list is not valid JSON")?;
    let workspace = HomePeerBundle {
        ticket: ticket.trim().to_owned(),
        cache_dir: replica_cache_dir.join(decoded_ticket.workspace_id().to_string()),
        capability_json,
        principals,
    };
    let mut bundle = if output.exists() {
        toml::from_str::<HomePeerSet>(&std::fs::read_to_string(&output)?)
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
            .map(|ticket| ticket.workspace != decoded_ticket.workspace)
            .unwrap_or(true)
    });
    bundle.workspaces.push(workspace);
    atomic_file::write(&output, toml::to_string_pretty(&bundle)?.as_bytes())?;
    println!("exported home-peer bundle to {}", output.display());
    Ok(())
}

/// Join a workspace as a durable, serving peer without a FUSE mount. The
/// normal workspace router keeps the supplied live capability policy in front
/// of its blob provider, so a home peer never becomes an unenforced source.
#[allow(clippy::too_many_lines)]
async fn replica(config_path: PathBuf) -> Result<()> {
    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("read home-peer bundle {}", config_path.display()))?;
    let bundle: HomePeerSet = toml::from_str(&config_text)
        .with_context(|| format!("parse home-peer bundle {}", config_path.display()))?;
    if bundle.workspaces.is_empty() {
        bail!("home-peer bundle contains no workspaces");
    }
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
        let Some((index, status)) = status_rx.recv().await else {
            bail!("all home-peer workspace workers exited");
        };
        states[index] = status;
        let aggregate = aggregate_status(&states);
        save_status(&bundle.status_file, &aggregate)?;
    }
}

#[allow(clippy::too_many_lines)]
async fn replica_workspace(
    bundle: HomePeerBundle,
    index: usize,
    status_tx: tokio::sync::mpsc::Sender<(usize, fs::full_replica::FullReplicaStatus)>,
) -> Result<()> {
    let ticket = Ticket::decode(&bundle.ticket).context("decode home-peer ticket")?;
    std::fs::create_dir_all(&bundle.cache_dir)?;
    let local_acl = bundle.cache_dir.join("capabilities.json");
    atomic_file::write(&local_acl, bundle.capability_json.as_bytes())?;

    let (transport_gate, _transport_gate_task) = fs::transport_gate::spawn_from_env()?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity(&paths, now_ns())?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let transport_identity = FsTransportIdentity::new(credentials.clone(), revocations)
        .with_transport_gate(transport_gate);
    let caps = resolve_acl(
        &AclArgs {
            acl: Some(local_acl),
            identity: None,
            principals: bundle.principals.clone(),
            acl_reload_secs: 1,
        },
        credentials.user_public_key(),
    )?
    .context("home-peer bundle requires a capability list")?;
    let acl = (caps.handle(), caps.identity());
    let _reload = spawn_acl_reload(caps, 1);
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret).await?;
    let store = Arc::new(open_store(Some(bundle.cache_dir.clone())).await?);
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), Arc::clone(&store)));
    let namespace_path = namespace_path(Some(&bundle.cache_dir));
    let namespace = Arc::new(Mutex::new(
        open_namespace_mount(
            endpoint.id().to_string(),
            namespace_path.as_deref(),
            &endpoint,
            ticket.node.clone(),
            &transport_identity,
        )
        .await?,
    ));
    let workspace = Workspace::join(
        endpoint.clone(),
        lookup,
        store,
        Arc::clone(&hydrator),
        Arc::clone(&namespace),
        ticket,
        transport_identity,
        Some(acl.0),
    )
    .await?;
    let _persist = namespace_path.map(|path| spawn_persist(Arc::clone(&namespace), path));
    println!("Home peer joined workspace; hydrating complete content replica.");
    let mut hydrated_namespace = None;
    let mut last_status = fs::full_replica::FullReplicaStatus::syncing();
    loop {
        let before = replica_namespace_fingerprint(&namespace);
        if hydrated_namespace.as_ref() == Some(&before) {
            // Keep the proof fresh without re-reading the full content set on
            // every heartbeat. Any namespace delta below clears this fast path.
            let _ = status_tx.send((index, last_status.clone())).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        // Withdraw first. A partial or stalled pass must never leave the
        // previous workspace revision advertised as a complete replica.
        let _ = status_tx
            .send((index, fs::full_replica::FullReplicaStatus::syncing()))
            .await;
        let status = hydrate_full_workspace(&namespace, &hydrator).await;
        let after = replica_namespace_fingerprint(&namespace);
        if status.full_content_replication && before != after {
            // A reconciliation landed while blobs were downloading. Start a
            // fresh pass against that exact namespace rather than claiming a
            // hybrid of two revisions.
            hydrated_namespace = None;
            continue;
        }
        let _ = status_tx.send((index, status.clone())).await;
        hydrated_namespace = status.full_content_replication.then_some(after);
        last_status = status;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = &workspace;
    }
}

fn replica_namespace_fingerprint(namespace: &Arc<Mutex<Namespace>>) -> [u8; 32] {
    let bytes = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .save();
    *blake3::hash(&bytes).as_bytes()
}

#[allow(clippy::too_many_lines)] // Linear CLI assembly keeps resource lifetimes explicit.
async fn serve(
    mount_point: PathBuf,
    cache_dir: Option<PathBuf>,
    seed: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
    acl_args: AclArgs,
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
        records.availability.clone_from(&availability);
    }
    let history = open_history(cache_dir.as_deref())?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity(&paths, now_ns())?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let share_issuer = load_workspace_share_issuer(&paths, &credentials)?;
    let transport_identity =
        FsTransportIdentity::new(credentials.clone(), Arc::clone(&revocations))
            .with_transport_gate(transport_gate);
    let history_actor = collab_types::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    // Files act as the certified person by default; transport authorship and
    // device-specific grants continue to use the endpoint/device key.
    let caps = resolve_acl(&acl_args, credentials.user_public_key())?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _reload = caps.map(|c| spawn_acl_reload(c, acl_args.acl_reload_secs));
    let named_outbox = cache_dir
        .as_ref()
        .map(|cache| {
            fs::NamedShareOutbox::open(cache.join("named-share-outbox.json")).map(Arc::new)
        })
        .transpose()?;
    let store = Arc::new(open_store(cache_dir.clone()).await?);
    let author = endpoint.id().to_string();

    // Restore a previously-persisted workspace, or start fresh (+ optional seed).
    let (ns_value, fresh) = match np.as_ref().filter(|p| p.exists()) {
        Some(p) => (load_namespace(p, author.clone())?, false),
        None => (Namespace::new(author.clone())?, true),
    };
    let namespace = Arc::new(Mutex::new(ns_value));
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let record_materializer = if let Some(records) = record_services.as_mut() {
        let materializer = RecordMaterializer::new(
            Ticket::workspace_id_for_topic(*endpoint.id().as_bytes()),
            namespace.clone(),
            history.clone(),
        );
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
    let workspace_id = Ticket::workspace_id_for_topic(*endpoint.id().as_bytes());
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
    let allow_legacy_record_migration = record_repository
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
    let workspace_args = acl.as_ref().map(|(handle, _)| handle.clone());
    let (workspace, ticket) = match record_services {
        Some(records) => {
            Workspace::host_with_records(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
                transport_identity,
                workspace_args,
                records,
            )
            .await?
        }
        None => {
            Workspace::host(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
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
    println!(
        "Hosting workspace — share this ticket to invite peers:\n{}",
        ticket.encode()?
    );
    if let Some(cache_dir) = &cache_dir {
        let encoded = ticket.encode()?;
        atomic_file::write(&workspace_ticket_path(cache_dir), encoded.as_bytes())?;
    }
    // Owner operations require the certified human root. Secondary devices
    // never substitute their device key and recipient mounts never host this
    // service.
    let share_task = match (
        &share_socket,
        &record_repository,
        &share_authority,
        share_issuer,
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
            )?;
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
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                let now = now_ms();
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
                    let _ = outbox.record_attempt(entry.envelope.invite.id, delivered, now_ms());
                }
            }
        })
    });

    let _control = control_sock
        .map(|s| spawn_control(&s, namespace.clone(), workspace.sync.clone(), acl.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    run_mount(
        mount_point,
        namespace.clone(),
        hydrator,
        cache_dir.clone(),
        Some(workspace.sync.clone()),
        acl,
        thumb_dir,
        availability,
        eviction_queue,
        share_authority,
        history.map(|history| (history, history_actor)),
        record_publisher,
        replacement_transactions,
        share_socket.clone().filter(|_| share_task.is_some()),
        allow_legacy_record_migration && !initialized_record_authority,
        storage_sock,
        false,
        false,
    )
    .await?;
    if let Some(p) = &np {
        persist_now(&namespace, p); // final flush
    }
    if let Some(task) = named_delivery_task {
        task.abort();
    }
    if let Some(h) = persist {
        h.abort();
    }
    if let Some(task) = share_task {
        task.abort();
    }
    if let Some(socket) = share_socket {
        let _ = std::fs::remove_file(socket);
    }
    drop(workspace);
    Ok(())
}

fn initialize_owner_record_authority(
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    history: Arc<FileHistoryStore>,
    actor: collab_types::FileActor,
    publisher: Arc<RecordPublisher>,
) -> Result<()> {
    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let initialized = QuickFS::new(namespace, hydrator, heuristics)
        .with_history(history, actor)?
        .with_record_publisher(publisher)?;
    drop(initialized);
    Ok(())
}

#[allow(clippy::too_many_lines)] // Linear CLI assembly keeps resource lifetimes explicit.
async fn mount(
    mount_point: PathBuf,
    ticket: String,
    cache_dir: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
    acl_args: AclArgs,
) -> Result<()> {
    let (transport_gate, _transport_gate_task) = fs::transport_gate::spawn_from_env()?;
    let ticket = Ticket::decode(&ticket)?;
    if let Some(cache_dir) = &cache_dir {
        atomic_file::write(
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
        records.availability.clone_from(&availability);
    }
    let history = open_history(cache_dir.as_deref())?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity(&paths, now_ns())?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let transport_identity = FsTransportIdentity::new(credentials.clone(), revocations)
        .with_transport_gate(transport_gate);
    let history_actor = collab_types::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    let caps = resolve_acl(&acl_args, credentials.user_public_key())?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _reload = caps.map(|c| spawn_acl_reload(c, acl_args.acl_reload_secs));
    let store = Arc::new(open_store(cache_dir.clone()).await?);
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let author = endpoint.id().to_string();

    let ns_value = open_namespace_mount(
        author,
        np.as_deref(),
        &endpoint,
        ticket.node.clone(),
        &transport_identity,
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
    let workspace = match record_services {
        Some(records) => {
            Workspace::join_with_records(
                endpoint.clone(),
                lookup,
                store.clone(),
                hydrator.clone(),
                namespace.clone(),
                ticket,
                transport_identity,
                workspace_args,
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
        mount_point,
        namespace.clone(),
        hydrator,
        cache_dir,
        Some(workspace.sync.clone()),
        acl,
        thumb_dir,
        availability,
        eviction_queue,
        share_authority,
        history.map(|history| (history, history_actor)),
        record_publisher,
        replacement_transactions,
        None,
        false,
        storage_sock,
        false,
        false,
    )
    .await?;
    if let Some(p) = &np {
        persist_now(&namespace, p);
    }
    if let Some(h) = persist {
        h.abort();
    }
    drop(workspace);
    Ok(())
}

#[derive(Clone)]
enum SharedCredential {
    Bearer(FileShareLinkV2),
    Named(fs::NamedShareEnvelopeV1),
}

impl SharedCredential {
    fn invite(&self) -> &collab_types::FileInviteV1 {
        match self {
            Self::Bearer(link) => &link.invite,
            Self::Named(envelope) => &envelope.invite,
        }
    }
    fn endpoint(&self) -> &iroh_base::EndpointAddr {
        match self {
            Self::Bearer(link) => &link.endpoint,
            Self::Named(envelope) => &envelope.endpoint,
        }
    }
    fn validate_at(&self, person: collab_types::Ed25519PublicKey, now_ms: u64) -> Result<()> {
        match self {
            Self::Bearer(link) => link.validate_at(now_ms).map_err(Into::into),
            Self::Named(envelope) => envelope.validate_for_at(person, now_ms).map_err(Into::into),
        }
    }
}

#[allow(clippy::too_many_lines)] // Shared-mount bootstrap is one ordered trust sequence.
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
    let certified = load_certified_mount_identity(&paths, now_ns())?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let actor = collab_types::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    credential.validate_at(actor.person, now_ms())?;
    let replica_signing_key = credentials.signing_key().clone();
    let (endpoint, _lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    let client =
        RecordProtocolClient::new(endpoint.clone(), credentials, Arc::clone(&revocations))?;
    let snapshot = match fetch_shared_snapshot(&client, &credential).await {
        Ok(snapshot) => {
            if let Some(subscriptions) = &subscriptions {
                subscriptions.record_success(subscription_key, now_ms())?;
            }
            snapshot
        }
        Err(error) => {
            if let Some(subscriptions) = &subscriptions {
                subscriptions.record_error(subscription_key, error.failure_code(), now_ms())?;
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
    if let Some(revision) = snapshot.content_revision {
        track_shared_revision(availability.as_ref(), revision)?;
        if let SharedCredential::Bearer(link) = &credential
            && let Err(error) = refresh_host_replica_observation(
                &client,
                link,
                revision,
                availability.as_ref(),
                actor.device,
            )
            .await
        {
            eprintln!("shared-record host replica is not yet available: {error}");
        }
    }
    let thumb_dir = thumb_path(cache.as_deref());

    // Refresh the authenticated snapshot and the host-side live blob session.
    // Same-actor bearer redemption is idempotent, while expiry/revocation
    // still stops subsequent refreshes and fetch authorization.
    let poll_namespace = Arc::clone(&namespace);
    let poll_history = Arc::clone(&history);
    let poll_path = namespace_path.clone();
    let poll_client = client.clone();
    let poll_credential = credential.clone();
    let poll_subscriptions = subscriptions.clone();
    let poll_store = Arc::clone(&store);
    let poll_availability = availability.clone();
    let poll_replica_signing_key = replica_signing_key;
    let poll_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            if poll_credential.validate_at(actor.person, now_ms()).is_err() {
                break;
            }
            match fetch_shared_snapshot(&poll_client, &poll_credential).await {
                Ok(snapshot) if snapshot.record_id == expected_record => {
                    if let Err(error) =
                        apply_shared_snapshot(&poll_namespace, &poll_history, &snapshot)
                    {
                        eprintln!("shared record refresh rejected: {error}");
                        break;
                    }
                    persist_now(&poll_namespace, &poll_path);
                    if let Some(revision) = snapshot.content_revision {
                        if let Err(error) =
                            track_shared_revision(poll_availability.as_ref(), revision)
                        {
                            eprintln!("could not persist shared-record version: {error}");
                            break;
                        }
                        if let SharedCredential::Bearer(poll_link) = &poll_credential
                            && let Err(error) = refresh_host_replica_observation(
                                &poll_client,
                                poll_link,
                                revision,
                                poll_availability.as_ref(),
                                actor.device,
                            )
                            .await
                        {
                            eprintln!("shared-record host replica is not yet available: {error}");
                        }
                    }
                    if let Some(revision) = snapshot.content_revision
                        && poll_store.verify_durable_revision(&revision).await.is_ok()
                    {
                        match fs::ReplicaAckRequestV1::new(
                            poll_credential.invite().workspace_id,
                            revision,
                        )
                        .and_then(|request| {
                            fs::ReplicaAckV1::signed(
                                &request,
                                actor,
                                now_ms(),
                                &poll_replica_signing_key,
                            )
                        }) {
                            Ok(receipt) => {
                                if !matches!(
                                    request_shared(
                                        &poll_client,
                                        &poll_credential,
                                        RecordProtocolOperationV1::AcknowledgeReplica(receipt)
                                    )
                                    .await,
                                    Ok(RecordProtocolResponseV1::ReplicaAcknowledged { .. })
                                ) {
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
                    if let Some(subscriptions) = &poll_subscriptions
                        && let Err(error) = subscriptions.record_success(subscription_key, now_ms())
                    {
                        eprintln!("could not persist shared-record refresh status: {error}");
                        break;
                    }
                }
                Ok(_) => {
                    if let Some(subscriptions) = &poll_subscriptions {
                        let _ = subscriptions.record_error(
                            subscription_key,
                            RecordSubscriptionFailureCode::InvalidResponse,
                            now_ms(),
                        );
                    }
                    break;
                }
                Err(error) => {
                    if let Some(subscriptions) = &poll_subscriptions {
                        let _ = subscriptions.record_error(
                            subscription_key,
                            error.failure_code(),
                            now_ms(),
                        );
                    }
                    eprintln!("shared record refresh failed: {error}");
                }
            }
        }
    });

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
        mount_point,
        Arc::clone(&namespace),
        hydrator,
        Some(cache_dir.clone()),
        None,
        Some((AclHandle::new(recipient_acl), recipient_identity)),
        thumb_dir,
        availability,
        eviction_queue,
        None,
        Some((history, actor)),
        None,
        None,
        None,
        false,
        None,
        false,
        true,
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
    std::fs::create_dir_all(&cache_root)?;
    let paths = identity_paths();
    let certified = load_certified_mount_identity(&paths, now_ns())?;
    let revocations = Arc::new(RwLock::new(load_revocation_list(&paths)?));
    let (device, credentials) = certified.into_parts();
    let local_person = collab_types::Ed25519PublicKey::from_bytes(credentials.user_public_key());
    let inbox = Arc::new(fs::NamedShareInbox::open_at(
        cache_root.join("named-share-inbox.json"),
        local_person,
        now_ms(),
    )?);
    let (inbox_endpoint, _inbox_lookup) = sync::bind_endpoint_with_key(device.secret).await?;
    let inbox_protocol = fs::NamedShareInboxProtocol::new(
        inbox_endpoint.clone(),
        credentials,
        Arc::clone(&revocations),
        Arc::clone(&inbox),
    )?;
    let inbox_router = iroh::protocol::Router::builder(inbox_endpoint.clone())
        .accept(fs::QOS_FS_NAMED_SHARE_INBOX_ALPN, inbox_protocol)
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
    for envelope in inbox.entries_at(now_ms()) {
        if let Err(error) = mounts.start_named(&envelope) {
            eprintln!(
                "could not restart named received record {}: {error}",
                envelope.invite.record_id.0
            );
        }
    }

    let named_mounts = Arc::clone(&mounts);
    let named_inbox = Arc::clone(&inbox);
    let named_mount_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            for envelope in named_inbox.entries_at(now_ms()) {
                let _ = named_mounts.start_named(&envelope);
            }
        }
    });

    let listener = bind_recipient_service_socket(&socket)?;
    let server = Arc::new(RecipientControlServer::with_mounts(subscriptions, mounts));
    println!("Recipient share service listening at {}", socket.display());
    let result = server.serve(listener).await;
    named_mount_task.abort();
    inbox_router.shutdown().await?;
    result?;
    Ok(())
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

async fn fetch_shared_snapshot(
    client: &RecordProtocolClient,
    credential: &SharedCredential,
) -> std::result::Result<RecordProtocolSnapshotV1, SharedFetchError> {
    match request_shared(client, credential, RecordProtocolOperationV1::Read).await? {
        RecordProtocolResponseV1::Snapshot { snapshot, .. } => Ok(snapshot),
        RecordProtocolResponseV1::Denied => Err(SharedFetchError::Denied),
        RecordProtocolResponseV1::NotFound => Err(SharedFetchError::NotFound),
        RecordProtocolResponseV1::InternalError => Err(SharedFetchError::Internal),
        _ => Err(SharedFetchError::InvalidResponse),
    }
}

async fn request_shared(
    client: &RecordProtocolClient,
    credential: &SharedCredential,
    operation: RecordProtocolOperationV1,
) -> std::result::Result<RecordProtocolResponseV1, RecordProtocolClientError> {
    match credential {
        SharedCredential::Bearer(link) => client.request_bearer_link(link, operation).await,
        SharedCredential::Named(envelope) => {
            client
                .request_named(
                    envelope.endpoint.clone(),
                    envelope.invite.clone(),
                    operation,
                )
                .await
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum SharedFetchError {
    #[error("shared-record transport or authentication failed")]
    Client(#[from] RecordProtocolClientError),
    #[error("shared-record access was denied or revoked")]
    Denied,
    #[error("shared record no longer exists")]
    NotFound,
    #[error("shared-record host returned an invalid response")]
    InvalidResponse,
    #[error("shared-record host failed internally")]
    Internal,
}

impl SharedFetchError {
    fn failure_code(&self) -> RecordSubscriptionFailureCode {
        match self {
            Self::Client(
                RecordProtocolClientError::Transport(_)
                | RecordProtocolClientError::Timeout
                | RecordProtocolClientError::ReplicaUnavailable,
            ) => RecordSubscriptionFailureCode::Offline,
            Self::Client(
                RecordProtocolClientError::Authentication(_)
                | RecordProtocolClientError::LocalEndpointMismatch
                | RecordProtocolClientError::ServerEndpointMismatch
                | RecordProtocolClientError::RevocationStatePoisoned
                | RecordProtocolClientError::ReplicaVerification(_),
            ) => RecordSubscriptionFailureCode::Authentication,
            Self::Denied
            | Self::Client(
                RecordProtocolClientError::Link(_) | RecordProtocolClientError::ReplicaDenied,
            ) => RecordSubscriptionFailureCode::Denied,
            Self::NotFound => RecordSubscriptionFailureCode::NotFound,
            Self::InvalidResponse
            | Self::Client(
                RecordProtocolClientError::RequestTooLarge
                | RecordProtocolClientError::InvalidRequest
                | RecordProtocolClientError::ResponseTooLarge
                | RecordProtocolClientError::InvalidResponse,
            ) => RecordSubscriptionFailureCode::InvalidResponse,
            Self::Internal => RecordSubscriptionFailureCode::Internal,
        }
    }
}

#[allow(clippy::significant_drop_tightening)] // Registry update and persistence are one mutation.
fn track_shared_revision(
    availability: Option<&Arc<Mutex<fs::AvailabilityRegistry>>>,
    revision: collab_types::ContentRevisionV1,
) -> Result<()> {
    let Some(availability) = availability else {
        return Ok(());
    };
    let mut registry = availability
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    revision: collab_types::ContentRevisionV1,
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
    let mut registry = availability
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.observe_replica(
        revision.record_id,
        local_device,
        observation,
        observed_at_ms,
    )?;
    drop(registry);
    Ok(())
}

#[allow(clippy::significant_drop_tightening)] // The complete projection is namespace-atomic.
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
        let mut namespace = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .filter(|preview| preview.status == collab_types::PreviewStatus::Ready)
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
    expected_record: collab_types::FileRecordId,
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

fn shared_record_name(record_id: collab_types::FileRecordId, mime: Option<&str>) -> String {
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
#[allow(clippy::significant_drop_tightening)] // Durable replacement is serialized with mutations.
fn persist_now(namespace: &Arc<Mutex<Namespace>>, path: &Path) {
    // Keep the namespace lock through the durable replacement. Transactional
    // projections use the same lock, preventing two same-process writers from
    // sharing atomic-file's process-scoped temporary pathname.
    let mut namespace = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bytes = namespace.save();
    if let Err(e) = fs::durable_backup::write(path, &bytes, |candidate| {
        Namespace::from_bytes("namespace-backup-validator", candidate).is_ok()
    }) {
        eprintln!("namespace persist to {} failed: {e}", path.display());
    }
}

fn load_namespace(path: &Path, author: String) -> Result<Namespace> {
    let outcome = fs::durable_backup::read_or_recover(path, |bytes| {
        Namespace::from_bytes("namespace-backup-validator", bytes).is_ok()
    })?
    .with_context(|| format!("namespace disappeared while opening {}", path.display()))?;
    report_namespace_recovery(path, outcome.recovery.as_ref());
    Ok(Namespace::from_bytes(author, &outcome.bytes)?)
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

/// Persist the namespace to disk every few seconds.
fn spawn_persist(namespace: Arc<Mutex<Namespace>>, path: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            persist_now(&namespace, &path);
        }
    })
}

/// Open a joiner's namespace: load any persisted copy, then merge the host's doc if reachable.
async fn open_namespace_mount(
    author: String,
    np: Option<&Path>,
    endpoint: &iroh::endpoint::Endpoint,
    host: iroh_base::EndpointAddr,
    identity: &FsTransportIdentity,
) -> Result<Namespace> {
    let persisted = match np.as_ref().filter(|p| p.exists()) {
        Some(path) => Some(load_namespace(path, author.clone())?),
        None => None,
    };

    match sync::fetch_remote_doc(endpoint, host, identity).await {
        Ok(doc) => match persisted {
            Some(mut ns) => {
                ns.merge_remote_and_resolve(&doc)?;
                Ok(ns)
            }
            None => Ok(Namespace::from_bytes(author, &doc)?),
        },
        Err(e) => match persisted {
            Some(ns) => {
                eprintln!("host unreachable ({e}); starting from persisted workspace");
                Ok(ns)
            }
            None => Err(e),
        },
    }
}

/// Mount the FUSE filesystem and run until interrupted (or the mount exits).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // CLI assembly mirrors independent mount options.
async fn run_mount(
    mount_point: PathBuf,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    predictive_cache_root: Option<PathBuf>,
    sync: Option<Arc<SyncHandle>>,
    acl: Option<(AclHandle, IdentityKey)>,
    thumb_dir: Option<PathBuf>,
    availability: Option<Arc<Mutex<fs::AvailabilityRegistry>>>,
    eviction_queue: Option<Arc<tokio::sync::Mutex<fs::eviction::EvictionQueue>>>,
    share_authority: Option<Arc<ShareAuthority>>,
    history: Option<(Arc<FileHistoryStore>, collab_types::FileActor)>,
    record_publisher: Option<Arc<RecordPublisher>>,
    replacement_transactions: Option<(Arc<Mutex<fs::RecordTransactionJournal>>, PathBuf)>,
    share_control_path: Option<PathBuf>,
    allow_legacy_record_migration: bool,
    storage_sock: Option<PathBuf>,
    read_only: bool,
    signal_recipient_ready: bool,
) -> Result<()> {
    if !mount_point.exists() {
        std::fs::create_dir_all(&mount_point)?;
    }

    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let predictive_policy_task =
        fs::smart_monitor::start(Arc::clone(&heuristics), predictive_cache_root).await;
    let fs = QuickFS::new(namespace, hydrator, heuristics);
    let fs = if allow_legacy_record_migration {
        fs
    } else {
        fs.without_legacy_record_migration()
    };
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
    // Host-side thumbnails: generate previews as metadata, served via the `user.qos.thumb` xattr.
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
    fs.resume_pending_evictions();

    let storage_authority = storage_sock.as_ref().map(|_| {
        fs::storage::StorageAuthority::new(
            Arc::clone(&fs.namespace),
            Arc::clone(&fs.hydrator),
            Arc::clone(&fs.hydrator.store),
            fs.write_buffers_handle(),
            "Current workspace",
        )
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

    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("quickfs".to_string())];
    if read_only {
        config.mount_options.push(MountOption::RO);
    }
    // The installed service uses auto-unmount and the system-wide qOS mount
    // policy. The mandatory process harness runs all peers under one test UID
    // and opts into the kernel's owner-only mode, which does not require
    // mutating host `/etc/fuse.conf`.
    if std::env::var_os("QOS_FS_FUSE_OWNER_ONLY").is_some() {
        config.acl = SessionACL::Owner;
    } else {
        config.acl = SessionACL::All;
        config.mount_options.push(MountOption::AutoUnmount);
    }

    let mp = mount_point.clone();
    let mount_task = tokio::task::spawn_blocking(move || {
        let session = fuser::spawn_mount2(fs, &mp, &config)?;
        if signal_recipient_ready {
            use std::io::Write;
            println!("{}", fs::recipient_mount::RECIPIENT_MOUNT_READY_LINE);
            std::io::stdout().flush()?;
        }
        session.join()
    });
    println!("Mounting QuickFS at {}", mount_point.display());

    // Exit cleanly on Ctrl-C or mount exit (don't hang a crash-looping service forever).
    tokio::select! {
        res = mount_task => match res {
            Ok(Ok(())) => println!("Filesystem unmounted."),
            Ok(Err(e)) => eprintln!("FUSE mount exited with error: {e}"),
            Err(e) => eprintln!("mount task failed: {e}"),
        },
        _ = tokio::signal::ctrl_c() => println!("Interrupted, exiting..."),
    }
    if let Some(task) = predictive_policy_task {
        task.abort();
    }
    Ok(())
}

fn now_ms() -> u64 {
    fs::clock::unix_now_ms()
}

fn now_ns() -> u64 {
    fs::clock::unix_now_ns()
}

/// Recursively ingest a directory's contents into the workspace.
async fn seed_from_dir(
    namespace: &Arc<Mutex<Namespace>>,
    store: &Arc<ContentStore>,
    root: &Path,
) -> Result<()> {
    let registry = qdoc::builtin_registry();
    let mut stack = vec![(ROOT_INO, root.to_path_buf())];
    while let Some((parent_ino, dir)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                let ino = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .create_dir(parent_ino, &name, now_ms())
                    .ok_or_else(|| {
                        anyhow::anyhow!("seeding {name:?}: parent inode {parent_ino} vanished")
                    })?;
                stack.push((ino, path));
            } else if ft.is_file() {
                let data = std::fs::read(&path)?;
                let codec = name
                    .rsplit('.')
                    .next()
                    .filter(|e| *e != name)
                    .and_then(|e| registry.provider_for_ext(e).map(|_| e.to_lowercase()));
                if let Some(c) = codec {
                    // Recognized rich-text format → seed as a CRDT document.
                    let mut ns = namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(ino) = ns.create_document(parent_ino, &name, &c, now_ms()) {
                        ns.import_document(ino, &registry, &data, now_ms());
                    }
                } else {
                    let size = data.len() as u64;
                    let hash = store.write_content(&data).await?;
                    namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .create_file(parent_ino, &name, hash.as_ref(), size, now_ms());
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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel)
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
        argv
    }

    #[test]
    fn service_command_line_parses() {
        // The exact line fs.service runs, so a bad flag fails here, not on boot.
        let argv = service_exec_start();
        assert_eq!(
            argv[0], "/usr/bin/fs",
            "unexpected ExecStart binary: {argv:?}"
        );
        let cli = Cli::try_parse_from(std::iter::once("fs".to_string()).chain(argv[1..].to_vec()))
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

    // An enforcement point shipped switched off is not an enforcement point, so the unit is asserted on directly.
    #[test]
    fn the_shipped_service_enforces_a_capability_list() {
        let argv = service_exec_start();
        let cli = Cli::try_parse_from(std::iter::once("fs".to_string()).chain(argv[1..].to_vec()))
            .expect("fs.service command line must parse");
        let Command::Serve { acl, .. } = cli.command else {
            panic!("expected serve");
        };
        assert_eq!(
            acl.acl.as_deref(),
            Some(Path::new("/var/lib/qos/fs-capabilities.json")),
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

    // tmpfiles.d must place the file the unit names, or the mount is fail-closed-refused on every fresh boot.
    #[test]
    fn tmpfiles_seeds_the_capability_list_the_service_names() {
        let conf = std::fs::read_to_string(os_file("os/usr/lib/tmpfiles.d/qos.conf"))
            .expect("the shipped tmpfiles snippet must exist");
        let seeding = conf
            .lines()
            .find(|l| l.starts_with("C ") && l.contains("/var/lib/qos/fs-capabilities.json"))
            .unwrap_or_else(|| {
                panic!("no tmpfiles line creates /var/lib/qos/fs-capabilities.json:\n{conf}")
            });
        assert!(
            seeding.contains("/usr/share/qos/fs-capabilities.json"),
            "the seed must come from the shipped default: {seeding}"
        );

        // ...and the image must contain the file tmpfiles copies from, or the seed does nothing and the mount is refused on every boot.
        let containerfile = std::fs::read_to_string(os_file("os/Containerfile"))
            .expect("the production Containerfile must exist");
        assert!(
            containerfile.lines().any(
                |l| l.starts_with("COPY ") && l.contains("/usr/share/qos/fs-capabilities.json")
            ),
            "os/Containerfile must ship the default capability list that tmpfiles.d seeds from"
        );
    }

    // ...and the seeded default must be a capability list this binary can parse, or the mount is refused on first boot.
    #[test]
    fn the_shipped_default_list_grants_the_local_session_its_workspace() {
        use fs::Access;

        let path = os_file("os/usr/share/qos/fs-capabilities.json");
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

    #[test]
    fn mount_command_line_parses() {
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

    // No `--acl` means Unix-veneer-only behaviour — the documented opt-out, and why fs.service still works unchanged.
    #[test]
    fn without_the_acl_flag_no_capability_list_is_bound() {
        let resolved = resolve_acl(&acl_args_from(&[]), NODE_KEY).expect("no ACL is not an error");
        assert!(
            resolved.is_none(),
            "omitting --acl must leave the mount unbound, not bind an empty ACL"
        );
    }

    // Asking for enforcement and not getting it must never be silent.
    #[test]
    fn a_missing_capability_file_refuses_the_mount() {
        let args = acl_args_from(&["--acl", "/nonexistent/qos-caps-does-not-exist.json"]);
        let err = resolve_acl(&args, NODE_KEY)
            .expect_err("a missing capability list must abort the mount, not fall back");
        assert!(
            format!("{err:#}").contains("could not be read"),
            "expected a read failure, got: {err:#}"
        );
    }

    #[test]
    fn a_malformed_capability_file_refuses_the_mount() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        std::fs::write(&path, b"{ not json ]").unwrap();
        let args = acl_args_from(&["--acl", path.to_str().unwrap()]);
        let err = resolve_acl(&args, NODE_KEY)
            .expect_err("a malformed capability list must abort the mount");
        assert!(
            format!("{err:#}").contains("JSON array of capabilities"),
            "expected a parse failure, got: {err:#}"
        );
    }

    // An empty capability list is a valid, total denial — not an error, and not a fallback to unenforced.
    #[test]
    fn an_empty_capability_file_binds_a_denying_acl() {
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

    #[test]
    fn the_capability_file_round_trips_and_is_enforced() {
        use collab_types::{CapOverride, Ed25519PublicKey, Effect, Scope, Subject, Verb};
        use fs::Access;

        let who = [0x0A; 32];
        let caps = vec![CapOverride::new(
            Subject::User(Ed25519PublicKey(who)),
            Scope::FsPath("/w/docs".into()),
            Effect::Grant,
            Verb::Read,
        )];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        std::fs::write(&path, serde_json::to_vec(&caps).unwrap()).unwrap();

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

    // `--identity` alone enforces nothing; accepting it silently would read as identity-scoped when it isn't.
    #[test]
    fn identity_without_a_capability_list_is_rejected() {
        let args = acl_args_from(&["--identity", &hex::encode([0x0A; 32])]);
        assert!(resolve_acl(&args, NODE_KEY).is_err());
    }

    #[test]
    fn a_short_identity_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        std::fs::write(&path, b"[]").unwrap();
        let args = acl_args_from(&["--acl", path.to_str().unwrap(), "--identity", "abcd"]);
        let err = resolve_acl(&args, NODE_KEY).expect_err("a 2-byte identity is not a key");
        assert!(format!("{err:#}").contains("32 bytes"), "got: {err:#}");
    }

    #[test]
    fn principal_specs_parse_by_kind() {
        assert_eq!(
            parse_principal("room:team").unwrap(),
            Principal::Room("team".into())
        );
        assert_eq!(
            parse_principal("app:org.qos.files").unwrap(),
            Principal::App("org.qos.files".into())
        );
        assert!(parse_principal("wizard:merlin").is_err());
        assert!(parse_principal("noseparator").is_err());
    }
}
