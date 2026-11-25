#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod mount;
mod primary_workspace;
mod session_supervisor;
mod workspace_startup;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use fuser::{Config, MountOption, SessionACL};
use mount::{MountConfig, MountOutcome, MountServices, run_mount};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_action_policy::MemberId;
use tilo_fs::clock::unix_now_ms;
use tilo_fs::namespace::{MountAuthor, ROOT_INO};
use tilo_fs::{
    AclHandle, CapabilityFile, ContentStore, Heuristics, Hydrator, IdentityKey, Namespace, QuickFS,
    device_public_key, identity_paths, load_certified_mount_identity_once_valid,
};
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

/// How the mount enforces per-identity file access.
#[derive(Args, Debug, Clone, Default)]
struct AclArgs {
    /// Enforce this capability list (JSON array of `collab_types::CapOverride`); a load failure aborts the mount.
    #[arg(long, value_name = "FILE")]
    acl: Option<PathBuf>,
    /// The identity this mount acts as: 64 hex chars (Ed25519 public key). Defaults to the certified human root.
    #[arg(long, value_name = "HEX")]
    identity: Option<String>,
    /// A member this mount acts for beyond its own key (`person:<id>`, `agent:<person>/<id>`, `bot:<id>`); repeatable.
    #[arg(long = "member", value_name = "KIND:ID")]
    members: Vec<String>,
    /// Read `--acl` once, at mount time, instead of on every change.
    #[arg(long)]
    acl_once: bool,
}

/// Where a mount serves its local services, and how it reaches the network.
#[derive(Args, Debug, Clone, Default)]
struct SocketArgs {
    /// Serve typed hydration accounting and safe eviction here.
    #[arg(long)]
    storage_sock: Option<PathBuf>,
    /// Serve the scoped document-media capability here.
    #[arg(long)]
    blob_sock: Option<PathBuf>,
}

/// Which local backend serves the workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum BackendChoice {
    /// The native tree (Tier 1) when the store volume and kernel support
    /// it, else FUSE.
    #[default]
    Auto,
    /// Always FUSE (Tier 2/3).
    Fuse,
    /// The native tree or fail.
    Native,
}

/// Where and how the Tier 1 backend runs.
#[derive(Args, Debug, Clone)]
struct BackendArgs {
    /// Local backend: `auto` serves the native tree when the store volume and kernel support it.
    #[arg(long, value_enum, default_value_t = BackendChoice::Auto)]
    backend: BackendChoice,
    /// Root of the dedicated Btrfs store volume holding the native workspace
    /// trees.
    #[arg(long, value_name = "DIR", default_value = "/var/lib/tilo/store")]
    store: PathBuf,
    /// Owner (`UID:GID`) of the workspace's files, the local person: the native backend materialises them as theirs (default: the workspace directory's owner), and FUSE presents them as theirs (default: the caller).
    #[arg(long, value_name = "UID:GID")]
    native_owner: Option<String>,
    /// Remote files up to this many bytes are fetched before they appear; larger ones start as placeholders.
    #[arg(long, value_name = "BYTES", default_value_t = 1024 * 1024)]
    native_eager_bytes: u64,
    /// Arbitrate the workspace's write leases: grant a free lease to one device at a time (the always-on host).
    #[arg(long)]
    lease_authority: bool,
}

impl Default for BackendArgs {
    fn default() -> Self {
        Self {
            backend: BackendChoice::Fuse,
            store: PathBuf::from("/var/lib/tilo/store"),
            native_owner: None,
            native_eager_bytes: 1024 * 1024,
            lease_authority: false,
        }
    }
}

/// The owner the mount point itself has, shown for every caller of a mount nobody named an
/// owner for.
fn mount_point_owner(at: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(at).ok()?;
    Some((metadata.uid(), metadata.gid()))
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
        /// Exit with an error unless the cache holds a resumable saved workspace.
        #[arg(long)]
        require_existing: bool,
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
        /// Use this login's identity service for workspace directory content keys.
        #[arg(long, hide = true, requires = "acl", value_name = "SOCKET")]
        workspace_key_socket: Option<PathBuf>,
        #[command(flatten)]
        sockets: SocketArgs,
        #[command(flatten)]
        acl: AclArgs,
        #[command(flatten)]
        backend: BackendArgs,
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
    /// Ask the origins of this device's public catalogue copies that are due
    /// whether they changed, through the running daemon.
    RefreshPublicContent {
        /// The daemon's document-media socket.
        #[arg(long, default_value = "/run/tilo/fs-blobs.sock")]
        blob_sock: PathBuf,
    },
    /// Serve the document-media capability alone: this device's content store
    /// and the scoped capability over it, with no workspace mount and no
    /// collaboration.
    ///
    /// A desktop session's document media is its filesystem daemon's
    /// (`fs serve --blob-sock`), which serves both from one store. A session
    /// whose filesystem is a *different* build's — every development session,
    /// whose apps and connector run this tree while the mount belongs to the
    /// image underneath — cannot use that daemon's capability: the two speak
    /// different wires, and every store call dies on the first frame. Such a
    /// session serves its own media here instead, from a store of its own,
    /// and hands its apps that socket ([`SERVE_MEDIA_SOCKET_ENV`]).
    ///
    /// A Tilo image needs none of this: its filesystem daemon is built from
    /// the same tree as the apps it serves, so `fs serve --blob-sock` is their
    /// document media already, and nothing on such a device starts this.
    ServeMedia {
        /// Where this device's content store is kept: created when absent,
        /// one directory per build's protocol.
        #[arg(long)]
        store: PathBuf,
        /// Serve the scoped document-media capability here.
        #[arg(long)]
        blob_sock: PathBuf,
    },
}

/// The variable naming the document-media socket a session serves its apps,
/// instead of the image's `/run/tilo/fs-blobs.sock`: set by a session whose
/// filesystem is another build's (`fs serve-media`).
pub const SERVE_MEDIA_SOCKET_ENV: &str = "TILO_FS_BLOB_SOCKET";

fn main() -> Result<()> {
    // Before the runtime starts any thread: this executable also serves as
    // the Automerge worker the conflict merge decodes peer versions in.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    let cli = Cli::parse();
    // What the daemon says goes to its standard error, which its service's log keeps: its
    // warnings, and what the cloud service does with each file account.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,tilo_fs::cloud=info")),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Command::ServePrimary(args) = &cli.command {
        std::process::exit(primary_workspace::run(args));
    }
    // FUSE callbacks poll their futures on the fuser threads via the
    // runtime handle; the workers only serve iroh, sockets and timers.
    tilo_rt::daemon("tilo-fs").run(run_command(cli))?
}

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch over every subcommand the daemon offers"
)]
async fn run_command(cli: Cli) -> Result<()> {
    if let Some(path) = &cli.test_clock_file {
        tilo_fs::clock::install_test_clock_file(path)
            .context("configuring deterministic test clock")?;
    }
    match cli.command {
        Command::WorkspaceStatus {
            cache_dir,
            require_existing,
        } => {
            let status = workspace_startup::inspect(&cache_dir, &tree_socket(None)?)?;
            if require_existing {
                status.require_existing()?;
            }
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        Command::ServePrimary(_) => bail!("serve-primary runs before the runtime starts"),
        Command::ServeMedia { store, blob_sock } => serve_media(&store, &blob_sock).await,
        Command::Serve {
            require_existing_workspace,
            mount_point,
            cache_dir,
            seed,
            workspace_key_socket,
            sockets,
            acl,
            backend,
        } => {
            if require_existing_workspace {
                let cache = cache_dir
                    .as_deref()
                    .context("recovery requires --cache-dir")?;
                workspace_startup::inspect(cache, &tree_socket(backend.native_owner.as_deref())?)?
                    .require_existing()?;
            }
            Box::pin(serve(
                mount_point,
                cache_dir,
                seed,
                workspace_key_socket,
                sockets,
                acl,
                backend,
            ))
            .await
        }
        #[cfg(target_os = "linux")]
        Command::CloudService {
            mounts_root,
            state_root,
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
                tilo_fs::cloud::CloudService::new(
                    tilo_fs::cloud::ServiceConfig {
                        // The daemon that keeps the workspaces' accounts releases each mount's
                        // sign-in, as the person on this computer, whenever the mount asks.
                        accounts: std::sync::Arc::new(
                            tilo_fs::cloud::SessionAccounts::new()
                                .map_err(std::io::Error::other)?,
                        ),
                        mounts_root,
                        state_root,
                        control_socket,
                        file_socket,
                    },
                    Box::new(tilo_fs::cloud::ProcessLauncher {
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
                tilo_fs::cloud::service::run_mount(&mount_point, &state_dir)
            })
            .await??;
            Ok(())
        }
        Command::RefreshPublicContent { blob_sock } => {
            // fs.service starts only on a device with its certified identity, and while it
            // waits for the clock or restarts nobody listens on its socket: no daemon
            // serving now means no public copies to refresh, which is no failure of the
            // timer. Its next run asks again.
            let stream = match tilo_files::native::socket::connect(&blob_sock) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    println!(
                        "no fs daemon serves {} now ({error}): no public copies to refresh",
                        blob_sock.display()
                    );
                    return Ok(());
                }
                stream => stream.with_context(|| format!("connect to {}", blob_sock.display()))?,
            };
            let changed = tokio::task::spawn_blocking(move || -> Result<u32> {
                let mut stream = stream;
                runtime_fs::blobs::write_handoff(
                    &mut stream,
                    &runtime_fs::blobs::BlobActor::Session,
                )?;
                Ok(runtime_fs::blobs::BlobClient::from_session_stream(stream)?.refresh_public()?)
            })
            .await??;
            println!("{changed} public copies changed");
            Ok(())
        }
    }
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

fn parse_member(spec: &str) -> Result<MemberId> {
    let (kind, id) = spec
        .split_once(':')
        .with_context(|| format!("member {spec:?} must be KIND:ID"))?;
    let id = id.to_string();
    Ok(match kind {
        "person" => MemberId::Person(
            collab_types::PersonId::parse(&id)
                .with_context(|| format!("{id:?} is not a person's root key"))?,
        ),
        // An agent is its id and the person it acts for: `agent:PERSON/ID`.
        "agent" => {
            let (person, agent) = id
                .split_once('/')
                .with_context(|| format!("agent {id:?} must be PERSON/ID"))?;
            MemberId::agent(
                agent,
                collab_types::PersonId::parse(person)
                    .with_context(|| format!("{person:?} is not a person's root key"))?,
            )
        }
        "bot" => MemberId::Bot(id),
        other => bail!("unknown member kind {other:?} (person|agent|bot)"),
    })
}

/// Resolve `--acl`, `--identity` and `--member` into the ACL the mount enforces (`None` if none
/// requested). Fail-closed: any resolution failure refuses the mount rather than falling back to
/// unenforced; the certified human root is the production default identity.
fn resolve_acl(
    args: &AclArgs,
    default_identity: IdentityKey,
) -> Result<Option<Arc<CapabilityFile>>> {
    let Some(path) = &args.acl else {
        if args.identity.is_some() || !args.members.is_empty() {
            bail!(
                "--identity/--member require --acl; without a capability list they enforce nothing"
            );
        }
        return Ok(None);
    };

    let identity = match &args.identity {
        Some(hex_str) => parse_identity(hex_str)?,
        None => default_identity,
    };
    let members = args
        .members
        .iter()
        .map(|s| parse_member(s))
        .collect::<Result<Vec<_>>>()?;

    let file = CapabilityFile::load(path, identity, members)
        .with_context(|| format!("binding the capability list {}", path.display()))?;
    println!(
        "Enforcing {} capabilities from {} as identity {}",
        file.count(),
        path.display(),
        hex::encode(identity)
    );
    Ok(Some(Arc::new(file)))
}

/// Keep the live mount's capability list current: re-read on every change
/// of the file, unless `once`.
fn spawn_acl_reload(
    file: Arc<CapabilityFile>,
    once: bool,
) -> Result<Option<tokio::task::JoinHandle<()>>> {
    if once {
        println!(
            "Capability list {} will NOT be re-read; a later grant needs a remount",
            file.path().display()
        );
        return Ok(None);
    }
    let path = file.path().to_path_buf();
    let reload = file
        .spawn_reload()
        .with_context(|| format!("watching the capability list {}", path.display()))?;
    Ok(Some(reload))
}

/// Serve document media alone, from the content store at `store`, at `socket`, for a session
/// whose filesystem daemon is another build's ([`Command::ServeMedia`]).
///
/// The server thread serves the socket until the process ends, so a socket a killed session
/// left behind is replaced the next time this runs.
async fn serve_media(store: &Path, socket: &Path) -> Result<()> {
    std::fs::create_dir_all(store)
        .with_context(|| format!("creating the content store at {}", store.display()))?;
    let store = Arc::new(
        ContentStore::open(store)
            .await
            .with_context(|| format!("opening the content store at {}", store.display()))?,
    );
    let media = tilo_fs::document_media::DocumentMedia::new(Arc::clone(&store));
    let _server = tilo_fs::document_media::serve_socket(socket, media)
        .with_context(|| format!("serving document media at {}", socket.display()))?;
    println!("Document media socket at {}", socket.display());
    // Serve until this process is ended, as the mount's own daemon does: the session that
    // started this one ends it, and nothing here has a last pass to run.
    std::future::pending::<()>().await;
    Ok(())
}

/// Serve document media from the content store (best-effort, like the other local sockets).
/// Content of another device's records reaches this store on the record's route, through the
/// session.
fn spawn_document_media(
    socket: &Path,
    store: &Arc<ContentStore>,
    cache_dir: Option<&Path>,
) -> Option<std::thread::JoinHandle<()>> {
    // The device's one document-media service also keeps the device's copies
    // of public files, beside the content store whose budget evicts them.
    let public = cache_dir.and_then(|dir| {
        let origin = tilo_fs::public_content::NetOrigin::new()
            .map_err(|error| eprintln!("public content cannot be fetched here: {error}"))
            .ok()?;
        tilo_fs::public_content::PublicContent::open(
            &dir.join("public-content"),
            Arc::clone(store.files()),
            origin,
        )
        .map_err(|error| eprintln!("public content cannot be kept here: {error}"))
        .ok()
    });
    let media = match public {
        Some(public) => {
            tilo_fs::document_media::DocumentMedia::with_public(Arc::clone(store), public)
        }
        None => tilo_fs::document_media::DocumentMedia::new(Arc::clone(store)),
    };
    match tilo_fs::document_media::serve_socket(socket, media) {
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

/// The path of a socket this daemon serves, removed when it stops serving it: a socket left
/// behind reads as a daemon that answers no one (`refresh-public-content` takes the blob
/// socket's presence for a running daemon).
struct ServedSocket(PathBuf);

impl Drop for ServedSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The content store for a mount that may serve the native backend: its file-backed content
/// lives on the store volume next to the workspace, so publishing a file and filling a
/// placeholder share extents instead of copying them.
async fn open_workspace_store(
    cache_dir: Option<PathBuf>,
    backend: &BackendArgs,
    workspace: uuid::Uuid,
) -> Result<ContentStore> {
    let Some(dir) = cache_dir else {
        return Ok(ContentStore::memory());
    };
    match native_content_dir(backend, workspace) {
        Some(files) => ContentStore::open_with_files(dir, &files).await,
        None => ContentStore::open(dir).await,
    }
}

/// `<store>/.content/<workspace>` when the native backend may run on a
/// dedicated store volume.
#[cfg(target_os = "linux")]
fn native_content_dir(backend: &BackendArgs, workspace: uuid::Uuid) -> Option<PathBuf> {
    if backend.backend == BackendChoice::Fuse
        || fs_native::store::check_dedicated(&backend.store).is_err()
    {
        return None;
    }
    Some(backend.store.join(".content").join(workspace.to_string()))
}

#[cfg(not(target_os = "linux"))]
fn native_content_dir(_backend: &BackendArgs, _workspace: uuid::Uuid) -> Option<PathBuf> {
    None
}

#[allow(
    clippy::too_many_lines,
    reason = "Serving assembles certified transport, ACLs, durable record services, namespace recovery, and mount task lifetimes in one ordered lifecycle."
)] // Linear CLI assembly keeps resource lifetimes explicit.
async fn serve(
    mount_point: PathBuf,
    cache_dir: Option<PathBuf>,
    seed: Option<PathBuf>,
    workspace_key_socket: Option<PathBuf>,
    sockets: SocketArgs,
    acl_args: AclArgs,
    backend: BackendArgs,
) -> Result<()> {
    let SocketArgs {
        storage_sock,
        blob_sock,
    } = sockets;
    let np = namespace_path(cache_dir.as_deref());
    let thumb_dir = thumb_path(cache_dir.as_deref());
    let availability = open_availability(cache_dir.as_deref())?;
    let eviction_queue = open_eviction_queue(cache_dir.as_deref())?;
    let paths = identity_paths();
    let device = device_public_key(&paths)?;
    let credentials = load_certified_mount_identity_once_valid(&paths, device).await?;
    let actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey(credentials.user_public_key()),
        device: collab_types::Ed25519PublicKey(credentials.device_public_key()),
    };
    let mount_actor = cache_dir.as_deref().map(|_| actor);
    // Files act as the certified person by default; device-specific grants use the device's
    // public key.
    let caps = resolve_acl(&acl_args, credentials.user_public_key())?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _reload = caps
        .clone()
        .map(|c| spawn_acl_reload(c, acl_args.acl_once))
        .transpose()?;
    let workspace_id = mount_id(credentials.device_public_key());
    let store = Arc::new(open_workspace_store(cache_dir.clone(), &backend, workspace_id).await?);
    let author = hex::encode(credentials.device_public_key());
    let fresh = np.as_ref().is_none_or(|dir| !namespace_kept(dir));
    // This mount's tree, as the Files resources of the person's own workspace, synced through
    // this device's workspace service, which the session serves the trees of. This daemon is the
    // app's endpoint, so what changes here reaches the person's other devices and the members of
    // a shared workspace. A tree kept here before the person's own workspace had it goes into it
    // as this device's write the first time the sync runs, signed as every other change of it is.
    let workspace =
        tilo_server_protocol::servers::person_workspace_id(&credentials.user_public_key());
    let trees = tree_socket(backend.native_owner.as_deref())?;
    let (tree, namespace, kept, service) = open_tree(
        np.as_deref(),
        trees,
        workspace,
        &credentials,
        author,
        &store,
        &mount_point,
    )
    .await?;
    let hydrator = Hydrator::new(store.clone());
    let hydrator = match &service {
        Some(service) => hydrator.with_source(service.source()),
        None => hydrator,
    };
    let hydrator = Arc::new(match (workspace_key_socket, &service) {
        (Some(socket), _) => {
            let (policy, reader) = acl.as_ref().context("workspace file keys require an ACL")?;
            anyhow::ensure!(
                backend.backend == BackendChoice::Fuse,
                "workspace file keys require FUSE"
            );
            hydrator.with_scoped_file_keys(socket, policy.clone(), *reader)
        }
        (None, Some(service)) if mount_point == Path::new(tilo_paths::WORKSPACE_MOUNT) => {
            let (policy, reader) = acl.as_ref().context("workspace file keys require an ACL")?;
            hydrator.with_tree_file_keys(service.clone(), policy.clone(), *reader)
        }
        (None, _) => hydrator,
    });
    // Who may read and write which folder is the session's to decide, from the workspace's signed
    // member list: what it says goes in force the moment it is said, and is kept for a start
    // while the session is away.
    let _access = match (&service, caps) {
        (Some(service), Some(caps)) => Some(follow_access(service.access(), caps)),
        _ => None,
    };

    if fresh {
        if let Some(seed) = seed {
            seed_from_dir(&namespace, &store, &seed).await?;
            println!("Seeded workspace from {}", seed.display());
        }
    } else if let Some(p) = &np {
        println!("Restored workspace from {}", p.display());
    }

    let media = blob_sock
        .as_deref()
        .and_then(|socket| spawn_document_media(socket, &store, cache_dir.as_deref()));
    let _media = media
        .as_ref()
        .and_then(|_| blob_sock.clone())
        .map(ServedSocket);
    let keeper = kept
        .clone()
        .map(|kept| spawn_keeper(namespace.clone(), kept));
    let shutdown = kept.clone().map(|kept| {
        let namespace = Arc::clone(&namespace);
        let store = Arc::clone(&store);
        Arc::new(move || {
            let _ = store.sync_pending();
            keep_now(&namespace, &kept);
        }) as Arc<dyn Fn() + Send + Sync>
    });
    let mount_outcome = run_mount(
        MountConfig {
            workspace_id,
            mount_point,
            kept: kept.clone(),
            shutdown,
            predictive_cache_root: cache_dir.clone(),
            thumb_dir,
            storage_sock,
            read_only: false,
            backend: Some((backend, *actor.device.as_bytes())),
        },
        MountServices {
            namespace: namespace.clone(),
            hydrator,
            acl,
            availability,
            eviction_queue,
            actor: mount_actor,
        },
    )
    .await?;
    let trace = |what: &str| {
        if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
            eprintln!("shutdown: {what}");
        }
    };
    trace("the mount returned; syncing content");
    let _ = store.sync_pending();
    trace("content synced; flushing the tree");
    if let Some(tree) = tree {
        tree.stop(TREE_STOPS_WITHIN).await;
    }
    final_flush(keeper, &namespace, kept.as_ref()).await;
    trace("tree flushed");
    let MountOutcome::Unmounted = mount_outcome;
    Ok(())
}

/// The id this device's mount keeps its content and state under: the device's key, as every
/// earlier run of it kept them.
fn mount_id(device: [u8; 32]) -> uuid::Uuid {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&device[..16]);
    uuid::Uuid::from_bytes(bytes)
}

/// Put each access list the session says in force, and keep it for the next start.
fn follow_access(
    mut told: tokio::sync::watch::Receiver<Option<String>>,
    caps: Arc<CapabilityFile>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let said = told.borrow_and_update().clone();
            if let Some(json) = said {
                match caps.keep(&json) {
                    Ok(n) => println!("Capability list from the session: {n} in force"),
                    Err(error) => eprintln!("the session's capability list: {error}"),
                }
            }
            if told.changed().await.is_err() {
                return;
            }
        }
    })
}

/// How long an unmount waits for the tree's sync to keep what it has.
const TREE_STOPS_WITHIN: Duration = Duration::from_secs(10);

/// A mount's tree of `workspace`: kept at `dir` and synced through this device's workspace
/// service, which the session serves the trees of; with no `dir`, an empty tree in memory for
/// this run alone, synced with nothing.
async fn open_tree(
    dir: Option<&Path>,
    trees: PathBuf,
    workspace: String,
    credentials: &tilo_fs::identity::PeerCredentials,
    author: String,
    store: &Arc<ContentStore>,
    mount_point: &Path,
) -> Result<(
    Option<tilo_fs::tree_sync::TreeSync>,
    Arc<Mutex<Namespace>>,
    Option<Arc<KeptTree>>,
    Option<tilo_fs::tree_sync::remote::Service>,
)> {
    let Some(dir) = dir else {
        let mount = MountAuthor::person(author, member(credentials.user_public_key()))
            .in_workspace(workspace);
        return Ok((
            None,
            Arc::new(Mutex::new(Namespace::new(mount)?)),
            None,
            None,
        ));
    };
    set_aside_older_namespace_file(dir)?;
    let (tree, service) = tilo_fs::tree_sync::remote::start(
        trees,
        workspace,
        (
            credentials.device_public_key(),
            credentials.user_public_key(),
        ),
        dir.to_path_buf(),
        Arc::clone(store),
        mount_point.to_path_buf(),
    )
    .await
    .with_context(|| format!("open the kept workspace tree at {}", dir.display()))?;
    let (namespace, kept) = (Arc::clone(&tree.namespace), tree.kept());
    Ok((Some(tree), namespace, Some(kept), Some(service)))
}

/// Where this device's daemon serves the trees of the workspaces it belongs to: among the sockets
/// of the daemon the mount's person reaches, the `--native-owner` (`UID:GID`) a machine's daemon
/// mounts for, else this account. A machine's daemon is root's, so its own account names no
/// person's daemon.
fn tree_socket(owner: Option<&str>) -> Result<PathBuf> {
    let account = match owner {
        Some(spec) => parse_owner(spec)?.0,
        None => rustix::process::geteuid().as_raw(),
    };
    tilo_device_identity::login::files_tree_socket(account).map_err(anyhow::Error::msg)
}

/// The directory the mount keeps its tree's documents in, each with the journal of the changes
/// it has made since it was last saved.
fn namespace_path(cache_dir: Option<&Path>) -> Option<PathBuf> {
    cache_dir.map(|d| d.join("namespace"))
}

/// Whether `dir` already holds kept documents: a workspace this device has opened before.
fn namespace_kept(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// Where host-generated thumbnails are cached (`thumbs/` under the content cache); `None` for an in-memory mount.
fn thumb_path(cache_dir: Option<&Path>) -> Option<PathBuf> {
    cache_dir.map(|d| d.join("thumbs"))
}

fn open_availability(
    cache_dir: Option<&Path>,
) -> Result<Option<Arc<Mutex<tilo_fs::AvailabilityRegistry>>>> {
    cache_dir
        .map(|dir| tilo_fs::AvailabilityRegistry::open(dir.join("availability.json")))
        .transpose()
        .map(|registry| registry.map(|registry| Arc::new(Mutex::new(registry))))
        .map_err(Into::into)
}

fn open_eviction_queue(
    cache_dir: Option<&Path>,
) -> Result<Option<Arc<tokio::sync::Mutex<tilo_fs::eviction::EvictionQueue>>>> {
    cache_dir
        .map(|dir| tilo_fs::eviction::EvictionQueue::open(dir.join("evictions.json")))
        .transpose()
        .map(|queue| queue.map(|queue| Arc::new(tokio::sync::Mutex::new(queue))))
        .map_err(Into::into)
}

type KeptTree = tilo_fs::kept_documents::KeptDocuments;

/// Save what changed now; a failure is reported, and the journals keep the changes until the
/// next save.
fn keep_now(namespace: &Arc<Mutex<Namespace>>, kept: &KeptTree) {
    if let Err(error) = kept.keep(namespace) {
        eprintln!("saving the workspace tree failed: {error}");
    }
}

/// Saves each document of the tree whole now and then while changes keep arriving
/// ([`tilo_fs::namespace_persist`]), letting its journal go; an idle tree costs nothing.
fn spawn_keeper(
    namespace: Arc<Mutex<Namespace>>,
    kept: Arc<KeptTree>,
) -> tokio::task::JoinHandle<()> {
    tilo_fs::namespace_persist::spawn_persister(namespace, move |namespace| {
        kept.keep(namespace)
            .inspect_err(|error| eprintln!("saving the workspace tree failed: {error}"))
            .map_err(Into::into)
    })
}

/// The last save of a mount, on shutdown and unmount: stops its keeper (waiting, so the two never
/// save at once), then saves what changed.
async fn final_flush(
    keeper: Option<tokio::task::JoinHandle<()>>,
    namespace: &Arc<Mutex<Namespace>>,
    kept: Option<&Arc<KeptTree>>,
) {
    let trace = |what: &str| {
        if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
            eprintln!("final flush: {what}");
        }
    };
    if let Some(keeper) = keeper {
        trace("stopping the keeper");
        keeper.abort();
        let _ = keeper.await;
        trace("keeper stopped");
    }
    if let Some(kept) = kept {
        trace("saving the tree");
        keep_now(namespace, kept);
        trace("tree saved");
    }
}

/// The member a person's root key is, as every workspace names them: the id their changes and
/// the nodes they make are read by.
fn member(key: [u8; 32]) -> tilo_actions::MemberId {
    tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(key))
}

/// The build before this kept the whole tree in `namespace.bin` beside the tree's parts kept
/// at `dir`; nothing reads it now, so it is set aside with the generations it kept rather than left
/// looking like state this mount keeps.
fn set_aside_older_namespace_file(dir: &Path) -> Result<()> {
    let legacy = dir.with_file_name("namespace.bin");
    if legacy.exists() {
        let kept = set_aside_older_namespace(&legacy)?;
        eprintln!(
            "namespace {} was kept by an earlier build in one file; kept it as {}; this mount \
             keeps the tree's documents at {}",
            legacy.display(),
            kept.display(),
            dir.display()
        );
    }
    Ok(())
}

/// Move the namespace file at `path`, and the directory of its retained generations, aside as
/// `<name>.older-image-<unix seconds>`; returns where the namespace went.
fn set_aside_older_namespace(path: &Path) -> Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let aside = |of: &Path| {
        let mut name = of.as_os_str().to_owned();
        name.push(format!(".older-image-{secs}"));
        PathBuf::from(name)
    };
    let kept = aside(path);
    std::fs::rename(path, &kept)
        .with_context(|| format!("set the older namespace {} aside", path.display()))?;
    let backups = tilo_fs::durable_backup::backup_dir(path)?;
    if backups.exists() {
        std::fs::rename(&backups, aside(&backups)).with_context(|| {
            format!(
                "set the older namespace's generations {} aside",
                backups.display()
            )
        })?;
    }
    Ok(kept)
}

/// Chooses the backend and, for Tier 1, its configuration. `None` means the FUSE backend
/// serves the workspace.
#[cfg(target_os = "linux")]
fn native_config(
    args: &BackendArgs,
    device: [u8; 32],
    workspace: uuid::Uuid,
    mount_point: &Path,
    cache_dir: Option<&Path>,
    read_only: bool,
) -> Result<Option<tilo_fs::native::NativeConfig>> {
    use tilo_fs::native::Backend;
    let choice = match args.backend {
        BackendChoice::Auto => Backend::Auto,
        BackendChoice::Fuse => Backend::Fuse,
        BackendChoice::Native => Backend::Native,
    };
    let selection = match (cache_dir, read_only) {
        (_, true) => tilo_fs::native::TierSelection {
            native: false,
            store: args.store.clone(),
            reasons: vec!["read-only mounts are served by FUSE".into()],
            report: None,
            probe_time: Duration::ZERO,
        },
        (None, _) => tilo_fs::native::TierSelection {
            native: false,
            store: args.store.clone(),
            reasons: vec!["no cache directory to keep the native backend's state in".into()],
            report: None,
            probe_time: Duration::ZERO,
        },
        (Some(_), false) => tilo_fs::native::tier::select(choice, &args.store),
    };
    println!("{}", selection.summary());
    if let Some(report) = &selection.report {
        eprint!("{report}");
    }
    if !selection.native {
        if choice == Backend::Native {
            bail!(
                "the native backend was requested but is unavailable: {}",
                selection.reasons.join("; ")
            );
        }
        return Ok(None);
    }
    let Some(cache_dir) = cache_dir else {
        return Ok(None);
    };
    let mut cfg = tilo_fs::native::NativeConfig::new(
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

/// Serves the workspace from the native tree until interrupted or terminated. The
/// bind mount lives exactly as long as this call.
#[cfg(target_os = "linux")]
async fn run_native(
    fs: QuickFS,
    cfg: tilo_fs::native::NativeConfig,
    stop: &mut StopSignals,
) -> Result<MountOutcome> {
    let fs = Arc::new(fs);
    let content = Arc::clone(&fs.hydrator);
    let hydrator = Arc::clone(&fs.hydrator);
    let runtime = tokio::runtime::Handle::current();
    let backend = tokio::task::spawn_blocking(move || {
        tilo_fs::native::NativeBackend::start(cfg, fs, content, runtime)
    })
    .await??;
    let outcome = tokio::select! {
        signal = stop.recv() => {
            println!("{signal}, exiting...");
            MountOutcome::Unmounted
        }
        () = backend.finished() => {
            eprintln!("the native backend stopped");
            MountOutcome::Unmounted
        }
    };
    tokio::task::spawn_blocking(move || backend.stop()).await??;
    let (ranged, blobs) = hydrator.received_bytes();
    println!(
        "Content received from the workspace: {ranged} bytes in ranges, {blobs} bytes as blobs"
    );
    println!("Workspace unmounted.");
    Ok(outcome)
}

/// The signals that end a mount cleanly: SIGINT from a terminal, SIGTERM from systemd's stop,
/// SIGHUP when the controlling terminal goes away. The default action of each kills the process at
/// once, before the caller's final namespace save, so a write acknowledged in the last
/// [`tilo_fs::namespace_persist::QUIET`] was lost. A handler, once installed, stays installed: a signal
/// that arrives after the mount ended no longer cuts that save short.
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

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "Interrupted",
            _ = self.terminate.recv() => "Terminated",
            _ = self.hangup.recv() => "Hung up",
        }
    }
}

async fn seed_from_dir(
    namespace: &Arc<Mutex<Namespace>>,
    store: &Arc<ContentStore>,
    root: &Path,
) -> Result<()> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os_file(rel: &str) -> PathBuf {
        // This crate lives in crates/core/fs; shipped image inputs remain
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

    tilo_test_env::scenario! {
        fn the_shipped_units_launch_and_serve_with_their_seeded_capability_lists;

        phase host_files_unit_runs_the_primary_launch_check;
        {
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
            // The unit starts only for a cache the person already prepared.
            let condition: Vec<&str> = unit
                .lines()
                .find_map(|line| line.strip_prefix("ExecCondition="))
                .expect("ExecCondition=")
                .split_whitespace()
                .collect();
            let cli = Cli::try_parse_from(std::iter::once("fs").chain(condition[1..].iter().copied()))
                .expect("the host Files unit condition must parse");
            assert!(
                matches!(
                    cli.command,
                    Command::WorkspaceStatus {
                        require_existing: true,
                        ..
                    }
                ),
                "the host unit must not start for an unprepared cache"
            );
        }

        phase shipped_units_serve_with_a_seeded_enforced_capability_list;
        {
            {
                let unit = std::fs::read_to_string(os_file("os/usr/lib/systemd/system/fs.service"))
                    .expect("the shipped fs.service must exist");
                assert!(
                    unit.lines().any(|line| line.trim() == "Restart=always"),
                    "a daemon that stops must be restarted by the shipped service"
                );
                assert!(
                    service_exec_start()
                        .windows(2)
                        .any(|pair| { pair[0].ends_with("fs") && pair[1] == "serve" }),
                    "the shipped unit must enter the serve startup path"
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
                    !acl.acl_once,
                    "the shipped mount must re-read its capability list; /var/workspace is mounted once \
                 at boot, so a list read only at mount time can never carry a later grant"
                );
                assert_eq!(
                    acl.members,
                    vec!["person:local".to_string()],
                    "the shipped list is written against a member, because the mount's own identity \
                 is the certificate of the device key made on first boot, which no shipped file \
                 could name in advance"
                );
            }
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

                let containerfile = std::fs::read_to_string(os_file("os/Containerfile"))
                    .expect("the production Containerfile must exist");
                assert!(
                    containerfile.lines().any(|l| l.starts_with("COPY ")
                        && l.contains("/usr/share/tilo/fs-capabilities.json")),
                    "os/Containerfile must ship the default capability list that tmpfiles.d seeds from"
                );
            }
            {
                use tilo_fs::Access;

                let path = os_file("os/usr/share/tilo/fs-capabilities.json");
                let identity: IdentityKey = [0x5A; 32]; // stands in for the per-start node key
                let file =
                    CapabilityFile::load(&path, identity, vec![parse_member("person:local").unwrap()])
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

                // The grant is the actor's, not the mount's — a mount not presenting `person:local` gets nothing, which narrows a peer-joined mount.
                let outsider = CapabilityFile::load(&path, identity, Vec::new())
                    .expect("loads")
                    .handle();
                assert!(
                    !outsider.permits(&identity, "/notes/today.md", Access::Read),
                    "the shipped list must grant its named actor, not whoever mounts"
                );
            }
        }

        phase a_capability_list_an_older_image_seeded_gives_way_to_the_one_the_image_ships;
        // tmpfiles' `C` never overwrites, so a machine keeps the capability list of the image it was
        // installed with across every update. Every version in the history must mount with its grant
        // through the same loader fs.service uses.
        {
            use tilo_fs::Access;

            let history = os_file("os/tests/seeded-state/var/lib/tilo/fs-capabilities.json");
            let mut versions: Vec<_> = std::fs::read_dir(&history)
                .expect("the seeded capability list history must exist")
                .map(|entry| entry.expect("history entry").path())
                .collect();
            versions.sort();
            let shipped = std::fs::read(os_file("os/usr/share/tilo/fs-capabilities.json"))
                .expect("the shipped default capability list must exist");
            assert!(
                versions
                    .iter()
                    .any(|path| std::fs::read(path).is_ok_and(|bytes| bytes == shipped)),
                "the history must hold the list the image ships today: {versions:?}"
            );
            let identity: IdentityKey = [0x5A; 32];
            let local = vec![parse_member("person:local").unwrap()];
            for seeded in &versions {
                // Every shipped list keeps its grants through an update.
                let machine = tempfile::tempdir().unwrap();
                let path = &machine.path().join("fs-capabilities.json");
                std::fs::copy(seeded, path).unwrap();
                let file = CapabilityFile::load(path, identity, local.clone())
                    .unwrap_or_else(|error| {
                        panic!("{} must load on an upgraded machine: {error}", seeded.display())
                    });
                let handle = file.handle();
                assert!(
                    handle.permits(&identity, "/notes/today.md", Access::Write),
                    "{} must let the local session write its workspace",
                    path.display()
                );
                let outsider = CapabilityFile::load(path, identity, Vec::new())
                    .expect("loads")
                    .handle();
                assert!(
                    !outsider.permits(&identity, "/notes/today.md", Access::Read),
                    "{} must grant only its named actor",
                    path.display()
                );
            }
        }
    }

    tilo_test_env::scenario! {
        fn the_command_line_parses_mounts_and_binds_acl_flags;

        phase acl_flags_bind_an_enforced_list_or_refuse_the_mount;
        {
            {
                let resolved =
                    resolve_acl(&acl_args_from(&[]), NODE_KEY).expect("no ACL is not an error");
                assert!(
                    resolved.is_none(),
                    "omitting --acl must leave the mount unbound, not bind an empty ACL"
                );
            }
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
                    format!("{err:#}").contains("is not a file access list"),
                    "expected a parse failure, got: {err:#}"
                );
            }
            {
                use tilo_fs::Access;

                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                std::fs::write(&path, br#"{"capabilities":[]}"#).unwrap();
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
                use collab_types::{Access as CapAccess, CapOverride, Effect, Scope};
                use tilo_fs::Access;

                let who = [0x0A; 32];
                let caps = vec![CapOverride::new(
                    tilo_action_policy::Grantee::person(hex::encode(who)),
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
            {
                let args = acl_args_from(&["--identity", &hex::encode([0x0A; 32])]);
                assert!(resolve_acl(&args, NODE_KEY).is_err());
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                std::fs::write(&path, br#"{"capabilities":[]}"#).unwrap();
                let args = acl_args_from(&["--acl", path.to_str().unwrap(), "--identity", "abcd"]);
                let err = resolve_acl(&args, NODE_KEY).expect_err("a 2-byte identity is not a key");
                assert!(format!("{err:#}").contains("32 bytes"), "got: {err:#}");
            }
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

    /// systemd stops `fs.service` with SIGTERM; SIGHUP arrives when a terminal running the daemon
    /// closes. Once a mount installed its stop signals, each of them, like SIGINT, reaches the stop
    /// path, which unmounts and makes the final namespace save.
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
