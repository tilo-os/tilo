use anyhow::Result;
use clap::{Parser, Subcommand};
use fs::namespace::ROOT_INO;
use fs::sync::{self, SyncHandle};
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS, Ticket, Workspace};
use fuser::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
        } => serve(mount_point, cache_dir, seed).await,
        Command::Mount {
            mount_point,
            ticket,
            cache_dir,
        } => mount(mount_point, ticket, cache_dir).await,
    }
}

async fn open_store(cache_dir: Option<PathBuf>) -> Result<ContentStore> {
    match cache_dir {
        Some(dir) => ContentStore::open(dir).await,
        None => Ok(ContentStore::memory()),
    }
}

async fn serve(mount_point: PathBuf, cache_dir: Option<PathBuf>, seed: Option<PathBuf>) -> Result<()> {
    let (endpoint, lookup) = sync::bind_endpoint().await?;
    let store = Arc::new(open_store(cache_dir).await?);
    let namespace = Arc::new(Mutex::new(Namespace::new(endpoint.id().to_string())));
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));

    if let Some(seed) = seed {
        seed_from_dir(&namespace, &store, &seed).await?;
        println!("Seeded workspace from {seed:?}");
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

    run_mount(mount_point, namespace, hydrator, workspace.sync.clone()).await?;
    drop(workspace); // keep the sync tasks alive until we unmount
    Ok(())
}

async fn mount(mount_point: PathBuf, ticket: String, cache_dir: Option<PathBuf>) -> Result<()> {
    let ticket = Ticket::decode(&ticket)?;
    let (endpoint, lookup) = sync::bind_endpoint().await?;
    let store = Arc::new(open_store(cache_dir).await?);
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));

    // Bootstrap the shared document from the host before joining.
    let doc = sync::fetch_remote_doc(&endpoint, ticket.node.clone()).await?;
    let namespace = Arc::new(Mutex::new(Namespace::from_bytes(
        endpoint.id().to_string(),
        &doc,
    )?));

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

    run_mount(mount_point, namespace, hydrator, workspace.sync.clone()).await?;
    drop(workspace);
    Ok(())
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
                let size = data.len() as u64;
                let hash = if data.is_empty() {
                    None
                } else {
                    Some(store.ingest(bytes::Bytes::from(data)).await?)
                };
                namespace
                    .lock()
                    .unwrap()
                    .create_file(parent_ino, &name, hash.as_ref(), size, now_ms());
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
