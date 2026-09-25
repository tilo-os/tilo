//! Capture epochs on a real Btrfs subvolume, driven by hand-written journal
//! batches so they run without fanotify privileges.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fs_native::capture::{CaptureConfig, ChangeOp, Changeset, EpochCapture, NodeKind};
use fs_native::chunking::Manifest;
use fs_native::journal::{JournalBatch, JournalEntry, JournalOp, RescanReason};

use crate::support::{btrfs_scratch, pattern};

fn batch(entries: &[(JournalOp, &str, bool)]) -> JournalBatch {
    let entries: Vec<JournalEntry> = entries
        .iter()
        .enumerate()
        .map(|(i, (op, path, is_dir))| JournalEntry {
            seq: i as u64 + 1,
            op: op.clone(),
            path: Some(PathBuf::from(path)),
            is_dir: *is_dir,
            pid: 0,
        })
        .collect();
    let through = entries.len() as u64;
    JournalBatch {
        entries,
        rescan: None,
        through,
    }
}

fn upserts(cs: &Changeset) -> Vec<(String, Option<blake3::Hash>)> {
    cs.ops
        .iter()
        .filter_map(|op| match op {
            ChangeOp::Upsert { path, node } => Some((
                path.display().to_string(),
                node.manifest.as_ref().map(|m| m.content_id),
            )),
            ChangeOp::Remove { .. } => None,
        })
        .collect()
}

fn removes(cs: &Changeset) -> Vec<String> {
    cs.ops
        .iter()
        .filter_map(|op| match op {
            ChangeOp::Remove { path, .. } => Some(path.display().to_string()),
            ChangeOp::Upsert { .. } => None,
        })
        .collect()
}

fn full_id(path: &Path) -> blake3::Hash {
    Manifest::build(&fs::read(path).unwrap(), None).0.content_id
}

#[test]
fn first_epoch_captures_the_whole_tree_with_exact_content_ids() {
    let Some(s) = btrfs_scratch("capture_first") else {
        return;
    };
    fs::create_dir_all(s.live.join("src/deep")).unwrap();
    fs::write(s.live.join("src/small.txt"), b"hello").unwrap();
    fs::write(s.live.join("src/deep/mid.bin"), pattern(1, 0, 300_000)).unwrap();
    fs::write(s.live.join("big.bin"), pattern(2, 0, 5 << 20)).unwrap();
    std::os::unix::fs::symlink("src/small.txt", s.live.join("link")).unwrap();
    fs::set_permissions(
        s.live.join("src/small.txt"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    let cs = cap.capture(&JournalBatch::default()).unwrap();
    assert!(cs.stats.full_rescan);
    let ups = upserts(&cs);
    let paths: Vec<&str> = ups.iter().map(|(p, _)| p.as_str()).collect();
    for want in [
        "src",
        "src/deep",
        "src/small.txt",
        "src/deep/mid.bin",
        "big.bin",
        "link",
    ] {
        assert!(paths.contains(&want), "{want} missing from {paths:?}");
    }
    // Parents precede children.
    let pos = |p: &str| paths.iter().position(|x| *x == p).unwrap();
    assert!(pos("src") < pos("src/deep") && pos("src/deep") < pos("src/deep/mid.bin"));
    for (p, id) in &ups {
        let abs = s.live.join(p);
        if abs.is_file() && !abs.is_symlink() {
            assert_eq!(*id, Some(full_id(&abs)), "{p}");
        }
    }
    let nodes: Vec<_> = cap.nodes().collect();
    let small = nodes
        .iter()
        .find(|(p, _)| *p == Path::new("src/small.txt"))
        .unwrap()
        .1;
    assert_eq!(small.mode, 0o755);
    assert_eq!(
        small.manifest.as_ref().unwrap().inline.as_deref(),
        Some(&b"hello"[..])
    );
    let link = nodes
        .iter()
        .find(|(p, _)| *p == Path::new("link"))
        .unwrap()
        .1;
    assert_eq!(link.kind, NodeKind::Symlink(PathBuf::from("src/small.txt")));
    cap.release().unwrap();
}

#[test]
fn an_in_place_edit_of_a_large_file_rehashes_only_the_changed_extent() {
    let Some(s) = btrfs_scratch("capture_inplace") else {
        return;
    };
    let path = s.live.join("disk.img");
    fs::write(&path, pattern(3, 0, 64 << 20)).unwrap();
    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    cap.capture(&JournalBatch::default()).unwrap();

    let f = OpenOptions::new().write(true).open(&path).unwrap();
    f.write_all_at(&[0xAB; 4096], 40 << 20).unwrap();
    drop(f);
    let cs = cap
        .capture(&batch(&[
            (JournalOp::Modify, "disk.img", false),
            (JournalOp::CloseWrite, "disk.img", false),
        ]))
        .unwrap();
    assert!(!cs.stats.full_rescan);
    let ups = upserts(&cs);
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0].1, Some(full_id(&path)));
    let chunks = cs.stats.chunks;
    assert!(
        chunks.chunks_hashed <= 2,
        "rehashed {} chunks for a 4 KiB edit",
        chunks.chunks_hashed
    );
    assert!(
        chunks.bytes_hashed <= 2 << 20,
        "hashed {} bytes",
        chunks.bytes_hashed
    );
    assert!(chunks.chunks_reused > 200);
    cap.release().unwrap();
}

#[test]
fn a_rename_reuses_the_manifest_without_reading_the_file() {
    let Some(s) = btrfs_scratch("capture_rename") else {
        return;
    };
    fs::write(s.live.join("a.bin"), pattern(4, 0, 8 << 20)).unwrap();
    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    let first = cap.capture(&JournalBatch::default()).unwrap();
    let id = upserts(&first)[0].1;
    fs::rename(s.live.join("a.bin"), s.live.join("b.bin")).unwrap();
    let cs = cap
        .capture(&batch(&[(
            JournalOp::Rename {
                from: Some("a.bin".into()),
            },
            "b.bin",
            false,
        )]))
        .unwrap();
    assert_eq!(removes(&cs), ["a.bin"]);
    assert_eq!(upserts(&cs), [("b.bin".to_owned(), id)]);
    assert_eq!(cs.stats.chunks.bytes_hashed, 0);
    cap.release().unwrap();
}

#[test]
fn directory_moves_and_deletes_cover_their_subtrees() {
    let Some(s) = btrfs_scratch("capture_dirs") else {
        return;
    };
    fs::create_dir_all(s.live.join("a/b")).unwrap();
    fs::write(s.live.join("a/b/f"), b"x").unwrap();
    fs::write(s.live.join("a/g"), b"y").unwrap();
    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    cap.capture(&JournalBatch::default()).unwrap();

    fs::rename(s.live.join("a"), s.live.join("z")).unwrap();
    let cs = cap
        .capture(&batch(&[(
            JournalOp::Rename {
                from: Some("a".into()),
            },
            "z",
            true,
        )]))
        .unwrap();
    let mut rm = removes(&cs);
    rm.sort();
    assert_eq!(rm, ["a", "a/b", "a/b/f", "a/g"]);
    let ups: Vec<String> = upserts(&cs).into_iter().map(|(p, _)| p).collect();
    for want in ["z", "z/b", "z/b/f", "z/g"] {
        assert!(
            ups.contains(&want.to_owned()),
            "{want} missing from {ups:?}"
        );
    }
    // Children are removed before their directory.
    let order: Vec<String> = cs
        .ops
        .iter()
        .map(|o| o.path().display().to_string())
        .collect();
    let at = |p: &str| order.iter().position(|x| x == p).unwrap();
    assert!(at("a/b/f") < at("a/b") && at("a/b") < at("a"));

    fs::remove_dir_all(s.live.join("z")).unwrap();
    let cs = cap
        .capture(&batch(&[(JournalOp::Delete, "z", true)]))
        .unwrap();
    let mut rm = removes(&cs);
    rm.sort();
    assert_eq!(rm, ["z", "z/b", "z/b/f", "z/g"]);
    assert_eq!(cap.nodes().count(), 0);
    cap.release().unwrap();
}

#[test]
fn a_rescan_finds_changes_the_journal_never_reported() {
    let Some(s) = btrfs_scratch("capture_rescan") else {
        return;
    };
    fs::write(s.live.join("keep"), b"same").unwrap();
    fs::write(s.live.join("edit"), b"before").unwrap();
    fs::write(s.live.join("gone"), b"bye").unwrap();
    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    cap.capture(&JournalBatch::default()).unwrap();

    fs::write(s.live.join("edit"), b"after!").unwrap();
    fs::remove_file(s.live.join("gone")).unwrap();
    for i in 0..500 {
        fs::write(s.live.join(format!("new-{i}")), format!("{i}")).unwrap();
    }
    let lost = JournalBatch {
        entries: Vec::new(),
        rescan: Some(RescanReason::Overflow),
        through: 0,
    };
    let cs = cap.capture(&lost).unwrap();
    assert!(cs.stats.full_rescan);
    assert_eq!(removes(&cs), ["gone"]);
    let ups: Vec<String> = upserts(&cs).into_iter().map(|(p, _)| p).collect();
    assert_eq!(ups.len(), 501, "{ups:?}");
    assert!(ups.contains(&"edit".to_owned()));
    assert!(!ups.contains(&"keep".to_owned()));
    assert_eq!(cap.nodes().count(), 502);
    cap.release().unwrap();
}

#[test]
fn local_only_trees_and_special_files_are_never_captured() {
    let Some(s) = btrfs_scratch("capture_local") else {
        return;
    };
    fs::create_dir_all(s.live.join("target/debug")).unwrap();
    fs::write(
        s.live.join("target/CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n# cache\n",
    )
    .unwrap();
    fs::write(s.live.join("target/debug/out"), b"artifact").unwrap();
    fs::create_dir_all(s.live.join("derived")).unwrap();
    fs::write(s.live.join("derived/index"), b"per-device").unwrap();
    let marked = xattr_set(&s.live.join("derived"), "user.tilo.local", b"1");
    fs::write(s.live.join("src.rs"), b"fn main() {}").unwrap();
    let fifo = s.live.join("pipe");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);

    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    let cs = cap.capture(&JournalBatch::default()).unwrap();
    let ups: Vec<String> = upserts(&cs).into_iter().map(|(p, _)| p).collect();
    assert!(ups.contains(&"src.rs".to_owned()));
    assert!(ups.iter().all(|p| !p.starts_with("target/")), "{ups:?}");
    assert!(!ups.contains(&"pipe".to_owned()));
    if marked {
        assert!(ups.iter().all(|p| !p.starts_with("derived")), "{ups:?}");
    }
    cap.release().unwrap();
}

fn xattr_set(path: &Path, name: &str, value: &[u8]) -> bool {
    let p = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let n = std::ffi::CString::new(name).unwrap();
    unsafe {
        libc::setxattr(
            p.as_ptr(),
            n.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        ) == 0
    }
}

#[test]
fn writers_keep_writing_while_epochs_are_cut() {
    let Some(s) = btrfs_scratch("capture_writers") else {
        return;
    };
    let path = s.live.join("log");
    File::create(&path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let writer = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        std::thread::spawn(move || {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            let mut max = std::time::Duration::ZERO;
            while !stop.load(Ordering::Relaxed) {
                let t = std::time::Instant::now();
                f.write_all(&[b'x'; 4096]).unwrap();
                max = max.max(t.elapsed());
                written.fetch_add(4096, Ordering::Relaxed);
                // A steady writer (~4 MB/s), not a disk-filling one.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            max
        })
    };
    let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    cap.capture(&JournalBatch::default()).unwrap();
    let mut sizes = Vec::new();
    for _ in 0..5 {
        let before = written.load(Ordering::Relaxed);
        let cs = cap
            .capture(&batch(&[(JournalOp::Modify, "log", false)]))
            .unwrap();
        let size = cap
            .nodes()
            .find(|(p, _)| *p == Path::new("log"))
            .map(|(_, n)| n.size)
            .unwrap();
        // The snapshot holds a whole number of completed writes.
        assert_eq!(size % 4096, 0);
        assert!(size >= before);
        sizes.push(size);
        assert!(cs.ops.len() <= 1);
    }
    stop.store(true, Ordering::Relaxed);
    let max_write = writer.join().unwrap();
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]));
    assert!(written.load(Ordering::Relaxed) > *sizes.last().unwrap());
    eprintln!("longest single 4 KiB append while capturing: {max_write:?}");
    cap.release().unwrap();
}
