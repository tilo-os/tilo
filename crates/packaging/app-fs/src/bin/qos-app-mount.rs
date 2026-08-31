//! `qos-app-mount` — mount a qOS app payload from a
//! content-addressed disk blob store at a target directory.
//!
//! Usage:
//!
//! ```text
//! qos-app-mount --package <collection-hash>            \
//!               --store   /var/lib/qos-apps/blobs      \
//!               --mount   /var/qos-apps/<package_id>
//! ```
//!
//! The process blocks in the foreground while the FUSE session
//! runs on a background worker; Ctrl-C (SIGINT / SIGTERM)
//! triggers an orderly unmount + join.
//!
//! `--package` is the hex-encoded BLAKE3 of the Collection
//! blob. In a full launcher run the launcher looks the
//! collection hash up from the signed manifest; here we take it
//! on the command line so the crate is usable standalone.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use app_fs::{
    Collection, DiskBlobStore, Mount, MountError, MountOptions,
    store::{from_hex, read_all},
};
use clap::Parser;

/// Mount a qOS package payload on a directory.
#[derive(Debug, Parser)]
#[command(name = "qos-app-mount", version, about)]
struct Cli {
    /// Hex-encoded BLAKE3 of the Collection blob to mount.
    #[arg(long)]
    package: String,
    /// Directory containing the content-addressed blob store
    /// (files named by hex hash).
    #[arg(long)]
    store: PathBuf,
    /// Mount point.
    #[arg(long)]
    mount: PathBuf,
    /// Allow other users to access the mount. Requires
    /// `user_allow_other` in `/etc/fuse.conf`.
    #[arg(long)]
    allow_other: bool,
    /// Page-cache capacity in 4 KiB pages. Defaults to 4096
    /// (16 MiB).
    #[arg(long)]
    cache_pages: Option<usize>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber();

    let cli = Cli::parse();

    let collection_hash = from_hex(&cli.package)
        .map_err(|e| format!("--package must be a 64-char hex BLAKE3 hash ({e})"))?;

    let store = DiskBlobStore::new(&cli.store);
    let collection_bytes = read_all(&store, &collection_hash)
        .map_err(|e| format!("read collection blob from store: {e}"))?;
    let collection =
        Collection::decode(&collection_bytes).map_err(|e| format!("decode collection: {e}"))?;

    let options = MountOptions {
        allow_other: cli.allow_other,
        cache_pages: cli.cache_pages,
        ..MountOptions::default()
    };

    let mount = Mount::spawn(&collection, store, &cli.mount, &options)?;
    eprintln!(
        "mounted {} entries at {} — Ctrl-C to unmount",
        collection.entries.len(),
        mount.mountpoint().display(),
    );

    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handler(&stop);
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(200));
    }

    match mount.unmount() {
        Ok(()) => {
            eprintln!("unmounted cleanly");
            Ok(())
        }
        Err(MountError::Fuse(_, e)) => Err(format!("unmount: {e}").into()),
        Err(e) => Err(Box::new(e)),
    }
}

fn tracing_subscriber() {
    // Minimal: honour RUST_LOG if present, otherwise stay
    // quiet. Uses `eprintln!` for user-facing lines so this
    // stays a hard dep-free stub.
    if std::env::var_os("RUST_LOG").is_some() {
        // Fall back to a no-op — the crate depends only on
        // `tracing` (facade), not `tracing-subscriber`, to keep
        // the mount binary small. Callers that want structured
        // logs can wrap this bin.
    }
}

/// Trap SIGINT/SIGTERM so an outer supervisor can shut us down
/// without leaving a dangling mount. Uses `libc::signal` with a
/// C-callable trampoline that pokes a `static AtomicBool`.
fn install_signal_handler(flag: &Arc<AtomicBool>) {
    static STOP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_signum: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }

    let flag = Arc::clone(flag);
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    thread::spawn(move || {
        loop {
            if STOP.load(Ordering::SeqCst) {
                flag.store(true, Ordering::SeqCst);
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
}
