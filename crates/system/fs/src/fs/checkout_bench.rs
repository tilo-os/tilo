//! What a repository checkout costs the mount: directories made, then files created, written and
//! closed through the same operations a FUSE `create`, `write` and `release` make, each change
//! journaled by [`crate::kept_documents`] before the operation answers.
//!
//! It prints files per second for each thousand files when it is let to measure, so a cost that
//! grows with the tree shows as a rate that falls. Start it with `--ignored --nocapture`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "measurement arithmetic: fixture sizes and the rates printed from them"
)]

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;

use super::*;
use crate::kept_documents::KeptDocuments;
use crate::namespace::ROOT_INO;

const PER_DIRECTORY: usize = 1_000;

/// A source-tree-sized file: the bytes a file of the checkout holds.
fn bytes_for(file: usize) -> Vec<u8> {
    let len = 400 + (file * 37).checked_rem(4_600).unwrap_or_default();
    let mut bytes = vec![0_u8; len];
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ file as u64;
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
    }
    bytes
}

/// What a measurement does through the mount.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    /// Every operation a checkout makes, each change journaled and each version synced.
    Everything,
    /// The same, without the journal, to price the journal on its own.
    Unjournaled,
    /// The same, with content commits that do not wait for the disk, to price its syncs.
    Unsynced,
    /// Only the creates, journaled: the names without the bytes.
    NamesOnly,
}

impl Work {
    /// Keep each change on disk before the operation answers.
    const fn journal(self) -> bool {
        !matches!(self, Self::Unjournaled)
    }

    /// Write bytes and commit them as a version.
    const fn bytes(self) -> bool {
        !matches!(self, Self::NamesOnly)
    }

    /// Let each content commit wait for the disk.
    const fn sync(self) -> bool {
        !matches!(self, Self::Unsynced)
    }
}

struct Mount {
    fs: QuickFS,
    work: Work,
    store: Arc<crate::ContentStore>,
    _dir: tempfile::TempDir,
}

async fn mount(work: Work) -> Mount {
    let dir = tilo_test_env::memory_tempdir().unwrap();
    let store = Arc::new(crate::ContentStore::memory());
    if !work.sync() {
        store.defer_sync();
    }
    let endpoint = test_server::direct_endpoint().await;
    let hydrator = Arc::new(Hydrator::new(endpoint, Arc::clone(&store)));
    let (kept, namespace) = {
        let (kept, _) = KeptDocuments::open(&dir.path().join("kept")).unwrap();
        let mut namespace = Namespace::new("bench").unwrap();
        kept.opened(&mut namespace, BTreeMap::new()).unwrap();
        (Arc::new(kept), namespace)
    };
    let mut fs = QuickFS::new(
        Arc::new(Mutex::new(namespace)),
        Arc::clone(&hydrator),
        Arc::new(Heuristics::new(hydrator)),
    );
    if work.journal() {
        fs = fs.with_change_journal(Arc::new(move |namespace: &mut Namespace| {
            kept.journal(namespace)
        }));
    }
    if work.bytes() {
        fs = fs.with_write_stage_dir(dir.path().join("write-stage"));
    }
    Mount {
        fs,
        work,
        store,
        _dir: dir,
    }
}

/// What each step of a checkout costs, for the thousand just made.
#[derive(Default)]
struct Spent {
    name: Duration,
    bytes: Duration,
    commit: Duration,
    close: Duration,
}

/// One file as the kernel's `create`, `write`, `flush` and `release` make it, noting what each
/// step took in `spent`.
fn create_and_write(
    fs: &QuickFS,
    parent: u64,
    name: &str,
    bytes: &[u8],
    work: Work,
    spent: &mut Spent,
) {
    let at = Instant::now();
    let ino = fs.create_for_write(parent, name.as_bytes(), 0o644).unwrap();
    fs.publish();
    spent.name = spent.name.saturating_add(at.elapsed());
    let at = Instant::now();
    let handle = fs.next_file_handle.fetch_add(1, Ordering::Relaxed);
    let path = fs.ns().path_for(ino);
    if let Some(path) = path {
        fs.open_file_handles.lock_recover().insert(
            handle,
            OpenFileHandleState {
                ino,
                opened_path: path,
                writable: true,
                append: false,
            },
        );
    }
    spent.bytes = spent.bytes.saturating_add(at.elapsed());
    if work.bytes() {
        let at = Instant::now();
        fs.write_handle(ino, handle, 0, bytes).unwrap();
        spent.bytes = spent.bytes.saturating_add(at.elapsed());
        let at = Instant::now();
        fs.flush_inode(ino).unwrap();
        spent.commit = spent.commit.saturating_add(at.elapsed());
    }
    let at = Instant::now();
    fs.release_handle(ino, handle).unwrap();
    spent.close = spent.close.saturating_add(at.elapsed());
}

/// A checkout of `total` source files, a directory of every thousand, printing the rate at each
/// thousand.
fn checkout(mount: &Mount, total: usize, label: &str) {
    let fs = &mount.fs;
    let work = mount.work;
    let started = Instant::now();
    let mut at_last_mark = started;
    let mut done = 0_usize;
    for directory in 0..total.div_ceil(PER_DIRECTORY) {
        let dir = fs
            .create_empty_dir(ROOT_INO, format!("dir-{directory}"))
            .unwrap();
        fs.publish();
        let in_this = PER_DIRECTORY.min(total - done);
        let mut spent = Spent::default();
        for file in 0..in_this {
            let number = done + file;
            create_and_write(
                fs,
                dir,
                &format!("file-{number}.rs"),
                &bytes_for(number),
                work,
                &mut spent,
            );
        }
        done += in_this;
        let mark = Instant::now();
        let rate = in_this as f64 / (mark - at_last_mark).as_secs_f64();
        let each = |spent: Duration| spent.as_secs_f64() * 1_000_000.0 / in_this as f64;
        println!(
            "{label}: {done} files, {rate:.0} files/s this thousand, {:.2} ms/file (name {:.0} us, write {:.0} us, commit {:.0} us, close {:.0} us)",
            (mark - at_last_mark).as_secs_f64() * 1_000.0 / in_this as f64,
            each(spent.name),
            each(spent.bytes),
            each(spent.commit),
            each(spent.close)
        );
        at_last_mark = mark;
    }
    println!(
        "{label}: {total} files in {:.1} s, {:.0} files/s overall, {} content-store syncs",
        started.elapsed().as_secs_f64(),
        total as f64 / started.elapsed().as_secs_f64(),
        mount.store.write_syncs()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measures a checkout's throughput; --ignored --nocapture"]
async fn checkout_throughput() {
    let total = std::env::var("TILO_CHECKOUT_FILES")
        .ok()
        .and_then(|files| files.parse().ok())
        .unwrap_or(20_000);
    let full = mount(Work::Everything).await;
    checkout(&full, total, "journaled");
    let unjournaled = mount(Work::Unjournaled).await;
    checkout(&unjournaled, total, "unjournaled");
    let unsynced = mount(Work::Unsynced).await;
    checkout(&unsynced, total, "unsynced");
    let creates = mount(Work::NamesOnly).await;
    checkout(&creates, total, "creates-only");
}

/// Where a closed file's commit spends its time: the content store alone, the tree alone, and
/// the whole commit of one file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measures a checkout's per-file costs; --ignored --nocapture"]
async fn checkout_commit_split() {
    const FILES: usize = 2_000;
    let mount = mount(Work::Everything).await;
    let fs = &mount.fs;
    let mut inos = Vec::with_capacity(FILES);
    for n in 0..FILES {
        let ino = fs
            .create_for_write(ROOT_INO, format!("file-{n}.rs").as_bytes(), 0o644)
            .unwrap();
        fs.publish();
        inos.push(ino);
    }
    let started = Instant::now();
    for n in 0..FILES {
        mount
            .store
            .write_content_after(&bytes_for(n), None)
            .await
            .unwrap();
    }
    println!(
        "store alone: {:.0} us/file, {} syncs",
        started.elapsed().as_secs_f64() * 1_000_000.0 / FILES as f64,
        mount.store.write_syncs()
    );
    let started = Instant::now();
    for n in 0..FILES {
        fs.hydrator
            .block_on(mount.store.write_content_after(&bytes_for(n), None))
            .unwrap();
    }
    println!(
        "store through the mount's blocking call: {:.0} us/file",
        started.elapsed().as_secs_f64() * 1_000_000.0 / FILES as f64
    );
    let started = Instant::now();
    for (n, ino) in inos.iter().enumerate() {
        let bytes = bytes_for(n);
        fs.commit_version_at(*ino, VersionSource::Bytes(&bytes), 1, None)
            .unwrap();
    }
    println!(
        "one version's commit, no stage bookkeeping: {:.0} us/file",
        started.elapsed().as_secs_f64() * 1_000_000.0 / FILES as f64
    );
    let started = Instant::now();
    for ino in &inos {
        let mut namespace = fs.ns();
        namespace.set_content(*ino, None, 0, 1);
        namespace.note_change();
    }
    println!(
        "tree alone: {:.0} us/file",
        started.elapsed().as_secs_f64() * 1_000_000.0 / FILES as f64
    );
    let started = Instant::now();
    for (n, ino) in inos.iter().enumerate() {
        fs.write_staged(*ino, Some(0), &bytes_for(n)).unwrap();
        fs.flush_inode(*ino).unwrap();
    }
    println!(
        "whole commit: {:.0} us/file",
        started.elapsed().as_secs_f64() * 1_000_000.0 / FILES as f64
    );
}

/// The journal alone, for each thousand changes, so a cost that grows with the tree shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measures the journal's per-change cost; --ignored --nocapture"]
async fn journal_cost() {
    let dir = tilo_test_env::memory_tempdir().unwrap();
    let (kept, mut namespace) = {
        let (kept, _) = KeptDocuments::open(dir.path()).unwrap();
        let mut namespace = Namespace::new("bench").unwrap();
        kept.opened(&mut namespace, BTreeMap::new()).unwrap();
        (kept, namespace)
    };
    let mut total = Duration::ZERO;
    for n in 0..20_000 {
        namespace
            .create_file(ROOT_INO, format!("file-{n}.rs"), None, 0, 1)
            .unwrap();
        let started = Instant::now();
        kept.journal(&mut namespace).unwrap();
        total = total.saturating_add(started.elapsed());
        if (n + 1) % PER_DIRECTORY == 0 {
            println!(
                "{n} files: journal {:.1} us/file over this thousand",
                total.as_secs_f64() * 1_000_000.0 / PER_DIRECTORY as f64
            );
            total = Duration::ZERO;
        }
    }
}
