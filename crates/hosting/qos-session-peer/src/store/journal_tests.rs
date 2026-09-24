//! Crash consistency of journaled session commits through the real store.
use super::test_fault::{Point, with_fault};
use super::test_journal::with_journal;
use super::*;

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
/// trailing signature.
fn successor(previous: &[u8], step: u64) -> Vec<u8> {
    let mut next = previous.to_vec();
    next[3] = step as u8;
    let at = 4096 + (step as usize * 811) % (next.len() / 2);
    next.splice(at..at, pseudo(48, step + 1000));
    let tail = next.len() - 64;
    next[tail..].copy_from_slice(&pseudo(64, step + 5000));
    next
}
fn journal_path(root: &Path) -> PathBuf {
    root.join(journal::NAME)
}
fn reopen(root: &Path) -> (SessionStore, Vec<u8>) {
    let mut store = SessionStore::open(root).unwrap();
    let bytes = store.read_for_owner().unwrap();
    (store, bytes)
}
/// Recovered state after a simulated crash, leaving the files untouched.
fn recovered_after_crash(root: &Path) -> Vec<u8> {
    let (store, bytes) = reopen(root);
    store.abandon();
    bytes
}

#[test]
fn journaled_commits_leave_checkpoint_untouched_and_reopen_exactly() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let base = pseudo(96 * 1024, 1);
        store.commit(&base).unwrap();
        let mut current = base.clone();
        for step in 1..=40 {
            current = successor(&current, step);
            store.commit(&current).unwrap();
            assert_eq!(
                store.acknowledged_digest().unwrap(),
                *blake3::hash(&current).as_bytes()
            );
        }
        assert_eq!(fs::read(root.join("session.bin")).unwrap(), base);
        let journal = fs::metadata(journal_path(&root)).unwrap().len();
        assert!(
            journal < 16 * 1024,
            "journal is {journal} bytes for 40 commits"
        );
        assert_eq!(store.read().unwrap(), current);
        store.abandon();
        let (mut store, recovered) = reopen(&root);
        assert_eq!(recovered, current);
        // Appending continues after recovery.
        current = successor(&current, 41);
        store.commit(&current).unwrap();
        store.abandon();
        assert_eq!(recovered_after_crash(&root), current);
    });
}

#[test]
fn orderly_drop_folds_the_journal_into_the_checkpoint() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let mut current = pseudo(64 * 1024, 2);
        store.commit(&current).unwrap();
        for step in 1..=5 {
            current = successor(&current, step);
            store.commit(&current).unwrap();
        }
        drop(store);
        assert_eq!(fs::read(root.join("session.bin")).unwrap(), current);
        assert_eq!(fs::metadata(journal_path(&root)).unwrap().len(), 0);
        assert_eq!(recovered_after_crash(&root), current);
    });
}

#[test]
fn torn_journal_tails_recover_the_last_complete_commit_and_are_cut_before_appending() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let mut states = vec![pseudo(64 * 1024, 3)];
        store.commit(&states[0]).unwrap();
        let mut ends = vec![];
        for step in 1..=4 {
            let next = successor(states.last().unwrap(), step);
            store.commit(&next).unwrap();
            ends.push(fs::metadata(journal_path(&root)).unwrap().len());
            states.push(next);
        }
        store.abandon();
        let complete = fs::read(journal_path(&root)).unwrap();
        // Every cut inside the last record, and garbage after it.
        let last_start = ends[2] as usize;
        for cut in (last_start..complete.len()).step_by(7) {
            fs::write(journal_path(&root), &complete[..cut]).unwrap();
            let (store, recovered) = reopen(&root);
            assert_eq!(recovered, states[3], "cut {cut}");
            store.abandon();
        }
        let mut garbage = complete.clone();
        garbage.extend_from_slice(&pseudo(333, 9));
        fs::write(journal_path(&root), &garbage).unwrap();
        let (mut store, recovered) = reopen(&root);
        assert_eq!(recovered, states[4]);
        // The next append cuts the garbage first, so its record is reachable.
        let next = successor(&states[4], 5);
        store.commit(&next).unwrap();
        store.abandon();
        assert_eq!(recovered_after_crash(&root), next);
        // A half-written record after a cut is itself a torn tail.
        let mut torn = fs::read(journal_path(&root)).unwrap();
        torn.truncate(torn.len() - 10);
        fs::write(journal_path(&root), &torn).unwrap();
        assert_eq!(recovered_after_crash(&root), states[4]);
    });
}

#[test]
fn append_faults_poison_and_reopen_yields_old_or_new_never_other() {
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        with_journal(|| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("state");
            let mut store = SessionStore::create(&root).unwrap();
            let mut old = pseudo(64 * 1024, 4);
            store.commit(&old).unwrap();
            old = successor(&old, 1);
            store.commit(&old).unwrap();
            let new = successor(&old, 2);
            assert!(with_fault(&root, point, || store.commit(&new)).is_err());
            assert!(store.is_poisoned());
            assert!(store.acknowledged_digest().is_err());
            assert!(
                store.commit(&new).is_err(),
                "poisoned store refuses commits"
            );
            drop(store);
            let (mut store, recovered) = reopen(&root);
            if point == Point::BeforeWrite {
                assert_eq!(recovered, old);
            } else {
                // The record was written; its acknowledgement was withheld.
                assert_eq!(recovered, new);
            }
            let after = successor(&recovered, 3);
            store.commit(&after).unwrap();
            store.abandon();
            assert_eq!(recovered_after_crash(&root), after);
        });
    }
}

#[test]
fn failed_journal_barrier_is_not_an_acknowledgement() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let old = pseudo(64 * 1024, 5);
        store.commit(&old).unwrap();
        let first = successor(&old, 1);
        store.commit(&first).unwrap();
        let new = successor(&first, 2);
        assert!(
            store
                .commit_with_barrier(&new, |_| bail!("injected journal barrier"), None)
                .is_err()
        );
        assert!(store.acknowledged_digest().is_err());
        drop(store);
        let recovered = recovered_after_crash(&root);
        assert!(recovered == first || recovered == new);
    });
}

#[test]
fn journal_bound_checkpoints_and_supersedes_older_records() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let mut current = pseudo(128 * 1024, 6);
        store.commit(&current).unwrap();
        let mut checkpointed = false;
        for step in 1..=120u64 {
            // ~20 KiB literal per commit: the 1 MiB bound is crossed quickly.
            let at = (step as usize * 4099) % (current.len() - 20 * 1024);
            current[at..at + 20 * 1024].copy_from_slice(&pseudo(20 * 1024, step + 77));
            store.commit(&current).unwrap();
            let journal = fs::metadata(journal_path(&root)).unwrap().len();
            assert!(journal <= JOURNAL_MIN_BYTES);
            if fs::read(root.join("session.bin")).unwrap() == current {
                checkpointed = true;
                assert_eq!(journal, 0);
            }
        }
        assert!(checkpointed, "the journal bound forced a checkpoint");
        store.abandon();
        assert_eq!(recovered_after_crash(&root), current);
    });
}

#[test]
fn a_stale_journal_behind_a_newer_checkpoint_is_ignored() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let base = pseudo(64 * 1024, 7);
        store.commit(&base).unwrap();
        let first = successor(&base, 1);
        store.commit(&first).unwrap();
        let second = successor(&first, 2);
        store.commit(&second).unwrap();
        store.abandon();
        // Crash after a checkpoint's rename and before the journal was emptied.
        let newer = successor(&second, 3);
        fs::write(root.join("session.bin.tmp"), &newer).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.join("session.bin.tmp"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        fs::rename(root.join("session.bin.tmp"), root.join("session.bin")).unwrap();
        let (mut store, recovered) = reopen(&root);
        assert_eq!(recovered, newer);
        // The next append replaces the stale journal with one bound to `newer`.
        let next = successor(&newer, 4);
        store.commit(&next).unwrap();
        store.abandon();
        assert_eq!(recovered_after_crash(&root), next);
    });
}

#[test]
fn checkpointing_bytes_equal_to_the_current_checkpoint_empties_the_journal() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let base = pseudo(64 * 1024, 8);
        store.commit(&base).unwrap();
        let mut current = base.clone();
        for step in 1..=3 {
            current = successor(&current, step);
            store.commit(&current).unwrap();
        }
        // An unrelated state forces a checkpoint...
        let unrelated = pseudo(64 * 1024, 9);
        store.commit(&unrelated).unwrap();
        assert_eq!(fs::read(root.join("session.bin")).unwrap(), unrelated);
        // ...and returning to the previous checkpoint's exact bytes must not
        // leave records that would replay onto it.
        for step in 4..=6 {
            current = successor(&current, step);
            store.commit(&current).unwrap();
        }
        store.commit(&unrelated).unwrap();
        store.abandon();
        assert_eq!(recovered_after_crash(&root), unrelated);
        assert_eq!(fs::metadata(journal_path(&root)).unwrap().len(), 0);
    });
}

#[test]
fn a_store_opened_without_recovery_still_retires_an_older_journal() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        let base = pseudo(64 * 1024, 10);
        store.commit(&base).unwrap();
        let next = successor(&base, 1);
        store.commit(&next).unwrap();
        store.abandon();
        // Administrative paths read without priming the journal, then commit
        // the checkpoint's exact bytes.
        let mut store = SessionStore::open(&root).unwrap();
        assert_eq!(store.read().unwrap(), next);
        store.commit(&base).unwrap();
        store.abandon();
        assert_eq!(recovered_after_crash(&root), base);
    });
}

#[test]
fn owner_mutations_journal_and_crash_recovery_reopens_the_exact_owner_state() {
    use ui::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = crate::SessionOwner::initialize(&path).unwrap();
        let checkpoint = fs::read(path.join("session.bin")).unwrap();
        let app = "so.tilo.word-processor";
        let record = "tab:0000000000000001";
        for step in 0..12i64 {
            let mut candidate = owner.peer().clone();
            let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
            document.put(ROOT, "journal-step", step).unwrap();
            candidate
                .publish_document(
                    app,
                    record,
                    document.save(),
                    &owner.identity.native,
                    web_time::SystemTime::now(),
                )
                .unwrap();
            owner
                .commit_mutation_candidate(candidate, owner.directory().to_vec())
                .unwrap();
        }
        let status = owner.status();
        assert_eq!(
            fs::read(path.join("session.bin")).unwrap(),
            checkpoint,
            "mutations appended to the journal instead of rewriting the checkpoint"
        );
        assert!(fs::metadata(journal_path(&path)).unwrap().len() > 0);
        owner.crash_store_on_drop();
        drop(owner);
        let owner = crate::SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), status);
        let document = AutoCommit::load(&owner.peer().document(app, record).unwrap()).unwrap();
        assert_eq!(
            document
                .get(ROOT, "journal-step")
                .unwrap()
                .unwrap()
                .0
                .to_i64(),
            Some(11)
        );
    });
}
