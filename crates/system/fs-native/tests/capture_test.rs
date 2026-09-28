//! Capture epochs on a real Btrfs subvolume, driven by hand-written journal
//! batches so they run without fanotify privileges.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fs_native::capture::{CaptureConfig, ChangeOp, Changeset, EpochCapture, NodeKind};
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

fn upserts(cs: &Changeset) -> Vec<(String, Option<[u8; 32]>)> {
    cs.ops
        .iter()
        .filter_map(|op| match op {
            ChangeOp::Upsert { path, node } => Some((
                path.display().to_string(),
                node.manifest.as_ref().map(|m| m.id()),
            )),
            ChangeOp::Remove { .. } | ChangeOp::Move { .. } => None,
        })
        .collect()
}

/// `from -> to` for every move, with `*` when the moved node also changed.
fn moves(cs: &Changeset) -> Vec<String> {
    cs.ops
        .iter()
        .filter_map(|op| match op {
            ChangeOp::Move {
                from,
                path,
                changed,
                ..
            } => Some(format!(
                "{} -> {}{}",
                from.display(),
                path.display(),
                if *changed { " *" } else { "" }
            )),
            _ => None,
        })
        .collect()
}

fn removes(cs: &Changeset) -> Vec<String> {
    cs.ops
        .iter()
        .filter_map(|op| match op {
            ChangeOp::Remove { path, .. } => Some(path.display().to_string()),
            ChangeOp::Upsert { .. } | ChangeOp::Move { .. } => None,
        })
        .collect()
}

fn full_id(path: &Path) -> [u8; 32] {
    fs_content::build(&fs::read(path).unwrap(), None).0.id()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn capture_epochs_hash_whole_trees_and_only_changed_extents() {
    // Phase `first_epoch_captures_the_whole_tree_with_exact_content_ids`.
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
        let manifest = small.manifest.as_ref().unwrap();
        assert_eq!(manifest.tier(), fs_content::ChunkTier::Inline);
        assert_eq!(manifest.chunks().len(), 1);
        assert_eq!(
            &manifest.chunks()[0].hash,
            blake3::hash(b"hello").as_bytes()
        );
        let link = nodes
            .iter()
            .find(|(p, _)| *p == Path::new("link"))
            .unwrap()
            .1;
        assert_eq!(link.kind, NodeKind::Symlink(PathBuf::from("src/small.txt")));
        cap.release().unwrap();
    }
    // Phase `an_in_place_edit_of_a_large_file_rehashes_only_the_changed_extent`.
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
    // Phase `without_content_hashing_changes_are_still_found_by_metadata`.
    fn without_content_hashing_changes_are_still_found_by_metadata() {
        let Some(s) = btrfs_scratch("capture_nohash") else {
            return;
        };
        fs::write(s.live.join("f"), b"one").unwrap();
        let mut cfg = CaptureConfig::new(&s.live, &s.epochs);
        cfg.hash_content = false;
        let mut cap = EpochCapture::new(cfg).unwrap();
        let cs = cap.capture(&batch(&[])).unwrap();
        assert_eq!(op_paths(&cs), ["+f"]);
        assert!(upserts(&cs).iter().all(|(_, id)| id.is_none()));
        fs::write(s.live.join("f"), b"two, longer").unwrap();
        let cs = cap
            .capture(&batch(&[(JournalOp::Modify, "f", false)]))
            .unwrap();
        assert_eq!(op_paths(&cs), ["+f"]);
        let cs = cap
            .capture(&batch(&[(JournalOp::CloseWrite, "f", false)]))
            .unwrap();
        assert!(cs.ops.is_empty());
        cap.release().unwrap();
    }
    // Phase `first_epoch_captures_the_whole_tree_with_exact_content_ids` (defined above).
    first_epoch_captures_the_whole_tree_with_exact_content_ids();
    // Phase `an_in_place_edit_of_a_large_file_rehashes_only_the_changed_extent` (defined above).
    an_in_place_edit_of_a_large_file_rehashes_only_the_changed_extent();
    // Phase `without_content_hashing_changes_are_still_found_by_metadata` (defined above).
    without_content_hashing_changes_are_still_found_by_metadata();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn capture_follows_renames_links_and_subtree_moves() {
    // Phase `a_rename_is_a_move_that_reads_nothing`.
    fn a_rename_is_a_move_that_reads_nothing() {
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
        assert!(removes(&cs).is_empty(), "{:?}", op_paths(&cs));
        assert!(upserts(&cs).is_empty(), "{:?}", op_paths(&cs));
        assert_eq!(moves(&cs), ["a.bin -> b.bin"]);
        let ChangeOp::Move { node, .. } = &cs.ops[0] else {
            panic!("{:?}", op_paths(&cs));
        };
        assert_eq!(node.manifest.as_ref().map(|m| m.id()), id);
        assert_eq!(cs.stats.chunks.bytes_hashed, 0);

        // Renamed over an existing file and then edited: one move that also
        // changed, replacing the file that stood there.
        fs::write(s.live.join("c.bin"), b"victim").unwrap();
        cap.capture(&batch(&[(JournalOp::Create, "c.bin", false)]))
            .unwrap();
        fs::rename(s.live.join("b.bin"), s.live.join("c.bin")).unwrap();
        OpenOptions::new()
            .write(true)
            .open(s.live.join("c.bin"))
            .unwrap()
            .write_all_at(b"edit", 4096)
            .unwrap();
        let cs = cap
            .capture(&batch(&[
                (
                    JournalOp::Rename {
                        from: Some("b.bin".into()),
                    },
                    "c.bin",
                    false,
                ),
                (JournalOp::Modify, "c.bin", false),
            ]))
            .unwrap();
        assert_eq!(moves(&cs), ["b.bin -> c.bin *"], "{:?}", op_paths(&cs));
        assert!(removes(&cs).is_empty() && upserts(&cs).is_empty());
        let paths: Vec<&Path> = cap.nodes().map(|(p, _)| p).collect();
        assert_eq!(paths, [Path::new("c.bin")]);
        cap.release().unwrap();
    }
    // Phase `hard_links_are_separate_files_and_a_write_through_one_updates_both`.
    fn hard_links_are_separate_files_and_a_write_through_one_updates_both() {
        let Some(s) = btrfs_scratch("capture_hardlinks") else {
            return;
        };
        fs::write(s.live.join("a"), b"one").unwrap();
        fs::hard_link(s.live.join("a"), s.live.join("b")).unwrap();
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        let first = cap.capture(&JournalBatch::default()).unwrap();
        let mut ups: Vec<String> = upserts(&first).into_iter().map(|(p, _)| p).collect();
        ups.sort();
        assert_eq!(ups, ["a", "b"]);

        // The journal names only the path written through.
        fs::write(s.live.join("a"), b"two").unwrap();
        let cs = cap
            .capture(&batch(&[(JournalOp::Modify, "a", false)]))
            .unwrap();
        let mut ups: Vec<String> = upserts(&cs).into_iter().map(|(p, _)| p).collect();
        ups.sort();
        assert_eq!(ups, ["a", "b"], "both links carry the new bytes");

        // Removing one link of two is a removal, not a move.
        fs::remove_file(s.live.join("a")).unwrap();
        let cs = cap
            .capture(&batch(&[(JournalOp::Delete, "a", false)]))
            .unwrap();
        assert_eq!(removes(&cs), ["a"]);
        assert!(moves(&cs).is_empty());
        cap.release().unwrap();
    }
    // Phase `directory_moves_and_deletes_cover_their_subtrees`.
    fn directory_moves_and_deletes_cover_their_subtrees() {
        let Some(s) = btrfs_scratch("capture_dirs") else {
            return;
        };
        fs::create_dir_all(s.live.join("a/b")).unwrap();
        fs::write(s.live.join("a/b/f"), b"x").unwrap();
        fs::write(s.live.join("a/g"), b"y").unwrap();
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        cap.capture(&JournalBatch::default()).unwrap();

        // Moving the directory moves its subtree in one operation. A file
        // deleted inside it and one created in it after the move follow it.
        fs::remove_file(s.live.join("a/g")).unwrap();
        fs::rename(s.live.join("a"), s.live.join("z")).unwrap();
        fs::write(s.live.join("z/new"), b"n").unwrap();
        let cs = cap
            .capture(&batch(&[
                (JournalOp::Delete, "a/g", false),
                (
                    JournalOp::Rename {
                        from: Some("a".into()),
                    },
                    "z",
                    true,
                ),
                (JournalOp::Create, "z/new", false),
            ]))
            .unwrap();
        assert_eq!(
            op_paths(&cs),
            ["a>z", "-z/g", "+z/new"],
            "one move, then what changed inside it, in the moved directory"
        );
        let mut paths: Vec<String> = cap.nodes().map(|(p, _)| p.display().to_string()).collect();
        paths.sort();
        assert_eq!(paths, ["z", "z/b", "z/b/f", "z/new"]);

        // A directory moved into place, and a subdirectory then moved out of it.
        fs::rename(s.live.join("z"), s.live.join("y")).unwrap();
        fs::rename(s.live.join("y/b"), s.live.join("x")).unwrap();
        let cs = cap
            .capture(&batch(&[
                (
                    JournalOp::Rename {
                        from: Some("z".into()),
                    },
                    "y",
                    true,
                ),
                (
                    JournalOp::Rename {
                        from: Some("y/b".into()),
                    },
                    "x",
                    true,
                ),
            ]))
            .unwrap();
        assert_eq!(op_paths(&cs), ["z>y", "y/b>x"]);
        let mut paths: Vec<String> = cap.nodes().map(|(p, _)| p.display().to_string()).collect();
        paths.sort();
        assert_eq!(paths, ["x", "x/f", "y", "y/new"]);
        fs::remove_dir_all(s.live.join("x")).unwrap();
        fs::rename(s.live.join("y"), s.live.join("z")).unwrap();
        fs::create_dir(s.live.join("z/b")).unwrap();
        fs::write(s.live.join("z/b/f"), b"x").unwrap();
        fs::write(s.live.join("z/g"), b"y").unwrap();
        fs::remove_file(s.live.join("z/new")).unwrap();
        cap.capture(&batch(&[
            (JournalOp::Delete, "x", true),
            (
                JournalOp::Rename {
                    from: Some("y".into()),
                },
                "z",
                true,
            ),
            (JournalOp::Create, "z/b", true),
            (JournalOp::Create, "z/b/f", false),
            (JournalOp::Create, "z/g", false),
            (JournalOp::Delete, "z/new", false),
        ]))
        .unwrap();

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
    // Phase `a_rename_is_a_move_that_reads_nothing` (defined above).
    a_rename_is_a_move_that_reads_nothing();
    // Phase `hard_links_are_separate_files_and_a_write_through_one_updates_both` (defined above).
    hard_links_are_separate_files_and_a_write_through_one_updates_both();
    // Phase `directory_moves_and_deletes_cover_their_subtrees` (defined above).
    directory_moves_and_deletes_cover_their_subtrees();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn capture_rescans_resumes_and_marked_paths_find_unjournaled_changes() {
    // Phase `a_rescan_finds_changes_the_journal_never_reported`.
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
    // Phase `a_resumed_capture_finds_what_changed_while_nothing_was_journaling`.
    fn a_resumed_capture_finds_what_changed_while_nothing_was_journaling() {
        let Some(s) = btrfs_scratch("capture_resume") else {
            return;
        };
        fs::create_dir_all(s.live.join("d")).unwrap();
        for name in ["a", "b", "c", "d/e"] {
            fs::write(s.live.join(name), name.as_bytes()).unwrap();
        }
        let mut cfg = CaptureConfig::new(&s.live, &s.epochs);
        cfg.defer_release = true;
        let mut cap = EpochCapture::new(cfg.clone()).unwrap();
        cap.capture(&batch(&[])).unwrap();
        let state = cap.state().unwrap();
        assert_eq!(state.next_epoch, 2);
        assert_eq!(state.nodes.len(), 5, "{:?}", state.nodes);
        // The process goes away without releasing its snapshot.
        drop(cap);

        fs::write(s.live.join("a"), b"changed while stopped").unwrap();
        fs::remove_file(s.live.join("b")).unwrap();
        fs::write(s.live.join("new"), b"n").unwrap();

        let mut cap = EpochCapture::resume(cfg, state).unwrap();
        let cs = cap.capture(&batch(&[])).unwrap();
        assert_eq!(cs.epoch, 2);
        let mut got = op_paths(&cs);
        got.sort();
        assert_eq!(got, ["+a", "+new", "-b"]);
        assert!(cs.stats.full_rescan);
        // Unchanged files were compared by metadata only.
        assert_eq!(cs.stats.files_hashed, 2);
        cap.release().unwrap();
    }
    // Phase `a_tracked_path_is_diffed_against_and_its_local_removal_is_captured`.
    fn a_tracked_path_is_diffed_against_and_its_local_removal_is_captured() {
        let Some(s) = btrfs_scratch("capture_track") else {
            return;
        };
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        cap.capture(&batch(&[])).unwrap();

        // Written by the consumer itself (a remote version), never journaled.
        fs::write(s.live.join("remote.txt"), b"from a peer").unwrap();
        cap.track(Path::new("remote.txt")).unwrap();
        assert!(cap.node_state(Path::new("remote.txt")).is_some());

        // A journal entry that names it without a change publishes nothing.
        let cs = cap
            .capture(&batch(&[(JournalOp::Attrib, "remote.txt", false)]))
            .unwrap();
        assert!(cs.ops.is_empty(), "{:?}", op_paths(&cs));

        // A local delete of it is captured.
        fs::remove_file(s.live.join("remote.txt")).unwrap();
        let cs = cap
            .capture(&batch(&[(JournalOp::Delete, "remote.txt", false)]))
            .unwrap();
        assert_eq!(op_paths(&cs), ["-remote.txt"]);

        // Untracking forgets a path the consumer removed itself.
        fs::write(s.live.join("gone"), b"x").unwrap();
        cap.track(Path::new("gone")).unwrap();
        fs::remove_file(s.live.join("gone")).unwrap();
        cap.untrack(Path::new("gone"));
        let cs = cap
            .capture(&batch(&[(JournalOp::Delete, "gone", false)]))
            .unwrap();
        assert!(cs.ops.is_empty(), "{:?}", op_paths(&cs));
        cap.release().unwrap();
    }
    // Phase `a_path_marked_dirty_is_examined_without_a_journal_entry`.
    fn a_path_marked_dirty_is_examined_without_a_journal_entry() {
        let Some(s) = btrfs_scratch("capture_dirty") else {
            return;
        };
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        cap.capture(&batch(&[])).unwrap();
        fs::write(s.live.join("quiet"), b"never journaled").unwrap();
        let cs = cap.capture(&batch(&[])).unwrap();
        assert!(cs.ops.is_empty());
        cap.mark_dirty(Path::new("quiet"));
        let cs = cap.capture(&batch(&[])).unwrap();
        assert_eq!(op_paths(&cs), ["+quiet"]);
        cap.release().unwrap();
    }
    // Phase `an_invalidated_path_is_captured_again_or_removed`.
    fn an_invalidated_path_is_captured_again_or_removed() {
        let Some(s) = btrfs_scratch("capture_invalidate") else {
            return;
        };
        fs::write(s.live.join("kept"), b"k").unwrap();
        fs::write(s.live.join("moved"), b"m").unwrap();
        let mut cfg = CaptureConfig::new(&s.live, &s.epochs);
        cfg.hash_content = false;
        let mut cap = EpochCapture::new(cfg).unwrap();
        cap.capture(&batch(&[])).unwrap();

        // Publishing both failed: an unchanged file is captured again...
        cap.invalidate(Path::new("kept"));
        cap.invalidate(Path::new("moved"));
        // ...and one renamed away before the next epoch is moved, with its
        // content published again.
        fs::rename(s.live.join("moved"), s.live.join("elsewhere")).unwrap();
        let cs = cap
            .capture(&batch(&[(
                JournalOp::Rename {
                    from: Some("moved".into()),
                },
                "elsewhere",
                false,
            )]))
            .unwrap();
        let mut got = op_paths(&cs);
        got.sort();
        assert_eq!(got, ["+kept", "moved>elsewhere"]);
        assert_eq!(moves(&cs), ["moved -> elsewhere *"]);
        cap.release().unwrap();
    }
    // Phase `a_rescan_finds_changes_the_journal_never_reported` (defined above).
    a_rescan_finds_changes_the_journal_never_reported();
    // Phase `a_resumed_capture_finds_what_changed_while_nothing_was_journaling` (defined above).
    a_resumed_capture_finds_what_changed_while_nothing_was_journaling();
    // Phase `a_tracked_path_is_diffed_against_and_its_local_removal_is_captured` (defined above).
    a_tracked_path_is_diffed_against_and_its_local_removal_is_captured();
    // Phase `a_path_marked_dirty_is_examined_without_a_journal_entry` (defined above).
    a_path_marked_dirty_is_examined_without_a_journal_entry();
    // Phase `an_invalidated_path_is_captured_again_or_removed` (defined above).
    an_invalidated_path_is_captured_again_or_removed();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn capture_skips_local_only_special_and_staging_files_while_writers_continue() {
    // Phase `local_only_trees_and_special_files_are_never_captured`.
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
    // Phase `staging_files_are_never_captured`.
    fn staging_files_are_never_captured() {
        let Some(s) = btrfs_scratch("capture_staging") else {
            return;
        };
        fs::write(s.live.join(".tilo-staging-1-0"), b"half written").unwrap();
        fs::write(s.live.join("real"), b"r").unwrap();
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        let cs = cap.capture(&batch(&[])).unwrap();
        assert_eq!(op_paths(&cs), ["+real"]);
        cap.release().unwrap();
    }
    // Phase `writers_keep_writing_while_epochs_are_cut`.
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
    // Phase `local_only_trees_and_special_files_are_never_captured` (defined above).
    local_only_trees_and_special_files_are_never_captured();
    // Phase `staging_files_are_never_captured` (defined above).
    staging_files_are_never_captured();
    // Phase `writers_keep_writing_while_epochs_are_cut` (defined above).
    writers_keep_writing_while_epochs_are_cut();
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

fn op_paths(cs: &Changeset) -> Vec<String> {
    cs.ops
        .iter()
        .map(|op| match op {
            ChangeOp::Upsert { path, .. } => format!("+{}", path.display()),
            ChangeOp::Remove { path, .. } => format!("-{}", path.display()),
            ChangeOp::Move { from, path, .. } => {
                format!("{}>{}", from.display(), path.display())
            }
        })
        .collect()
}
