//! Crash consistency of the owner side log through the real store.
use super::test_fault::{Point, with_fault};
use super::test_journal::with_journal;
use super::*;

fn owner_store(root: &Path) -> (SessionStore, Vec<u8>, Vec<Vec<u8>>) {
    let mut store = SessionStore::open(root).unwrap();
    let state = store.read_for_owner().unwrap();
    let records = store.read_side_log_for_owner().unwrap();
    (store, state, records)
}
/// Records visible after a simulated crash, leaving the files untouched.
fn records_after_crash(root: &Path) -> Vec<Vec<u8>> {
    let (store, _, records) = owner_store(root);
    store.abandon();
    records
}
fn record(step: u8) -> Vec<u8> {
    vec![step; 40 + usize::from(step)]
}
fn created(root: &Path) -> SessionStore {
    let mut store = SessionStore::create(root).unwrap();
    store.commit(b"aggregate v1").unwrap();
    store
}

#[test]
fn appends_are_durable_and_survive_abrupt_death_and_compaction() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = created(&root);
        assert!(store.side_log_enabled());
        for step in 1..=5 {
            store.append_side_record(&record(step)).unwrap();
        }
        // A journaled commit without folding would be a caller bug; here the
        // orderly drop compacts the journal and must keep the side log.
        drop(store);
        assert_eq!(
            records_after_crash(&root),
            (1..=5).map(record).collect::<Vec<_>>()
        );
        let (mut store, state, records) = owner_store(&root);
        assert_eq!(state, b"aggregate v1");
        assert_eq!(records.len(), 5);
        store.append_side_record(&record(6)).unwrap();
        store.abandon();
        assert_eq!(records_after_crash(&root).len(), 6);
    });
}

#[test]
fn caller_commit_on_owner_store_folds_and_truncates_but_admin_commit_does_not() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = created(&root);
        store.append_side_record(&record(1)).unwrap();
        store.abandon();
        // Administrative store (no owner recovery): its commits never fold.
        let mut admin = SessionStore::open(&root).unwrap();
        let state = admin.read().unwrap();
        assert!(!admin.side_log_enabled());
        assert!(admin.append_side_record(&record(9)).is_err());
        admin.commit(&state).unwrap();
        drop(admin);
        assert_eq!(records_after_crash(&root), vec![record(1)]);
        // Owner store: an aggregate commit folds (the owner applied them).
        let (mut store, state, records) = owner_store(&root);
        assert_eq!(records, vec![record(1)]);
        store
            .commit(&[state.as_slice(), b" folded"].concat())
            .unwrap();
        assert_eq!(fs::metadata(root.join(side_log::NAME)).unwrap().len(), 0);
        store.append_side_record(&record(2)).unwrap();
        store.abandon();
        assert_eq!(records_after_crash(&root), vec![record(2)]);
    });
}

#[test]
fn every_torn_tail_recovers_exactly_the_acknowledged_prefix_and_is_cut() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = created(&root);
        for step in 1..=4 {
            store.append_side_record(&record(step)).unwrap();
        }
        store.abandon();
        let path = root.join(side_log::NAME);
        let full = fs::read(&path).unwrap();
        let mut ends = vec![0];
        for step in 1..=4u8 {
            ends.push(ends.last().unwrap() + 4 + record(step).len() + 32);
        }
        for cut in 0..=full.len() {
            fs::write(&path, &full[..cut]).unwrap();
            let complete = ends.iter().filter(|end| **end <= cut).count() - 1;
            let (mut store, _, records) = owner_store(&root);
            assert_eq!(
                records,
                (1..=complete as u8).map(record).collect::<Vec<_>>(),
                "cut {cut}"
            );
            // The next append cuts the torn tail; the new record follows the prefix.
            store.append_side_record(&record(50)).unwrap();
            store.abandon();
            let mut expected = (1..=complete as u8).map(record).collect::<Vec<_>>();
            expected.push(record(50));
            assert_eq!(records_after_crash(&root), expected, "cut {cut}");
        }
    });
}

#[test]
fn a_fault_at_each_point_is_never_acknowledged_and_recovers_old_or_new() {
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        for first in [true, false] {
            with_journal(|| {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("state");
                let mut store = created(&root);
                if !first {
                    store.append_side_record(&record(1)).unwrap();
                }
                let failed = with_fault(&root, point, || store.append_side_record(&record(2)));
                assert!(failed.is_err(), "{point:?}");
                assert!(store.is_poisoned());
                assert!(store.append_side_record(&record(3)).is_err());
                assert!(store.commit(b"after").is_err());
                store.abandon();
                let old: Vec<_> = if first { vec![] } else { vec![record(1)] };
                let mut new = old.clone();
                new.push(record(2));
                let recovered = records_after_crash(&root);
                assert!(
                    recovered == old || recovered == new,
                    "{point:?} first={first}"
                );
                if point == Point::BeforeWrite {
                    assert_eq!(recovered, old);
                }
            });
        }
    }
}

#[test]
fn full_log_refuses_without_writing_or_poisoning() {
    with_journal(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = created(&root);
        let big = vec![5u8; 4000];
        let mut appended = 0;
        loop {
            match store.append_side_record(&big) {
                Ok(()) => appended += 1,
                Err(error) => {
                    assert!(error.is::<SideLogFull>(), "{error:#}");
                    break;
                }
            }
        }
        assert!(appended > 0 && !store.is_poisoned());
        let len = fs::metadata(root.join(side_log::NAME)).unwrap().len();
        assert!(len <= side_log::MAX_BYTES);
        // Folding makes room again.
        store.commit(b"aggregate folded").unwrap();
        store.append_side_record(&big).unwrap();
    });
}

#[test]
fn whole_file_mode_never_uses_the_side_log() {
    if super::test_journal::enabled() {
        return; // QOS_TEST_SESSION_JOURNAL=1 forces the journaled mode.
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let store = created(&root);
    assert!(!store.side_log_enabled());
}
