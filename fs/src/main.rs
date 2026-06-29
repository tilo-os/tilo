use anyhow::Result;
use clap::{Parser, Subcommand};
use fs::namespace::ROOT_INO;
use fs::sync::{self, SyncHandle};
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS, Ticket, Workspace};
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
        } => serve(mount_point, cache_dir, seed, control_sock).await,
        Command::Mount {
            mount_point,
            ticket,
            cache_dir,
            control_sock,
        } => mount(mount_point, ticket, cache_dir, control_sock).await,
    }
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
) -> Result<()> {
    let np = namespace_path(&cache_dir);
    let (endpoint, lookup) = sync::bind_endpoint().await?;
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
    run_mount(mount_point, namespace.clone(), hydrator, workspace.sync.clone()).await?;
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
) -> Result<()> {
    let ticket = Ticket::decode(&ticket)?;
    let np = namespace_path(&cache_dir);
    let (endpoint, lookup) = sync::bind_endpoint().await?;
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
    run_mount(mount_point, namespace.clone(), hydrator, workspace.sync.clone()).await?;
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
) -> Result<()> {
    if !mount_point.exists() {
        std::fs::create_dir_all(&mount_point)?;
    }

    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let fs = QuickFS::new(namespace, hydrator, heuristics).with_sync(sync);

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
}
