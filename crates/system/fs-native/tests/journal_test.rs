//! The fanotify change journal on a dedicated store volume.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use fs_native::capture::{CaptureConfig, ChangeOp, EpochCapture};
use fs_native::journal::{ChangeJournal, JournalConfig, JournalOp};

use crate::support::fanotify_scratch;

fn describe(e: &fs_native::journal::JournalEntry) -> String {
    let p = e
        .path
        .as_ref()
        .map_or(String::new(), |p| p.display().to_string());
    match &e.op {
        JournalOp::Rename { from } => format!(
            "rename {}->{p}",
            from.as_ref()
                .map_or_else(|| "<outside>".to_owned(), |f| f.display().to_string())
        ),
        op => format!("{} {p}", format!("{op:?}").to_lowercase()),
    }
}

fn drain(j: &mut ChangeJournal) -> Vec<String> {
    j.pump_until_idle(Duration::from_millis(150), Duration::from_secs(10))
        .unwrap();
    j.take().entries.iter().map(describe).collect()
}

fn assert_subsequence(got: &[String], want: &[&str]) {
    let mut it = got.iter();
    for w in want {
        assert!(
            it.any(|g| g == w),
            "{w:?} missing or out of order in {got:#?}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn the_journal_records_changes_in_order_once_per_batch_and_across_epochs() {
    // Phase `journal_records_every_change_in_writer_order`.
    fn journal_records_every_change_in_writer_order() {
        let Some(s) = fanotify_scratch("journal_order") else {
            return;
        };
        // Renames cannot cross subvolumes, so the unwatched directory lives in
        // the same subvolume as the watched one.
        let watched = s.live.join("tree");
        let outside = s.live.join("outside");
        fs::create_dir_all(&watched).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let mut j = ChangeJournal::open(&s.store, JournalConfig::new(&watched)).unwrap();
        // Paths are resolved when events are read, so the journal is pumped
        // between steps as the daemon's reader thread would be.
        let mut got = Vec::new();
        fs::create_dir_all(watched.join("repo/objects")).unwrap();
        got.extend(drain(&mut j));
        fs::write(watched.join("repo/objects/1"), b"object").unwrap();
        got.extend(drain(&mut j));
        fs::write(watched.join("repo/HEAD.lock"), b"ref").unwrap();
        got.extend(drain(&mut j));
        fs::rename(watched.join("repo/HEAD.lock"), watched.join("repo/HEAD")).unwrap();
        got.extend(drain(&mut j));
        fs::rename(watched.join("repo"), watched.join("moved")).unwrap();
        got.extend(drain(&mut j));
        fs::write(outside.join("in"), b"i").unwrap();
        got.extend(drain(&mut j));
        fs::rename(outside.join("in"), watched.join("moved/in")).unwrap();
        got.extend(drain(&mut j));
        fs::rename(watched.join("moved/objects/1"), outside.join("out")).unwrap();
        got.extend(drain(&mut j));
        fs::remove_file(watched.join("moved/HEAD")).unwrap();
        got.extend(drain(&mut j));
        // Exactly this, in this order; nothing that happened only outside the
        // watched tree appears.
        assert_eq!(
            got,
            [
                "create repo",
                "create repo/objects",
                "create repo/objects/1",
                "modify repo/objects/1",
                "closewrite repo/objects/1",
                "create repo/HEAD.lock",
                "modify repo/HEAD.lock",
                "closewrite repo/HEAD.lock",
                "rename repo/HEAD.lock->repo/HEAD",
                "rename repo->moved",
                "rename <outside>->moved/in",
                "delete moved/objects/1",
                "delete moved/HEAD",
            ]
        );
    }
    // Phase `late_path_resolution_is_absorbed_by_capture`.
    fn late_path_resolution_is_absorbed_by_capture() {
        let Some(s) = fanotify_scratch("journal_late") else {
            return;
        };
        let mut j = ChangeJournal::open(&s.store, JournalConfig::new(&s.live)).unwrap();
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        cap.capture(&j.take()).unwrap();
        // Everything happens before the journal reads a single event, so the
        // creates are reported under the directory's final name.
        fs::create_dir_all(s.live.join("a/b")).unwrap();
        fs::write(s.live.join("a/b/f"), b"x").unwrap();
        fs::rename(s.live.join("a"), s.live.join("z")).unwrap();
        j.pump_until_idle(Duration::from_millis(150), Duration::from_secs(5))
            .unwrap();
        let cs = cap.capture(&j.take()).unwrap();
        let mut paths: Vec<String> = cs
            .ops
            .iter()
            .map(|o| o.path().display().to_string())
            .collect();
        paths.sort();
        assert_eq!(paths, ["z", "z/b", "z/b/f"]);
        cap.release().unwrap();
    }
    // Phase `repeated_writes_to_an_open_file_are_journaled_once_per_batch`.
    fn repeated_writes_to_an_open_file_are_journaled_once_per_batch() {
        let Some(s) = fanotify_scratch("journal_ratelimit") else {
            return;
        };
        let mut j = ChangeJournal::open(&s.store, JournalConfig::new(&s.live)).unwrap();
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(s.live.join("db"))
            .unwrap();
        for _ in 0..2000 {
            f.write_all(&[7u8; 512]).unwrap();
            // Keep the queue drained so events cannot merge in the kernel.
            j.pump(Some(Duration::ZERO)).unwrap();
        }
        j.pump_until_idle(Duration::from_millis(100), Duration::from_secs(5))
            .unwrap();
        let first = j.take();
        let modifies = first
            .entries
            .iter()
            .filter(|e| e.op == JournalOp::Modify)
            .count();
        assert_eq!(modifies, 1, "modify entries for 2000 writes");
        eprintln!(
            "{} raw events for 2000 writes; kernel-side suppression: {}",
            j.events_read(),
            j.kernel_mutes() > 0
        );
        // The next batch sees the file again.
        f.write_all(b"more").unwrap();
        drop(f);
        let got = drain(&mut j);
        assert_subsequence(&got, &["modify db", "closewrite db"]);
    }
    // Phase `queue_overflow_forces_a_rescan_that_finds_every_file`.
    fn queue_overflow_forces_a_rescan_that_finds_every_file() {
        let Some(s) = fanotify_scratch("journal_overflow") else {
            return;
        };
        let mut j = ChangeJournal::open(&s.store, JournalConfig::new(&s.live)).unwrap();
        let mut cap = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
        cap.capture(&j.take()).unwrap();
        // The default queue holds 16384 events; create more without reading.
        for d in 0..20u32 {
            let dir = s.live.join(format!("d{d}"));
            fs::create_dir(&dir).unwrap();
            for f in 0..1000 {
                fs::write(dir.join(format!("f{f}")), [u8::try_from(d).unwrap(); 64]).unwrap();
            }
        }
        j.pump_until_idle(Duration::from_millis(200), Duration::from_secs(30))
            .unwrap();
        let batch = j.take();
        // Other tests on the volume can also make the journal ask for a rescan
        // (their directories vanish before events are read); either reason
        // leads to the same comparison.
        assert!(batch.rescan.is_some());
        let cs = cap.capture(&batch).unwrap();
        assert!(cs.stats.full_rescan);
        let files = cs
            .ops
            .iter()
            .filter(
                |op| matches!(op, ChangeOp::Upsert { path, .. } if path.components().count() == 2),
            )
            .count();
        assert_eq!(files, 20_000);
        assert_eq!(cap.nodes().count(), 20_020);
        // After the rescan only the next real change is published.
        fs::write(s.live.join("d0/f0"), b"changed").unwrap();
        j.pump_until_idle(Duration::from_millis(150), Duration::from_secs(5))
            .unwrap();
        let cs = cap.capture(&j.take()).unwrap();
        let paths: Vec<PathBuf> = cs.ops.iter().map(|o| o.path().to_path_buf()).collect();
        assert_eq!(paths, [PathBuf::from("d0/f0")]);
        cap.release().unwrap();
    }
    // Phase `an_epoch_split_keeps_the_window_between_barriers_for_the_next_batch`.
    fn an_epoch_split_keeps_the_window_between_barriers_for_the_next_batch() {
        let Some(s) = fanotify_scratch("journal_barrier") else {
            return;
        };
        let mut j = ChangeJournal::open(&s.store, JournalConfig::new(&s.live)).unwrap();
        fs::write(s.live.join("before"), b"1").unwrap();
        let before = j.barrier(&s.epochs).unwrap();
        fs::write(s.live.join("during"), b"2").unwrap();
        let after = j.barrier(&s.epochs).unwrap();
        fs::write(s.live.join("after"), b"3").unwrap();
        let batch = j.take_epoch(before, after).unwrap();
        let names = |es: &[fs_native::journal::JournalEntry]| -> Vec<String> {
            let mut v: Vec<String> = es
                .iter()
                .filter_map(|e| e.path.as_ref().map(|p| p.display().to_string()))
                .collect();
            v.dedup();
            v
        };
        assert_eq!(names(&batch.entries), ["before", "during"]);
        j.pump_until_idle(Duration::from_millis(100), Duration::from_secs(5))
            .unwrap();
        assert_eq!(names(j.pending()), ["during", "after"]);
    }
    // Phase `journal_records_every_change_in_writer_order` (defined above).
    journal_records_every_change_in_writer_order();
    // Phase `late_path_resolution_is_absorbed_by_capture` (defined above).
    late_path_resolution_is_absorbed_by_capture();
    // Phase `repeated_writes_to_an_open_file_are_journaled_once_per_batch` (defined above).
    repeated_writes_to_an_open_file_are_journaled_once_per_batch();
    // Phase `queue_overflow_forces_a_rescan_that_finds_every_file` (defined above).
    queue_overflow_forces_a_rescan_that_finds_every_file();
    // Phase `an_epoch_split_keeps_the_window_between_barriers_for_the_next_batch` (defined above).
    an_epoch_split_keeps_the_window_between_barriers_for_the_next_batch();
}
