//! Crash consistency of journaled Cloud Store commits (analogous to
//! `qos-session-peer/src/store/journal_tests.rs`), through the real writer,
//! the real reconciled reader and the real Store.
use super::test_journal::{Point, with_fault, with_journal};
use super::*;
use atomic_file::{PublicationState, StorageDirectory, test_support::Checkpoint};
use std::{fs, path::PathBuf};

fn pseudo(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
/// A realistic successor: counter near the start, an inserted entry, a new
/// trailing value.
fn successor(previous: &[u8], step: u64) -> Vec<u8> {
    let mut next = previous.to_vec();
    next[3] = step as u8;
    let at = 4096 + (step as usize * 811) % (next.len() / 2);
    next.splice(at..at, pseudo(48, step + 1000));
    let tail = next.len() - 64;
    next[tail..].copy_from_slice(&pseudo(64, step + 5000));
    next
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    directory: std::fs::File,
}
fn fixture() -> Fixture {
    use std::os::unix::fs::DirBuilderExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let directory = std::fs::File::open(&root).unwrap();
    Fixture {
        _temp: temp,
        root,
        directory,
    }
}
impl Fixture {
    fn commit(&self, tip: &mut Option<Tip>, bytes: &[u8]) -> std::result::Result<(), WriteError> {
        write(&self.directory, &self.root, tip, bytes)
    }
    /// A fresh reader (as after a crash or in another process): the exact
    /// reconstructed state and the diff base for further commits.
    fn reopen(&self) -> (Vec<u8>, Option<Tip>) {
        // Another process: nothing this process wrote is remembered.
        self.forget();
        self.warm_reopen()
    }
    /// A reader in the writing process (may use what it remembers).
    fn warm_reopen(&self) -> (Vec<u8>, Option<Tip>) {
        let directory = StorageDirectory::from_file(self.directory.try_clone().unwrap()).unwrap();
        let persisted = read_reconciled(&directory, &self.root).unwrap().unwrap();
        (persisted.bytes.clone(), persisted.into_tip())
    }
    /// Forget what this process remembered about this fixture's files only
    /// (other tests run concurrently in the same process).
    fn forget(&self) {
        forget_durable_directory_for_test(&self.directory);
    }
    fn aggregate(&self) -> Aggregate {
        self.forget();
        let directory = StorageDirectory::from_file(self.directory.try_clone().unwrap()).unwrap();
        let mut aggregate = Aggregate::open(&directory).unwrap();
        aggregate.read_reconciled(&directory).unwrap();
        aggregate
    }
    fn checkpoint(&self) -> Vec<u8> {
        fs::read(self.root.join("state.json")).unwrap()
    }
    fn journal(&self) -> Vec<u8> {
        fs::read(self.root.join("state.journal")).unwrap_or_default()
    }
    fn set_journal(&self, bytes: &[u8]) {
        fs::write(self.root.join("state.journal"), bytes).unwrap();
    }
}

#[test]
fn journaled_commits_leave_checkpoint_untouched_and_reopen_exactly() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(96 * 1024, 1);
        f.commit(&mut tip, &base).unwrap();
        let mut current = base.clone();
        for step in 1..=20 {
            current = successor(&current, step);
            f.commit(&mut tip, &current).unwrap();
        }
        assert_eq!(f.checkpoint(), base);
        let journal = f.journal().len();
        assert!(
            journal < 8 * 1024,
            "journal is {journal} bytes for 20 commits"
        );
        let (recovered, mut tip) = f.reopen();
        assert_eq!(recovered, current);
        // Appending continues from a reopened reader's base.
        current = successor(&current, 41);
        f.commit(&mut tip, &current).unwrap();
        assert_eq!(f.reopen().0, current);
        assert_eq!(f.checkpoint(), base);
    });
}

#[test]
fn every_torn_tail_recovers_the_last_complete_commit_and_is_cut_before_appending() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let mut states = vec![pseudo(64 * 1024, 3)];
        f.commit(&mut tip, &states[0]).unwrap();
        let mut ends = vec![];
        for step in 1..=4 {
            let next = successor(states.last().unwrap(), step);
            f.commit(&mut tip, &next).unwrap();
            ends.push(f.journal().len());
            states.push(next);
        }
        let complete = f.journal();
        // Every cut: before the header, inside each record, at each boundary.
        for cut in 0..complete.len() {
            f.set_journal(&complete[..cut]);
            let (recovered, _) = f.reopen();
            let records = ends.iter().filter(|end| **end <= cut).count();
            assert_eq!(recovered, states[records], "cut {cut}");
        }
        // Garbage after the last record, then the next append cuts it first.
        let mut garbage = complete.clone();
        garbage.extend_from_slice(&pseudo(333, 9));
        f.set_journal(&garbage);
        let aggregate = f.aggregate();
        assert!(
            !aggregate.clean(),
            "a torn tail is never a comparable observation"
        );
        let (recovered, mut tip) = f.reopen();
        assert_eq!(recovered, states[4]);
        let next = successor(&states[4], 5);
        f.commit(&mut tip, &next).unwrap();
        assert_eq!(f.reopen().0, next);
        // A half-written record after a cut is itself a torn tail.
        let mut torn = f.journal();
        torn.truncate(torn.len() - 10);
        f.set_journal(&torn);
        assert_eq!(f.reopen().0, states[4]);
    });
}

/// A checkpoint plus one journaled commit (`old`).
fn journaled() -> (Vec<u8>, Fixture, Option<Tip>) {
    let f = fixture();
    let mut tip = None;
    let base = pseudo(64 * 1024, 4);
    f.commit(&mut tip, &base).unwrap();
    let old = successor(&base, 1);
    f.commit(&mut tip, &old).unwrap();
    (old, f, tip)
}
/// Fail the commit after `journaled`.
fn faulted_append(
    fault: impl FnOnce(&Fixture, &mut Option<Tip>, &[u8]) -> WriteError,
) -> (Vec<u8>, Vec<u8>, Fixture, WriteError) {
    let (old, f, mut tip) = journaled();
    let new = successor(&old, 2);
    let error = fault(&f, &mut tip, &new);
    assert!(tip.is_none(), "a failed write leaves no diff base");
    (old, new, f, error)
}

#[test]
fn append_faults_yield_old_or_new_and_withhold_acknowledgement() {
    with_journal(|| {
        for (point, expect_new, state) in [
            (Point::BeforeWrite, false, PublicationState::NotPublished),
            (
                Point::BeforeBarrier,
                true,
                PublicationState::PublishedDurabilityUnknown,
            ),
        ] {
            let (old, new, f, error) = faulted_append(|f, tip, new| {
                with_fault(&f.root, point, || f.commit(tip, new)).unwrap_err()
            });
            assert_eq!(error.state, state, "{point:?}");
            let (recovered, mut tip) = f.reopen();
            // The record reached the file or it did not; nothing else.
            assert_eq!(
                &recovered,
                if expect_new { &new } else { &old },
                "{point:?}"
            );
            let after = successor(&recovered, 3);
            f.commit(&mut tip, &after).unwrap();
            assert_eq!(f.reopen().0, after);
        }
        // The crate-wide save-failure hook maps onto the same append phases.
        for (checkpoint, expect_new) in [
            (Checkpoint::BeforePublish, false),
            (Checkpoint::AfterPublish, true),
            (Checkpoint::BeforeCommit, true),
        ] {
            let (old, new, f, _) = faulted_append(|f, tip, new| {
                test_support::with_save_failure(&f.root, checkpoint, || f.commit(tip, new))
                    .unwrap_err()
            });
            assert_eq!(
                f.reopen().0,
                if expect_new { new } else { old },
                "{checkpoint:?}"
            );
        }
    });
}

#[test]
fn checkpoint_faults_yield_old_or_new() {
    with_journal(|| {
        for (checkpoint, expect_new) in [
            (Checkpoint::BeforePublish, false),
            (Checkpoint::AfterPublish, true),
            (Checkpoint::BeforeCommit, true),
        ] {
            let (old, f, mut tip) = journaled();
            let unrelated = pseudo(64 * 1024, 44);
            let error = test_support::with_save_failure(&f.root, checkpoint, || {
                f.commit(&mut tip, &unrelated)
            })
            .unwrap_err();
            assert!(tip.is_none());
            if expect_new {
                // A published checkpoint is never reported as unpublished.
                assert_ne!(
                    error.state,
                    PublicationState::NotPublished,
                    "{checkpoint:?}"
                );
            }
            assert_eq!(
                f.reopen().0,
                if expect_new { unrelated } else { old },
                "{checkpoint:?}"
            );
        }
        // Crash between a durable new checkpoint and the journal retirement:
        // the stale journal names the old checkpoint and is ignored.
        let (_, f, mut tip) = journaled();
        let unrelated = pseudo(64 * 1024, 45);
        let error = with_fault(&f.root, Point::BeforeRetire, || {
            f.commit(&mut tip, &unrelated)
        })
        .unwrap_err();
        assert_eq!(error.state, PublicationState::PublishedDurabilityUnknown);
        assert!(!f.journal().is_empty(), "the old journal is still on disk");
        let (recovered, mut tip) = f.reopen();
        assert_eq!(recovered, unrelated);
        let next = successor(&unrelated, 7);
        f.commit(&mut tip, &next).unwrap();
        assert_eq!(f.reopen().0, next);
    });
}

#[test]
fn journal_bound_checkpoints_and_supersedes_older_records() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let mut current = pseudo(128 * 1024, 6);
        f.commit(&mut tip, &current).unwrap();
        let mut checkpointed = false;
        for step in 1..=120u64 {
            // ~20 KiB literal per commit: the 1 MiB bound is crossed quickly.
            let at = (step as usize * 4099) % (current.len() - 20 * 1024);
            current[at..at + 20 * 1024].copy_from_slice(&pseudo(20 * 1024, step + 77));
            f.commit(&mut tip, &current).unwrap();
            let journal = f.journal().len() as u64;
            assert!(journal <= JOURNAL_MIN_BYTES);
            if f.checkpoint() == current {
                checkpointed = true;
                assert_eq!(journal, 0);
            }
        }
        assert!(checkpointed, "the journal bound forced a checkpoint");
        assert_eq!(f.reopen().0, current);
    });
}

#[test]
fn the_writing_process_reads_what_it_wrote_without_replay_and_others_reconstruct_it() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let mut current = pseudo(64 * 1024, 16);
        f.commit(&mut tip, &current).unwrap();
        for step in 1..=5 {
            current = successor(&current, step);
            f.commit(&mut tip, &current).unwrap();
            let (warm, warm_tip) = f.warm_reopen();
            assert_eq!(warm, current);
            let warm_tip = warm_tip.unwrap();
            assert_eq!(warm_tip.base.records, step as usize);
            // A different reader (cold) reconstructs the identical state and base.
            let (cold, cold_tip) = f.reopen();
            assert_eq!(cold, current);
            let cold_tip = cold_tip.unwrap();
            assert_eq!(cold_tip.base.valid_len, warm_tip.base.valid_len);
            assert_eq!(
                cold_tip.base.checkpoint_digest,
                warm_tip.base.checkpoint_digest
            );
        }
        // Once anything else changes the files, the remembered state is unused.
        let mut other = f.reopen().1;
        let theirs = successor(&current, 50);
        f.commit(&mut other, &theirs).unwrap();
        f.forget();
        let mut torn = f.journal();
        torn.extend_from_slice(&pseudo(20, 1));
        f.set_journal(&torn);
        assert_eq!(f.warm_reopen().0, theirs);
    });
}

#[test]
fn record_bound_checkpoints_so_every_read_replays_a_bounded_journal() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(64 * 1024, 15);
        f.commit(&mut tip, &base).unwrap();
        let mut current = base.clone();
        for step in 1..=JOURNAL_MAX_RECORDS as u64 {
            current = successor(&current, step);
            f.commit(&mut tip, &current).unwrap();
        }
        assert_eq!(
            f.checkpoint(),
            base,
            "{JOURNAL_MAX_RECORDS} records appended"
        );
        current = successor(&current, 1000);
        f.commit(&mut tip, &current).unwrap();
        assert_eq!(f.checkpoint(), current, "the next commit checkpoints");
        assert!(f.journal().is_empty());
        // A reader's base counts records too.
        for step in 1001..=1003 {
            let (_, mut reader) = f.reopen();
            current = successor(&current, step);
            f.commit(&mut reader, &current).unwrap();
            assert_eq!(reader.unwrap().base.records, (step - 1000) as usize);
        }
        assert_eq!(f.reopen().0, current);
    });
}

#[test]
fn a_stale_journal_behind_a_newer_checkpoint_is_ignored() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(64 * 1024, 7);
        f.commit(&mut tip, &base).unwrap();
        let first = successor(&base, 1);
        f.commit(&mut tip, &first).unwrap();
        let second = successor(&first, 2);
        f.commit(&mut tip, &second).unwrap();
        // A crash after a checkpoint's rename and before the journal was retired.
        let newer = successor(&second, 3);
        let staged = f.root.join("state.json.tmp");
        fs::write(&staged, &newer).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fs::rename(&staged, f.root.join("state.json")).unwrap();
        let (recovered, mut tip) = f.reopen();
        assert_eq!(recovered, newer);
        assert!(!f.aggregate().clean(), "stale bytes are never comparable");
        // The next append replaces the stale journal with one bound to `newer`.
        let next = successor(&newer, 4);
        f.commit(&mut tip, &next).unwrap();
        assert_eq!(f.reopen().0, next);
    });
}

#[test]
fn checkpointing_bytes_equal_to_the_current_checkpoint_retires_the_journal() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(64 * 1024, 8);
        f.commit(&mut tip, &base).unwrap();
        let mut current = base.clone();
        for step in 1..=3 {
            current = successor(&current, step);
            f.commit(&mut tip, &current).unwrap();
        }
        // An unrelated state forces a checkpoint...
        let unrelated = pseudo(64 * 1024, 9);
        f.commit(&mut tip, &unrelated).unwrap();
        assert_eq!(f.checkpoint(), unrelated);
        // ...and returning to that checkpoint's exact bytes must not leave
        // records that would replay onto it.
        for step in 4..=6 {
            current = successor(&unrelated, step);
            f.commit(&mut tip, &current).unwrap();
        }
        let checkpoint = fs::metadata(f.root.join("state.json")).unwrap();
        // A small edit back to `unrelated` would itself be journaled; a diff
        // base the delta search cannot use (as after a large change) takes
        // the checkpoint path with the checkpoint's exact bytes.
        tip.as_mut().unwrap().current = pseudo(64 * 1024, 99);
        // Fault at the commit point of an unchanged checkpoint: old state.
        let mut failing = tip.clone();
        let error = with_fault(&f.root, Point::BeforeRetire, || {
            f.commit(&mut failing, &unrelated)
        })
        .unwrap_err();
        assert_eq!(error.state, PublicationState::PublicationUnknown);
        assert_eq!(f.reopen().0, current);
        f.commit(&mut tip, &unrelated).unwrap();
        {
            use std::os::unix::fs::MetadataExt;
            let after = fs::metadata(f.root.join("state.json")).unwrap();
            assert_eq!(
                after.ino(),
                checkpoint.ino(),
                "identical checkpoint not republished"
            );
        }
        assert_eq!(f.reopen().0, unrelated);
        assert!(f.journal().is_empty());
    });
}

#[test]
fn a_replaced_checkpoint_file_refuses_append() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(64 * 1024, 10);
        f.commit(&mut tip, &base).unwrap();
        let first = successor(&base, 1);
        f.commit(&mut tip, &first).unwrap();
        // Same bytes, another file: records appended now would name a
        // checkpoint this writer never observed.
        let staged = f.root.join("replacement");
        fs::write(&staged, &base).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fs::rename(&staged, f.root.join("state.json")).unwrap();
        let second = successor(&first, 2);
        let error = f.commit(&mut tip, &second).unwrap_err();
        assert_eq!(error.state, PublicationState::NotPublished);
        assert!(tip.is_none());
        assert_eq!(f.reopen().0, first);
    });
}

#[test]
fn a_writer_without_a_base_checkpoints_and_retires_the_journal() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(64 * 1024, 11);
        f.commit(&mut tip, &base).unwrap();
        let next = successor(&base, 1);
        f.commit(&mut tip, &next).unwrap();
        // `publish_bytes_typed` paths know no base: they write the
        // checkpoint's exact old bytes here.
        let mut none = None;
        f.commit(&mut none, &base).unwrap();
        assert_eq!(f.reopen().0, base);
        assert!(f.journal().is_empty());
    });
}

#[test]
fn baseline_mode_writes_whole_files_but_still_reads_the_journal() {
    let f = fixture();
    let base = pseudo(64 * 1024, 12);
    let next = successor(&base, 1);
    with_journal(|| {
        let mut tip = None;
        f.commit(&mut tip, &base).unwrap();
        f.commit(&mut tip, &next).unwrap();
    });
    // Journaling disabled (the QOS_HOST_PERF_BASELINE=1 writer): reads still
    // reconstruct the journal and the first commit retires it.
    let (recovered, tip) = f.reopen();
    assert_eq!(recovered, next);
    assert!(tip.is_none());
    let later = successor(&next, 2);
    f.commit(&mut None, &later).unwrap();
    assert_eq!(f.checkpoint(), later);
    assert!(f.journal().is_empty());
    assert_eq!(f.reopen().0, later);
}

#[test]
fn retained_observations_detect_every_commit() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(64 * 1024, 13);
        f.commit(&mut tip, &base).unwrap();
        let retained = f.aggregate();
        assert!(retained.same(&f.aggregate()));
        let mut current = base;
        for step in 1..=3 {
            current = successor(&current, step);
            f.commit(&mut tip, &current).unwrap();
            let retained_now = f.aggregate();
            assert!(!retained.same(&retained_now));
            assert!(retained_now.same(&f.aggregate()));
        }
        // A retirement (unchanged checkpoint) replaces the journal file.
        let before = f.aggregate();
        let checkpoint = f.checkpoint();
        let (_, mut base_tip) = f.reopen();
        base_tip.as_mut().unwrap().current = successor(&checkpoint, 99);
        f.commit(&mut base_tip, &checkpoint).unwrap();
        assert!(!before.same(&f.aggregate()));
        assert!(before.matches_unlocked(&f.directory) == before.same(&f.aggregate()));
    });
}

#[test]
fn orderly_compaction_folds_the_journal_into_the_checkpoint() {
    with_journal(|| {
        let f = fixture();
        fs::write(f.root.join("state.lock"), b"").unwrap();
        let mut tip = None;
        let mut current = pseudo(64 * 1024, 14);
        f.commit(&mut tip, &current).unwrap();
        for step in 1..=5 {
            current = successor(&current, step);
            f.commit(&mut tip, &current).unwrap();
        }
        assert!(compact(&f.root).unwrap());
        assert_eq!(f.checkpoint(), current);
        assert!(f.journal().is_empty());
        assert_eq!(f.reopen().0, current);
        assert!(!compact(&f.root).unwrap(), "nothing left to fold");
    });
}

/// The real Store: journaled commits, reopen reconstructs the exact State,
/// and a retained decode is never reused across another Store's append.
#[test]
fn store_commits_journal_and_retained_decodes_see_every_append() {
    with_journal(|| {
        let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        let checkpoint = fs::read(dir.path().join("state.json")).unwrap();
        let mut now = 1_000_000_000_000u64;
        for _ in 0..5 {
            now += 400_000_000_000;
            store.inspect(now).unwrap();
        }
        let generation = store.state.generation;
        let expected = serde_json::to_vec(&store.state).unwrap();
        drop(store);
        assert_eq!(
            fs::read(dir.path().join("state.json")).unwrap(),
            checkpoint,
            "commits appended to the journal instead of rewriting state.json"
        );
        assert!(
            !fs::read(dir.path().join("state.journal"))
                .unwrap()
                .is_empty()
        );
        let reopened = crate::Store::open(dir.path(), "agent-admission").unwrap();
        assert_eq!(serde_json::to_vec(&reopened.state).unwrap(), expected);
        drop(reopened);

        let mut retained = None;
        let (store, opened) = crate::delivery::access::try_open_retained(
            dir.path(),
            "agent-admission",
            &mut retained,
            &mut || true,
        )
        .unwrap();
        assert_eq!(store.state.generation, generation);
        let mut retained = opened.retain(store);
        assert!(retained.is_some());
        // Another component commits through the journal meanwhile.
        let mut other = crate::Store::open(dir.path(), "agent-admission").unwrap();
        other.inspect(now + 400_000_000_000).unwrap();
        let newer = other.state.generation;
        drop(other);
        assert!(
            !retained
                .as_ref()
                .unwrap()
                .matches(&std::fs::File::open(dir.path()).unwrap())
        );
        let (store, opened) = crate::delivery::access::try_open_retained(
            dir.path(),
            "agent-admission",
            &mut retained,
            &mut || true,
        )
        .unwrap();
        assert!(
            !opened.reused,
            "a journal append invalidates the retained decode"
        );
        assert_eq!(store.state.generation, newer);
        // Unchanged: reused, and a commit from the reused decode journals.
        let mut retained = opened.retain(store);
        let (mut store, opened) = crate::delivery::access::try_open_retained(
            dir.path(),
            "agent-admission",
            &mut retained,
            &mut || true,
        )
        .unwrap();
        assert!(opened.reused);
        let before = fs::metadata(dir.path().join("state.journal"))
            .unwrap()
            .len();
        store.inspect(now + 800_000_000_000).unwrap();
        assert!(
            fs::metadata(dir.path().join("state.journal"))
                .unwrap()
                .len()
                > before
        );
        let expected = serde_json::to_vec(&store.state).unwrap();
        drop(store);
        let reopened = crate::Store::open(dir.path(), "agent-admission").unwrap();
        assert_eq!(serde_json::to_vec(&reopened.state).unwrap(), expected);
    });
}

/// A failed append latches the Store: no acknowledgement, no further commit.
#[test]
fn store_failed_journal_barrier_is_not_acknowledged() {
    with_journal(|| {
        let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        let mut now = 1_000_000_000_000u64;
        now += 400_000_000_000;
        store.inspect(now).unwrap();
        let acknowledged = store.state.generation;
        now += 400_000_000_000;
        let failed = with_fault(dir.path(), Point::BeforeBarrier, || store.inspect(now));
        let error = failed.unwrap_err();
        assert!(
            error.chain().any(|cause| cause
                .downcast_ref::<crate::UncertainStoreCommit>()
                .is_some()
                || cause.to_string().contains("injected")),
            "{error:#}"
        );
        assert_eq!(store.state.generation, acknowledged, "state not advanced");
        assert!(
            store.inspect(now + 400_000_000_000).is_err(),
            "latched until reopen"
        );
        drop(store);
        let reopened = crate::Store::open(dir.path(), "agent-admission").unwrap();
        // The record reached the file before the failed barrier.
        assert_eq!(reopened.state.generation, acknowledged + 1);
    });
}

/// Paired measurement on a real aggregate (not run by default):
/// `QOS_CLOUD_STATE_SAMPLE=<dir with state.json> QOS_CLOUD_STATE_MACHINE=<id>
/// cargo test -p qos-cloud-host --lib measure_commit_io -- --ignored --nocapture`.
/// "baseline" is the whole-file writer (`journal_enabled() == false`, what
/// `QOS_HOST_PERF_BASELINE=1` selects); "journal" is the default writer.
#[test]
#[ignore = "manual I/O benchmark; requires QOS_CLOUD_STATE_SAMPLE"]
fn measure_commit_io() {
    let Some(sample) = std::env::var_os("QOS_CLOUD_STATE_SAMPLE") else {
        eprintln!("QOS_CLOUD_STATE_SAMPLE unset; skipped");
        return;
    };
    let machine = std::env::var("QOS_CLOUD_STATE_MACHINE").unwrap();
    let commits: usize = std::env::var("QOS_CLOUD_STATE_COMMITS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let sample = fs::read(std::path::Path::new(&sample).join("state.json")).unwrap();
    let run = |journal: bool, cold: bool| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::write(root.join("state.json"), &sample).unwrap();
            fs::set_permissions(root.join("state.json"), fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let body = || {
            // Migrations (if any) commit once on the first open.
            drop(crate::Store::open(&root, &machine).unwrap());
            let size = fs::metadata(root.join("state.json")).unwrap().len();
            let before = (
                counters::load(&counters::BYTES_WRITTEN),
                counters::load(&counters::APPENDS),
                counters::load(&counters::CHECKPOINTS),
                counters::load(&counters::COMMIT_NS),
                counters::load(&counters::DELTA_NS),
            );
            let mut latencies = Vec::with_capacity(commits);
            let mut opens = Vec::with_capacity(commits);
            let mut now = 2_000_000_000_000_000_000u64;
            for _ in 0..commits {
                // One transaction per turn, as the Cloud components do: a
                // full reconciled read and decode, then one small commit.
                if cold {
                    // Every read as if by another process.
                    forget_durable_for_test();
                }
                let began = std::time::Instant::now();
                let mut store = crate::Store::open(&root, &machine).unwrap();
                opens.push(began.elapsed());
                now += 400_000_000_000;
                let began = std::time::Instant::now();
                store.inspect(now).unwrap();
                latencies.push(began.elapsed());
            }
            let bytes = counters::load(&counters::BYTES_WRITTEN) - before.0;
            let appends = counters::load(&counters::APPENDS) - before.1;
            let checkpoints = counters::load(&counters::CHECKPOINTS) - before.2;
            let write_ns = counters::load(&counters::COMMIT_NS) - before.3;
            let delta_ns = counters::load(&counters::DELTA_NS) - before.4;
            latencies.sort();
            opens.sort();
            let pct =
                |values: &[std::time::Duration], p: usize| values[(values.len() - 1) * p / 100];
            eprintln!(
                "{}: state {size} B, {commits} commits: bytes/commit {} appends {appends} checkpoints {checkpoints} storage write mean {:?} (of which digest+delta {:?}) commit p50 {:?} p95 {:?} open+read p50 {:?} p95 {:?} journal {} B",
                match (journal, cold) {
                    (false, _) => "baseline",
                    (true, false) => "journal (same-process reads)",
                    (true, true) => "journal (cold reads)",
                },
                bytes / commits as u64,
                std::time::Duration::from_nanos(write_ns / commits as u64),
                std::time::Duration::from_nanos(delta_ns / commits as u64),
                pct(&latencies, 50),
                pct(&latencies, 95),
                pct(&opens, 50),
                pct(&opens, 95),
                fs::metadata(root.join("state.journal")).map_or(0, |m| m.len()),
            );
        };
        if journal { with_journal(body) } else { body() }
    };
    for _ in 0..2 {
        run(false, false);
        run(true, false);
        run(true, true);
    }
}

/// Read-only opens of an unchanged aggregate perform no durability barrier
/// (fix plan B4): after this process wrote (or once reconciled) the files,
/// later readers reuse that knowledge; any foreign change reconciles again.
#[test]
fn unchanged_aggregate_reads_skip_reconciliation_barriers() {
    with_journal(|| {
        let f = fixture();
        let mut tip = None;
        let base = pseudo(32 * 1024, 11);
        f.commit(&mut tip, &base).unwrap();
        let next = successor(&base, 1);
        f.commit(&mut tip, &next).unwrap();
        let before = reconciles_for_test();
        for _ in 0..5 {
            assert_eq!(f.warm_reopen().0, next);
        }
        assert_eq!(reconciles_for_test(), before, "own writes need no barrier");
        // A reader that never saw a write (another process wrote) reconciles
        // once, then later readers reuse its observation.
        let (_, _) = f.reopen();
        let after_foreign = reconciles_for_test();
        assert_eq!(after_foreign, before + 1);
        for _ in 0..3 {
            assert_eq!(f.warm_reopen().0, next);
        }
        assert_eq!(reconciles_for_test(), after_foreign);
        // A changed journal (another writer) is read and reconciled again.
        let mut other = tip.clone();
        let third = successor(&next, 2);
        f.forget();
        f.commit(&mut other, &third).unwrap();
        f.forget();
        assert_eq!(f.warm_reopen().0, third);
        assert_eq!(reconciles_for_test(), after_foreign + 1);
    });
}
