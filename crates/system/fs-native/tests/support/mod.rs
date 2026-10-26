//! Shared harness for tests that need a real Btrfs volume and, for
//! fanotify, `CAP_SYS_ADMIN`.
//!
//! `TILO_FS_NATIVE_STORE` names the root of a dedicated Btrfs store volume.
//! Without it, capture tests fall back to the target directory when that is
//! on Btrfs. Tests that need fanotify filesystem marks or pre-content events
//! skip with a message unless `TILO_REQUIRE_FANOTIFY=1`, which turns every
//! skip into a failure.

#![allow(dead_code, reason = "each test module uses a different subset")]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs_native::btrfs;
use fs_native::hydrate::Fetcher;

fn skip(test: &str, why: &str) -> Option<Scratch> {
    tilo_test_env::capability_unavailable(
        tilo_test_env::Capability::Fanotify,
        format_args!(
            "{test}: {why} (set TILO_FS_NATIVE_STORE to a dedicated Btrfs mount and run as root)"
        ),
    );
    None
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A per-test directory on the store, with a `live` subvolume and an
/// `epochs` directory. Everything is removed on drop.
pub struct Scratch {
    pub store: PathBuf,
    pub dir: PathBuf,
    pub live: PathBuf,
    pub epochs: PathBuf,
}

impl Scratch {
    fn create(store: &Path, test: &str) -> io::Result<Self> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = store.join(format!("t-{test}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let live = dir.join("live");
        btrfs::create_subvolume(&live)?;
        let epochs = dir.join("epochs");
        fs::create_dir_all(&epochs)?;
        Ok(Self {
            store: store.to_path_buf(),
            dir,
            live,
            epochs,
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.epochs) {
            for e in entries.flatten() {
                let _ = btrfs::delete_subvolume(&e.path());
            }
        }
        let _ = btrfs::delete_subvolume(&self.live);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn store_from_env() -> Option<PathBuf> {
    std::env::var_os("TILO_FS_NATIVE_STORE").map(PathBuf::from)
}

/// A scratch area on any Btrfs filesystem the current user may snapshot.
pub fn btrfs_scratch(test: &str) -> Option<Scratch> {
    let store = store_from_env().unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
    fs::create_dir_all(&store).ok()?;
    if !btrfs::is_btrfs(&store).unwrap_or(false) {
        return skip(test, &format!("{} is not on Btrfs", store.display()));
    }
    match Scratch::create(&store, test) {
        Ok(s) => Some(s),
        Err(e) => skip(test, &format!("cannot create a subvolume: {e}")),
    }
}

/// A scratch area on a dedicated store volume with fanotify privileges.
pub fn fanotify_scratch(test: &str) -> Option<Scratch> {
    let Some(store) = store_from_env() else {
        return skip(test, "TILO_FS_NATIVE_STORE is not set");
    };
    if !fs_native::sys::has_capability(fs_native::sys::CAP_SYS_ADMIN) {
        return skip(
            test,
            "CAP_SYS_ADMIN is required for fanotify filesystem marks",
        );
    }
    if let Err(e) = fs_native::store::check_dedicated(&store) {
        return skip(
            test,
            &format!("{} is not a dedicated store volume: {e}", store.display()),
        );
    }
    match Scratch::create(&store, test) {
        Ok(s) => Some(s),
        Err(e) => skip(test, &format!("cannot create a subvolume: {e}")),
    }
}

/// Deterministic content: the byte at `offset` of the body named `seed`.
pub fn pattern_byte(seed: u8, offset: u64) -> u8 {
    let x = offset.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ u64::from(seed).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    (x >> 56) as u8
}

pub fn pattern(seed: u8, offset: u64, len: usize) -> Vec<u8> {
    (0..len as u64)
        .map(|i| pattern_byte(seed, offset + i))
        .collect()
}

/// A fetcher serving [`pattern`] bytes; the content id's first byte is the
/// seed. It records every request and can be made slow or failing.
#[derive(Default)]
pub struct PatternFetcher {
    pub delay: Mutex<Duration>,
    pub fail: std::sync::atomic::AtomicBool,
    pub calls: Mutex<Vec<(u64, u64)>>,
}

impl PatternFetcher {
    pub fn new(delay: Duration) -> Arc<Self> {
        let f = Arc::new(Self::default());
        *f.delay.lock().unwrap() = delay;
        f
    }

    pub fn calls(&self) -> Vec<(u64, u64)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Fetcher for PatternFetcher {
    fn fetch(&self, content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        self.calls.lock().unwrap().push((offset, len));
        if self.fail.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no source reachable",
            ));
        }
        Ok(pattern(content[0], offset, len as usize))
    }
}

pub fn content_id(seed: u8) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0] = seed;
    id[1] = 0xC0;
    id
}
