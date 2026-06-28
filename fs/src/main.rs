use anyhow::Result;
use clap::Parser;
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS};
use fuser::*;
use iroh::endpoint::Endpoint;
use iroh::protocol::Router;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Where to mount the collaborative filesystem.
    #[arg(short, long)]
    mount_point: PathBuf,

    /// Persistent content cache directory (the iroh-blobs store root).
    /// In-memory if omitted.
    #[arg(short, long)]
    cache_dir: Option<PathBuf>,

    /// Iroh ticket for the workspace to join.
    #[arg(short, long)]
    ticket: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if !args.mount_point.exists() {
        std::fs::create_dir_all(&args.mount_point)?;
    }

    let endpoint = Endpoint::bind().await?;

    let store = Arc::new(match &args.cache_dir {
        Some(dir) => ContentStore::open(dir).await?,
        None => ContentStore::memory(),
    });

    let _router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, store.blobs_protocol(None))
        .spawn();

    let namespace = Arc::new(Mutex::new(Namespace::new(endpoint.id().to_string())));
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));

    // TODO(Phase D): join the gossip workspace + bootstrap-sync from --ticket.
    if args.ticket.is_some() {
        eprintln!("note: --ticket provided but workspace sync is not yet wired");
    }

    let fs = QuickFS::new(namespace, hydrator, heuristics);

    println!("Mounting QuickFS at {:?}", args.mount_point);
    println!("Iroh NodeID: {}", endpoint.id());

    let options = vec![
        MountOption::FSName("quickfs".to_string()),
        MountOption::AutoUnmount,
    ];
    let mut config = Config::default();
    config.mount_options = options;
    config.acl = SessionACL::All;

    let mount_point = args.mount_point.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = fuser::mount2(fs, &mount_point, &config) {
            eprintln!("FUSE mount failed: {e}");
        }
    });

    tokio::signal::ctrl_c().await?;
    println!("Exiting...");

    Ok(())
}
