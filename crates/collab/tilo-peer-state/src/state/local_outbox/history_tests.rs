use super::*;
use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, ROOT};
use tilo_automerge::ApplyIncoming as _;

/// The dependency walk as it was written: every round asks the history what
/// it misses and rebuilds each missing change from the admitted record. Its
/// document is the reference [`build`] must reproduce byte for byte.
fn walked(mutation: &DocumentMutation, admitted: &Automerge) -> Result<Automerge, String> {
    let mut history = match mutation {
        DocumentMutation::Snapshot(bytes) => Automerge::load(bytes).map_err(err)?,
        DocumentMutation::Incremental(bytes) => {
            let mut history = Automerge::new();
            history.load_incremental(bytes).map_err(err)?;
            history
        }
    };
    loop {
        let missing = history.get_missing_deps(&[]);
        if missing.is_empty() {
            break;
        }
        let dependencies = missing
            .iter()
            .map(|hash| {
                admitted
                    .get_change_by_hash(hash)
                    .ok_or("retained publication dependency is not in admitted history")
            })
            .collect::<Result<Vec<_>, _>>()?;
        history.apply_changes(dependencies).map_err(err)?;
        if history.get_missing_deps(&[]) == missing {
            return Err("retained publication dependencies did not advance".into());
        }
    }
    if !history
        .get_heads()
        .iter()
        .all(|head| admitted.get_change_by_hash(head).is_some())
    {
        return Err("retained publication changes are not in admitted history".into());
    }
    Ok(history)
}

/// A small deterministic generator, so a failing history can be replayed.
struct Draw(u64);
impl Draw {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let below = u64::try_from(below).unwrap_or(u64::MAX);
        usize::try_from((self.0 >> 33).checked_rem(below).unwrap_or(0)).unwrap_or(0)
    }
}

/// A record written by `writers` replicas that edit, sync with each other at
/// random and, with `second_root`, one of which starts from nothing, merged
/// into one admitted record.
fn record(seed: u64, writers: usize, edits: usize) -> AutoCommit {
    record_from(seed, writers, edits, true)
}

fn record_from(seed: u64, writers: usize, edits: usize, second_root: bool) -> AutoCommit {
    let mut draw = Draw(seed);
    let mut replicas = (0..writers)
        .map(|writer| {
            let mut replica = AutoCommit::new();
            replica.set_actor(ActorId::from(vec![
                u8::try_from(writer.saturating_add(1))
                    .unwrap();
                16
            ]));
            replica
        })
        .collect::<Vec<_>>();
    // The replicas start from the first one's first change; with a second
    // root the last starts on its own.
    replicas[0].put(ROOT, "title", "start").unwrap();
    replicas[0].commit();
    let first = replicas[0].save();
    let joined = if second_root {
        writers.saturating_sub(1)
    } else {
        writers
    };
    for replica in replicas.iter_mut().take(joined).skip(1) {
        replica.load_incoming(&first).unwrap();
    }
    for edit in 0..edits {
        let writer = draw.next(writers);
        let key = format!("key{}", draw.next(6));
        replicas[writer].put(ROOT, key, edit as u64).unwrap();
        replicas[writer].commit();
        if draw.next(3) == 0 {
            let other = draw.next(writers);
            if other != writer {
                let (from, to) = if other < writer {
                    let (low, high) = replicas.split_at_mut(writer);
                    (&mut high[0], &mut low[other])
                } else {
                    let (low, high) = replicas.split_at_mut(other);
                    (&mut low[writer], &mut high[0])
                };
                to.merge_incoming(from).unwrap();
            }
        }
    }
    let mut admitted = AutoCommit::new();
    for replica in &mut replicas {
        admitted.merge_incoming(replica).unwrap();
    }
    admitted
}

/// Publications of the record's changes: each run of consecutive changes in
/// the record's order, as a device would have published them.
fn publications(admitted: &mut AutoCommit, seed: u64) -> Vec<DocumentMutation> {
    let changes = admitted.get_changes(&[]);
    let mut draw = Draw(seed ^ 0x5eed);
    let mut publications = Vec::new();
    for _ in 0..12 {
        let start = draw.next(changes.len());
        let length = draw.next(4).saturating_add(1);
        let bytes = changes
            .iter()
            .skip(start)
            .take(length)
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect::<Vec<_>>();
        publications.push(DocumentMutation::Incremental(bytes));
    }
    publications.push(DocumentMutation::Snapshot(admitted.save()));
    publications
}

#[test]
fn the_history_is_the_walked_history_byte_for_byte() {
    for seed in 0..24_u64 {
        let writers = usize::try_from(seed % 3).unwrap().saturating_add(2);
        let edits = usize::try_from(seed).unwrap().saturating_add(30);
        let mut admitted = record(seed, writers, edits);
        for mutation in publications(&mut admitted, seed) {
            let record = admitted.committed_document();
            let expected = walked(&mutation, record).map(|history| history.save());
            let built = build(&mutation, record).map(|history| history.save());
            assert_eq!(built, expected, "seed {seed}");
        }
    }
}

/// Every change of the record published on its own, in the record's order,
/// as a device publishes its edits one by one: each history is the walked
/// history byte for byte, whether it was built or extended from the one
/// before.
#[test]
fn an_extended_history_is_the_walked_history_byte_for_byte() {
    let mut extended_any = false;
    for seed in 0..16_u64 {
        let writers = usize::try_from(seed % 3).unwrap().saturating_add(1);
        let mut admitted = if writers == 1 {
            let mut alone = AutoCommit::new();
            alone.set_actor(ActorId::from(vec![9; 16]));
            for edit in 0..40_u64 {
                alone.put(ROOT, format!("key{}", edit % 5), edit).unwrap();
                alone.commit();
            }
            alone
        } else {
            record_from(seed, writers, 40, seed % 2 == 0)
        };
        forget();
        let before = rebuilds();
        let changes = admitted.get_changes(&[]);
        let record = admitted.committed_document();
        for change in &changes {
            let mutation = DocumentMutation::Incremental(change.raw_bytes().to_vec());
            let expected = walked(&mutation, record).map(|history| history.save());
            let built = of(key(&mutation), &mutation, record).map(|history| history.save());
            assert_eq!(built, expected, "seed {seed}");
        }
        let rebuilt = rebuilds().saturating_sub(before);
        extended_any |= rebuilt < changes.len();
        if writers == 1 {
            // A single writer's history is a chain: after the first change,
            // every history extends the one before.
            assert!(rebuilt <= 2, "seed {seed}: {rebuilt} rebuilt");
        }
        // Publications of several changes, the later first in some.
        forget();
        for (index, window) in changes.windows(3).enumerate() {
            let mut window = window.iter().collect::<Vec<_>>();
            if index % 2 == 1 {
                window.reverse();
            }
            let bytes = window
                .iter()
                .flat_map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>();
            let mutation = DocumentMutation::Incremental(bytes);
            let expected = walked(&mutation, record).map(|history| history.save());
            let built = of(key(&mutation), &mutation, record).map(|history| history.save());
            assert_eq!(built, expected, "seed {seed} window {index}");
        }
    }
    assert!(extended_any);
}

#[test]
fn a_dependency_the_record_lacks_is_refused_as_the_walk_refuses_it() {
    let mut admitted = record(7, 3, 40);
    let mutation = publications(&mut admitted, 7).swap_remove(0);
    // A record without the publication's history at all.
    let mut other = AutoCommit::new();
    other.put(ROOT, "unrelated", true).unwrap();
    other.commit();
    let other = other.committed_document();
    assert_eq!(
        build(&mutation, other).map(|history| history.save()),
        walked(&mutation, other).map(|history| history.save()),
    );
    assert!(build(&mutation, other).is_err());
}

#[test]
fn a_remembered_history_is_reused_only_while_the_record_holds_it() {
    let mut admitted = record(11, 2, 25);
    let mutation = publications(&mut admitted, 11).swap_remove(1);
    let key = key(&mutation);
    let record = admitted.committed_document();
    let first = of(key, &mutation, record).unwrap();
    let again = of(key, &mutation, record).unwrap();
    assert!(Arc::ptr_eq(&first, &again), "built once");
    let mut other = AutoCommit::new();
    other.put(ROOT, "unrelated", true).unwrap();
    other.commit();
    assert!(
        of(key, &mutation, other.committed_document()).is_err(),
        "a record without its changes rebuilds, and refuses"
    );
}
