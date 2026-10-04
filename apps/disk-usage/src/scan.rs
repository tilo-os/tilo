//! Bounded, symlink-safe recursive directory measurement.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Cheap cooperative cancellation shared with a background scanner.
#[derive(Clone, Debug, Default)]
pub struct ScanCancellation(Arc<AtomicBool>);

impl ScanCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub files: u64,
    pub unreadable: u64,
}

#[derive(Clone, Debug)]
pub struct ScanResult {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub total: u64,
    pub files: u64,
    pub unreadable: u64,
    pub error: Option<String>,
}

impl ScanResult {
    /// A finished scan of a made-up home folder: what a screenshot shows in
    /// place of scanning the machine it is taken on.
    pub fn sample() -> Self {
        let path = PathBuf::from("/home/you");
        let entries: Vec<Entry> = [
            ("Projects", 84_200_000_000),
            ("Videos", 48_600_000_000),
            ("Photos", 31_800_000_000),
            ("Downloads", 19_400_000_000),
            ("Documents", 8_700_000_000),
            ("Other", 5_100_000_000),
        ]
        .into_iter()
        .map(|(name, bytes)| Entry {
            path: path.join(name.to_lowercase()),
            name: name.into(),
            bytes,
            files: bytes / 1_000_000,
            unreadable: 0,
        })
        .collect();
        let total = entries.iter().map(|entry| entry.bytes).sum();
        Self {
            path,
            entries,
            total,
            files: 12_842,
            unreadable: 0,
            error: None,
        }
    }
}

fn measure(path: &Path, cancellation: &ScanCancellation) -> (u64, u64, u64) {
    if cancellation.is_cancelled() {
        return (0, 0, 0);
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return (0, 0, 1);
    };
    if meta.file_type().is_symlink() {
        return (0, 0, 0);
    }
    if meta.is_file() {
        return (meta.len(), 1, 0);
    }
    if !meta.is_dir() {
        return (meta.len(), 0, 0);
    }
    let Ok(read) = fs::read_dir(path) else {
        return (0, 0, 1);
    };
    read.take_while(|_| !cancellation.is_cancelled())
        .filter_map(Result::ok)
        .fold((0_u64, 0_u64, 0_u64), |acc, child| {
            let measured = measure(&child.path(), cancellation);
            (
                acc.0.saturating_add(measured.0),
                acc.1.saturating_add(measured.1),
                acc.2.saturating_add(measured.2),
            )
        })
}

pub fn scan(path: PathBuf) -> ScanResult {
    scan_with_cancellation(path, &ScanCancellation::default())
}

pub fn scan_with_cancellation(path: PathBuf, cancellation: &ScanCancellation) -> ScanResult {
    let mut unreadable = 0_u64;
    let mut error = None;
    let mut candidates = Vec::new();
    if let Ok(read) = fs::read_dir(&path) {
        for child in read.take_while(|_| !cancellation.is_cancelled()) {
            match child {
                Ok(child) => {
                    let child_path = child.path();
                    let is_dir =
                        fs::symlink_metadata(&child_path).is_ok_and(|metadata| metadata.is_dir());
                    candidates.push((
                        child_path,
                        child.file_name().to_string_lossy().into_owned(),
                        is_dir,
                    ));
                }
                Err(_) => unreadable = unreadable.saturating_add(1),
            }
        }
    } else {
        unreadable = 1;
        error = Some("The folder is unavailable or access was not granted".into());
    }

    let next = AtomicUsize::new(0);
    let measured = Mutex::new((Vec::new(), 0_u64, 0_u64, unreadable));
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let worker_count = available.clamp(1, 8).min(candidates.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((child_path, name, is_dir)) = candidates.get(index) else {
                        break;
                    };
                    let (bytes, files, denied) = measure(child_path, cancellation);
                    let Ok(mut result) = measured.lock() else {
                        break;
                    };
                    result.3 = result.3.saturating_add(denied);
                    if *is_dir {
                        result.0.push(Entry {
                            name: name.clone(),
                            path: child_path.clone(),
                            bytes,
                            files,
                            unreadable: denied,
                        });
                    } else {
                        result.1 = result.1.saturating_add(bytes);
                        result.2 = result.2.saturating_add(files);
                    }
                }
            });
        }
    });
    let (mut entries, own_bytes, own_files, unreadable) = measured
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if own_bytes > 0 {
        entries.push(Entry {
            path: path.join("."),
            name: "Files in this folder".into(),
            bytes: own_bytes,
            files: own_files,
            unreadable: 0,
        });
    }
    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let total = entries
        .iter()
        .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
    let files = entries
        .iter()
        .fold(0_u64, |sum, entry| sum.saturating_add(entry.files));
    ScanResult {
        path,
        entries,
        total,
        files,
        unreadable,
        error,
    }
}
