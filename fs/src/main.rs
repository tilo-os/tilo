use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use collab_types::{CapOverride, Principal};
use fs::namespace::ROOT_INO;
use fs::sync::{self, SyncHandle};
use fs::{
    ContentStore, Heuristics, Hydrator, IdentityKey, Namespace, QuickFS, Ticket, Workspace,
    WorkspaceAcl,
};
use fuser::*;
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
///
/// Shared by both subcommands: whether you host a workspace or join
/// someone else's, the file-access question is the same one.
#[derive(Args, Debug, Clone, Default)]
struct AclArgs {
    /// Enforce this capability list. A JSON file holding an array of
    /// capability records (`collab_types::CapOverride`), which is the
    /// system's one permission vocabulary.
    ///
    /// Without this flag the mount keeps the Unix-veneer-only behaviour.
    /// *With* it, the file must load: a missing or malformed file aborts
    /// the mount rather than silently serving an unenforced workspace.
    #[arg(long, value_name = "FILE")]
    acl: Option<PathBuf>,
    /// The identity this mount acts as: 64 hex characters (a 32-byte
    /// Ed25519 public key). Defaults to this node's own key.
    #[arg(long, value_name = "HEX")]
    identity: Option<String>,
    /// A principal this mount can prove it is, beyond its own person id —
    /// `room:<id>`, `app:<id>`, `agent:<id>`, `token:<id>` or
    /// `person:<id>`. Repeatable.
    #[arg(long = "principal", value_name = "KIND:ID")]
    principals: Vec<String>,
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
        /// Serve the document control socket here (apps fine-grained-edit
        /// documents collaboratively).
        #[arg(long)]
        control_sock: Option<PathBuf>,
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
            acl,
        } => serve(mount_point, cache_dir, seed, control_sock, acl).await,
        Command::Mount {
            mount_point,
            ticket,
            cache_dir,
            control_sock,
            acl,
        } => mount(mount_point, ticket, cache_dir, control_sock, acl).await,
    }
}

/// Parse a 64-character hex identity into the 32-byte key it names.
fn parse_identity(hex_str: &str) -> Result<IdentityKey> {
    let raw = hex::decode(hex_str.trim())
        .with_context(|| format!("identity {hex_str:?} is not hex"))?;
    let key: IdentityKey = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity must be 32 bytes (64 hex chars), got {}", raw.len()))?;
    Ok(key)
}

/// Parse a `kind:id` principal argument.
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

/// Resolve `--acl`/`--identity`/`--principal` into the ACL the mount
/// enforces, or `None` when no capability list was requested.
///
/// Fail-closed on every error path: if enforcement was asked for and
/// anything about it does not resolve — the file is missing, the JSON is
/// malformed, the identity is not a key — the mount is refused. Silently
/// falling back to an unenforced mount would turn a typo in a systemd
/// unit into a wide-open workspace, which is precisely the failure this
/// enforcement point exists to prevent. Omitting `--acl` entirely is the
/// documented opt-out and keeps the pre-existing behaviour, so
/// single-user mounts are unaffected.
fn resolve_acl(args: &AclArgs, node_key: IdentityKey) -> Result<Option<(WorkspaceAcl, IdentityKey)>> {
    let Some(path) = &args.acl else {
        if args.identity.is_some() || !args.principals.is_empty() {
            bail!("--identity/--principal require --acl; without a capability list they enforce nothing");
        }
        return Ok(None);
    };

    let identity = match &args.identity {
        Some(hex_str) => parse_identity(hex_str)?,
        None => node_key,
    };
    let principals = args
        .principals
        .iter()
        .map(|s| parse_principal(s))
        .collect::<Result<Vec<_>>>()?;

    let raw = std::fs::read(path)
        .with_context(|| format!("capability list {path:?} could not be read"))?;
    let caps: Vec<CapOverride> = serde_json::from_slice(&raw)
        .with_context(|| format!("capability list {path:?} is not a JSON array of capabilities"))?;

    // `now: None` ⇒ evaluate expiry against the live clock on every
    // check, so a time-limited capability really does lapse mid-mount.
    let acl = WorkspaceAcl::from_capabilities(identity, &caps, &principals, None);
    println!(
        "Enforcing {} capabilities from {path:?} as identity {}",
        caps.len(),
        hex::encode(identity)
    );
    Ok(Some((acl, identity)))
}

/// Bind and serve the document control socket (best-effort).
fn spawn_control(
    sock: PathBuf,
    namespace: Arc<Mutex<Namespace>>,
    sync: Arc<SyncHandle>,
) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(&sock); // clear a stale socket
    match tokio::net::UnixListener::bind(&sock) {
        Ok(listener) => {
            // The socket can read and edit every document — restrict to the owner.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
            println!("Document control socket at {sock:?}");
            tokio::spawn(fs::control::serve(listener, namespace, Some(sync)))
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
    acl_args: AclArgs,
) -> Result<()> {
    let np = namespace_path(&cache_dir);
    let (endpoint, lookup) = sync::bind_endpoint().await?;
    // This node's own key is the mount's identity unless one is named.
    let acl = resolve_acl(&acl_args, *endpoint.id().as_bytes())?;
    let store = Arc::new(open_store(cache_dir).await?);
    let author = endpoint.id().to_string();

    // Restore a previously-persisted workspace, or start fresh (+ optional seed).
    let (ns_value, fresh) = match np.as_ref().filter(|p| p.exists()) {
        Some(p) => (Namespace::from_bytes(author.clone(), &std::fs::read(p)?)?, false),
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
    )
    .await?;

    println!("Iroh NodeID: {}", endpoint.id());
    println!("Hosting workspace — share this ticket to invite peers:\n{}", ticket.encode());

    let _control =
        control_sock.map(|s| spawn_control(s, namespace.clone(), workspace.sync.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    run_mount(
        mount_point,
        namespace.clone(),
        hydrator,
        workspace.sync.clone(),
        acl,
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
    acl_args: AclArgs,
) -> Result<()> {
    let ticket = Ticket::decode(&ticket)?;
    let np = namespace_path(&cache_dir);
    let (endpoint, lookup) = sync::bind_endpoint().await?;
    let acl = resolve_acl(&acl_args, *endpoint.id().as_bytes())?;
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
    )
    .await?;

    println!("Iroh NodeID: {}", endpoint.id());
    println!("Joined workspace.");

    let _control =
        control_sock.map(|s| spawn_control(s, namespace.clone(), workspace.sync.clone()));
    let persist = np.clone().map(|p| spawn_persist(namespace.clone(), p));
    run_mount(
        mount_point,
        namespace.clone(),
        hydrator,
        workspace.sync.clone(),
        acl,
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

/// Atomically persist the namespace document to `path` (write-temp + rename).
fn persist_now(namespace: &Arc<Mutex<Namespace>>, path: &Path) {
    let bytes = {
        namespace.lock().unwrap().save()
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
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

/// Open a joiner's namespace: load any persisted copy (offline-capable), then
/// merge the host's current document if reachable. Fails only if there's
/// neither a persisted copy nor a reachable host.
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
    acl: Option<(WorkspaceAcl, IdentityKey)>,
) -> Result<()> {
    if !mount_point.exists() {
        std::fs::create_dir_all(&mount_point)?;
    }

    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let fs = QuickFS::new(namespace, hydrator, heuristics).with_sync(sync);
    // Bind the capability list, if one was given, before the mount is
    // live — every open/read/write on this mount is then checked as the
    // bound identity.
    let fs = match acl {
        Some((acl, identity)) => fs.with_acl(acl, identity),
        None => fs,
    };

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("quickfs".to_string()),
        MountOption::AutoUnmount,
    ];
    config.acl = SessionACL::All;

    let mp = mount_point.clone();
    let mount_task = tokio::task::spawn_blocking(move || fuser::mount2(fs, &mp, &config));
    println!("Mounted QuickFS at {mount_point:?}");

    // Exit cleanly on Ctrl-C or if the mount terminates on its own (don't hang
    // a crash-looping service forever on a failed mount).
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
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
                match codec {
                    Some(c) => {
                        // Recognized rich-text format → seed as a CRDT document.
                        let mut ns = namespace.lock().unwrap();
                        if let Some(ino) = ns.create_document(parent_ino, &name, &c, now_ms()) {
                            ns.import_document(ino, &registry, &data, now_ms());
                        }
                    }
                    None => {
                        let size = data.len() as u64;
                        let hash = store.write_content(&data).await?;
                        namespace
                            .lock()
                            .unwrap()
                            .create_file(parent_ino, &name, hash.as_ref(), size, now_ms());
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_command_line_parses() {
        // The exact line fs.service runs. This used to crash-loop on boot
        // because `--cache-dir` was not a recognized argument (clap exit 2).
        let cli = Cli::try_parse_from([
            "fs",
            "serve",
            "--mount-point",
            "/var/workspace",
            "--cache-dir",
            "/var/cache/quick-fs",
        ])
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

    // No `--acl` ⇒ no capability list ⇒ the mount keeps the
    // Unix-veneer-only behaviour. This is the documented opt-out and the
    // reason the shipped fs.service line still works unchanged.
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

    // An empty capability list is a valid, total denial — not an error,
    // and emphatically not a fallback to the unenforced mount.
    #[test]
    fn an_empty_capability_file_binds_a_denying_acl() {
        use fs::Access;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        std::fs::write(&path, b"[]").unwrap();
        let args = acl_args_from(&["--acl", path.to_str().unwrap()]);
        let (acl, identity) = resolve_acl(&args, NODE_KEY)
            .expect("an empty list is valid")
            .expect("--acl was given, so an ACL must be bound");
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
        let (acl, identity) = resolve_acl(&args, NODE_KEY).unwrap().unwrap();
        assert_eq!(identity, who, "--identity overrides the node key");
        assert!(acl.permits(&who, "/w/docs/a.md", Access::Read));
        assert!(!acl.permits(&who, "/w/docs/a.md", Access::Write));
        assert!(!acl.permits(&who, "/w/other", Access::Read));
    }

    // `--identity` alone enforces nothing; accepting it silently would
    // read as "this mount is now identity-scoped" when it is not.
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
