use clap::Parser;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use anyhow::Result;
use fuser::*;
use fs::{QuickFS, MetadataStore, Hydrator, Heuristics};
use iroh::endpoint::Endpoint;
use iroh_blobs::BlobsProtocol;
use iroh_blobs::store::mem::MemStore;
use iroh::protocol::Router;
use iroh_blobs::provider::events::{EventMask, EventSender, ProviderMessage};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Mount point
    #[arg(short, long)]
    mount_point: PathBuf,

    /// Iroh Ticket for the workspace
    #[arg(short, long)]
    ticket: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if !args.mount_point.exists() {
        std::fs::create_dir_all(&args.mount_point)?;
    }

    // 1. Create an iroh endpoint.
    let endpoint = Endpoint::builder()
        .bind() 
        .await?;
    
    // 2. Setup event tracking for participants
    let (event_sender, mut event_rx) = EventSender::channel(32, EventMask::ALL_READONLY);
    
    // 3. Create a blob store and net protocol.
    let store = MemStore::new();
    let blobs = BlobsProtocol::new(&store, Some(event_sender));

    // 4. Build and spawn the router.
    let _router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, blobs)
        .spawn();

    // 5. Run event loop to track participants
    tokio::spawn(async move {
        while let Some(msg) = event_rx.recv().await {
             match msg {
                 ProviderMessage::ClientConnected(c) => {
                     println!("Participant connected: {:?}", c.inner.endpoint_id);
                     let _ = c.tx.send(Ok(())).await;
                 }
                 ProviderMessage::ConnectionClosed(c) => {
                     println!("Participant disconnected: connection {}", c.inner.connection_id);
                 }
                 _ => {}
             }
        }
    });

    let metadata = Arc::new(Mutex::new(MetadataStore::new()));
    let hydrator = Arc::new(Hydrator::new(endpoint.clone()));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), metadata.clone()));

    let fs = QuickFS::new(metadata, hydrator, heuristics);

    println!("Mounting QuickFS at {:?}", args.mount_point);
    println!("Iroh NodeID: {}", endpoint.secret_key().public());
    
    let options = vec![
        MountOption::RO,
        MountOption::FSName("quickfs".to_string()),
        MountOption::AutoUnmount,
    ];

    let mut config = Config::default();
    config.mount_options = options;
    config.acl = SessionACL::All;

    tokio::task::spawn_blocking(move || {
        fuser::mount2(fs, args.mount_point, &config).expect("FUSE mount failed");
    });

    tokio::signal::ctrl_c().await?;
    println!("Exiting...");

    Ok(())
}
