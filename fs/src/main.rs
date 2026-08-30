use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use collab_types::Principal;
use fs::namespace::ROOT_INO;
use fs::sync::{self, SyncHandle};
use fs::{
    AclHandle, CapabilityFile, ContentStore, Heuristics, Hydrator, IdentityKey, Namespace, QuickFS,
    Refresh, Ticket, Workspace, identity_paths, load_mount_identity,
};
use fuser::{Config, MountOption, SessionACL};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(author, version, about = "qOS collaborative filesystem", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// How the mount enforces per-identity file access.
#[derive(Args, Debug, Clone, Default)]
struct AclArgs {
    /// Enforce this capability list (JSON array of `collab_types::CapOverride`); a load failure aborts the mount.
    #[arg(long, value_name = "FILE")]
    acl: Option<PathBuf>,
    /// The identity this mount acts as: 64 hex chars (Ed25519 public key). Defaults to this node's own key.
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
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
/// the mount rather than falling back to unenforced; `device_key` is the default subject.
fn resolve_acl(args: &AclArgs, device_key: IdentityKey) -> Result<Option<Arc<CapabilityFile>>> {
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
        None => device_key,
    };
    let principals = args
        .principals
        .iter()
        .map(|s| parse_principal(s))
        .collect::<Result<Vec<_>>>()?;

    let file = CapabilityFile::load(path, identity, principals)
        .with_context(|| format!("binding the capability list {path:?}"))?;
    println!(
        "Enforcing {} capabilities from {path:?} as identity {}",
        file.count(),
        hex::encode(identity)
    );
    Ok(Some(Arc::new(file)))
}

/// Re-read the capability list every `every` seconds and swap it into the live mount (`0` disables reload).
fn spawn_acl_reload(file: Arc<CapabilityFile>, every: u64) -> Option<tokio::task::JoinHandle<()>> {
    if every == 0 {
        println!(
            "Capability list {:?} will NOT be re-read; a later grant needs a remount",
            file.path()
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
                    println!("Capability list {:?} reloaded: {n} in force", file.path());
                }
                Refresh::Revoked => eprintln!(
                    "Capability list {:?} is gone; denying every access until it returns",
                    file.path()
                ),
                Refresh::Failed(e) => eprintln!(
                    "Capability list {:?} could not be re-read ({e}); the previously loaded list \
                     stays in force",
                    file.path()
                ),
            }
        }
    }))
}

/// Bind and serve the document control socket (best-effort), using the same ACL/identity as the mount.
fn spawn_control(
    sock: PathBuf,
    namespace: Arc<Mutex<Namespace>>,
    sync: Arc<SyncHandle>,
    enforcement: fs::control::Enforcement,
) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(&sock); // clear a stale socket
    match tokio::net::UnixListener::bind(&sock) {
        Ok(listener) => {
            // The socket can read and edit every document — restrict to the owner.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
            println!("Document control socket at {sock:?}");
            tokio::spawn(fs::control::serve(
                listener,
                namespace,
                Some(sync),
                enforcement,
            ))
        }
        Err(e) => {
            eprintln!("could not bind control socket {sock:?}: {e}");
            tokio::spawn(async {})
        }
    }
}

async fn open_store(cache_dir: Option<PathBuf>) -> Result<ContentStore> {
    match cache_dir {
        Some(dir) => ContentStore::open(dir).await,
        None => Ok(ContentStore::memory()),
    }
}

async fn serve(
    mount_point: PathBuf,
    cache_dir: Option<PathBuf>,
    seed: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
    acl_args: AclArgs,
) -> Result<()> {
    let np = namespace_path(&cache_dir);
    let thumb_dir = thumb_path(&cache_dir);
    let device = load_mount_identity(&identity_paths())?;
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    // This device's own key is the mount's identity unless one is named.
    let caps = resolve_acl(&acl_args, device.public)?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _reload = caps.map(|c| spawn_acl_reload(c, acl_args.acl_reload_secs));
    let store = Arc::new(open_store(cache_dir).await?);
    let author = endpoint.id().to_string();

    // Restore a previously-persisted workspace, or start fresh (+ optional seed).
    let (ns_value, fresh) = match np.as_ref().filter(|p| p.exists()) {
        Some(p) => (
            Namespace::from_bytes(author.clone(), &std::fs::read(p)?)?,
            false,
        ),
        None => (Namespace::new(author.clone()), true),
    };
    let namespace = Arc::new(Mutex::new(ns_value));
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));

    if fresh {
        if let Some(seed) = seed {
            seed_from_dir(&namespace, &store, &seed).await?;
            println!("Seeded workspace from {seed:?}");
        }
    } else {
        println!("Restored workspace from {:?}", np.as_ref().unwrap());
    }

    let (workspace, ticket) = Workspace::host(
        endpoint.clone(),
        lookup,
        store.clone(),
        hydrator.clone(),
        namespace.clone(),
        acl.as_ref().map(|(handle, _)| handle.clone()),
    )
    .await?;

    println!("Iroh NodeID: {}", endpoint.id());
    println!(
        "Hosting workspace — share this ticket to invite peers:\n{}",
        ticket.encode()
    );

    let _control = control_sock
        .map(|s| spawn_control(s, namespace.clone(), workspace.sync.clone(), acl.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    run_mount(
        mount_point,
        namespace.clone(),
        hydrator,
        workspace.sync.clone(),
        acl,
        thumb_dir,
        storage_sock,
    )
    .await?;
    if let Some(p) = &np {
        persist_now(&namespace, p); // final flush
    }
    if let Some(h) = persist {
        h.abort();
    }
    drop(workspace);
    Ok(())
}

async fn mount(
    mount_point: PathBuf,
    ticket: String,
    cache_dir: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
    acl_args: AclArgs,
) -> Result<()> {
    let ticket = Ticket::decode(&ticket)?;
    let np = namespace_path(&cache_dir);
    let thumb_dir = thumb_path(&cache_dir);
    let device = load_mount_identity(&identity_paths())?;
    let (endpoint, lookup) = sync::bind_endpoint_with_key(device.secret.clone()).await?;
    let caps = resolve_acl(&acl_args, device.public)?;
    let acl = caps.as_ref().map(|c| (c.handle(), c.identity()));
    let _reload = caps.map(|c| spawn_acl_reload(c, acl_args.acl_reload_secs));
    let store = Arc::new(open_store(cache_dir).await?);
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let author = endpoint.id().to_string();

    let ns_value = open_namespace_mount(author, &np, &endpoint, ticket.node.clone()).await?;
    let namespace = Arc::new(Mutex::new(ns_value));

    let workspace = Workspace::join(
        endpoint.clone(),
        lookup,
        store.clone(),
        hydrator.clone(),
        namespace.clone(),
        ticket,
        acl.as_ref().map(|(handle, _)| handle.clone()),
    )
    .await?;

    println!("Iroh NodeID: {}", endpoint.id());
    println!("Joined workspace.");

    let _control = control_sock
        .map(|s| spawn_control(s, namespace.clone(), workspace.sync.clone(), acl.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    run_mount(
        mount_point,
        namespace.clone(),
        hydrator,
        workspace.sync.clone(),
        acl,
        thumb_dir,
        storage_sock,
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

fn namespace_path(cache_dir: &Option<PathBuf>) -> Option<PathBuf> {
    cache_dir.as_ref().map(|d| d.join("namespace.bin"))
}

/// Where host-generated thumbnails are cached (`thumbs/` under the content cache); `None` for an in-memory mount.
fn thumb_path(cache_dir: &Option<PathBuf>) -> Option<PathBuf> {
    cache_dir.as_ref().map(|d| d.join("thumbs"))
}

/// Atomically persist the namespace document to `path` (write-temp + rename).
fn persist_now(namespace: &Arc<Mutex<Namespace>>, path: &Path) {
    let bytes = { namespace.lock().unwrap().save() };
    if let Err(e) = atomic_file::write(path, &bytes) {
        eprintln!("namespace persist to {} failed: {e}", path.display());
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
    np: &Option<PathBuf>,
    endpoint: &iroh::endpoint::Endpoint,
    host: iroh_base::EndpointAddr,
) -> Result<Namespace> {
    let persisted = np
        .as_ref()
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read(p).ok());

    match sync::fetch_remote_doc(endpoint, host).await {
        Ok(doc) => match persisted {
            Some(bytes) => {
                let mut ns = Namespace::from_bytes(author, &bytes)?;
                ns.merge_remote_and_resolve(&doc)?;
                Ok(ns)
            }
            None => Ok(Namespace::from_bytes(author, &doc)?),
        },
        Err(e) => match persisted {
            Some(bytes) => {
                eprintln!("host unreachable ({e}); starting from persisted workspace");
                Ok(Namespace::from_bytes(author, &bytes)?)
            }
            None => Err(e),
        },
    }
}

/// Mount the FUSE filesystem and run until interrupted (or the mount exits).
async fn run_mount(
    mount_point: PathBuf,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sync: Arc<SyncHandle>,
    acl: Option<(AclHandle, IdentityKey)>,
    thumb_dir: Option<PathBuf>,
    storage_sock: Option<PathBuf>,
) -> Result<()> {
    if !mount_point.exists() {
        std::fs::create_dir_all(&mount_point)?;
    }

    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let fs = QuickFS::new(namespace, hydrator, heuristics).with_sync(sync);
    let storage_authority = storage_sock.as_ref().map(|_| {
        fs::storage::StorageAuthority::new(
            fs.namespace.clone(),
            fs.hydrator.clone(),
            fs.hydrator.store.clone(),
            fs.write_buffers_handle(),
            "Current workspace",
        )
    });
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
                eprintln!("thumbnail cache unavailable at {dir:?} ({e}); previews disabled");
                fs
            }
        },
        None => fs,
    };

    let _storage = storage_sock
        .zip(storage_authority)
        .map(|(sock, authority)| {
            let _ = std::fs::remove_file(&sock);
            match tokio::net::UnixListener::bind(&sock) {
                Ok(listener) => {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o660));
                    println!("Storage accounting socket at {sock:?}");
                    tokio::spawn(fs::storage::serve(listener, authority))
                }
                Err(error) => {
                    eprintln!("could not bind storage socket {sock:?}: {error}");
                    tokio::spawn(async {})
                }
            }
        });

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("quickfs".to_string()),
        MountOption::AutoUnmount,
    ];
    config.acl = SessionACL::All;

    let mp = mount_point.clone();
    let mount_task = tokio::task::spawn_blocking(move || fuser::mount2(fs, &mp, &config));
    println!("Mounted QuickFS at {mount_point:?}");

    // Exit cleanly on Ctrl-C or mount exit (don't hang a crash-looping service forever).
    tokio::select! {
        res = mount_task => match res {
            Ok(Ok(())) => println!("Filesystem unmounted."),
            Ok(Err(e)) => eprintln!("FUSE mount exited with error: {e}"),
            Err(e) => eprintln!("mount task failed: {e}"),
        },
        _ = tokio::signal::ctrl_c() => println!("Interrupted, exiting..."),
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
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
                    .unwrap()
                    .create_dir(parent_ino, &name, now_ms())
                    .expect("create_dir under known parent");
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
                    let mut ns = namespace.lock().unwrap();
                    if let Some(ino) = ns.create_document(parent_ino, &name, &c, now_ms()) {
                        ns.import_document(ino, &registry, &data, now_ms());
                    }
                } else {
                    let size = data.len() as u64;
                    let hash = store.write_content(&data).await?;
                    namespace.lock().unwrap().create_file(
                        parent_ino,
                        &name,
                        hash.as_ref(),
                        size,
                        now_ms(),
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
            other => panic!("expected serve subcommand, got {other:?}"),
        }
    }

    // An enforcement point shipped switched off is not an enforcement point, so the unit is asserted on directly.
    #[test]
    fn the_shipped_service_enforces_a_capability_list() {
        let argv = service_exec_start();
        let cli = Cli::try_parse_from(std::iter::once("fs".to_string()).chain(argv[1..].to_vec()))
            .expect("fs.service command line must parse");
        let acl = match cli.command {
            Command::Serve { acl, .. } => acl,
            other => panic!("expected serve, got {other:?}"),
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
            other => panic!("expected serve, got {other:?}"),
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
