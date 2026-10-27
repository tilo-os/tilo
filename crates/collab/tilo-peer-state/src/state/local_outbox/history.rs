//! The history a recovery re-sends for one retained publication, and what
//! re-sending it costs, each built once.
//!
//! A publication's history is the publication applied with every change it
//! depends on, taken from the admitted record: the walk loads the
//! publication, then fetches all of its missing dependencies at once and
//! applies them, and again, until nothing is missing. The document that walk
//! builds is the recovery's payload, so its every byte is reserved and later
//! compared: [`build`] must build exactly that document, change for change
//! and in the same order.
//!
//! The walk as written cost the record's size once per change in its
//! history: each dependency was rebuilt from the record's op set
//! (`get_change_by_hash`), and each round walked and re-sorted everything the
//! history queued so far. Every committed edit of a shared document ran it
//! for each publication the governor had not yet acknowledged, so an edit of
//! a long document cost seconds. [`build`] takes the record's changes in one
//! pass, finds each round's missing dependencies among the changes that round
//! fetched (none of the earlier ones can name a new one), and hands the
//! history a round's changes only when one of them can apply — which is when
//! the walk applied anything; a round before that only queued. The history
//! therefore receives the same changes in the same order and builds the same
//! document, for the record's size once.
//!
//! Even once, that is the whole record per edit. A device's next edit
//! usually depends on everything its previous edit's history holds, and the
//! walk then applies that history's changes first, in the same order, and the
//! edit's few new changes after them. [`extended`] works out from the
//! changes' metadata alone the order in which the walk would apply them, and
//! when a remembered history is exactly the start of that order, copies it and
//! applies the rest: the same changes in the same order again, for the size of
//! the edit (and a copy).
//!
//! Both a history and its reservation depend only on the publication's bytes
//! and on changes named by their hashes, so they are remembered by those
//! bytes; a remembered history is reused only while the admitted record still
//! holds its heads, and with them every change it contains.
use super::err;
use automerge::{Automerge, Change, ChangeHash, ReadDoc};
use collab_types::DocumentMutation;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::Mutex;
#[cfg(not(test))]
use tilo_lock::MutexRecover as _;

/// Histories kept: one per publication the governor has not yet
/// acknowledged, for a few records at a time.
const REMEMBERED_HISTORIES: usize = 32;
/// Their combined size in ops, and that of the merges: each is a whole copy
/// of its record.
const REMEMBERED_OPS: u64 = 2_000_000;
/// Merged histories kept, one per list of publications merged together.
const REMEMBERED_MERGES: usize = 8;
/// Reservations kept, one per set of publications reserved together.
const REMEMBERED_RESERVATIONS: usize = 64;

/// A publication's identity: the exact bytes its history is built from.
pub(super) type Key = [u8; 32];

pub(super) fn key(mutation: &DocumentMutation) -> Key {
    let mut hasher = blake3::Hasher::new_derive_key("tilo retained publication history v1");
    let (tag, bytes) = match mutation {
        DocumentMutation::Snapshot(bytes) => (0_u8, bytes),
        DocumentMutation::Incremental(bytes) => (1_u8, bytes),
    };
    hasher.update(&[tag]);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

/// The history of the publication `mutation` with its dependencies from
/// `admitted`, remembered by `key`.
///
/// # Errors
/// The publication does not load, or the admitted record lacks one of its
/// dependencies or one of its changes.
pub(super) fn of(
    key: Key,
    mutation: &DocumentMutation,
    admitted: &Automerge,
) -> Result<Arc<Automerge>, String> {
    if let Some(history) = remembered(key, admitted) {
        return Ok(history);
    }
    let history = if let Some(history) = extended(mutation, admitted) {
        history
    } else {
        #[cfg(test)]
        REBUILDS.with(|count| count.set(count.get().saturating_add(1)));
        build(mutation, admitted)?
    };
    let history = Arc::new(history);
    #[cfg(test)]
    BUILDS.with(|count| count.set(count.get().saturating_add(1)));
    remember(key, &history);
    Ok(history)
}

#[cfg(test)]
thread_local! {
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many histories this thread has built rather than reused.
#[cfg(test)]
pub(super) fn builds() -> usize {
    BUILDS.with(std::cell::Cell::get)
}

/// How many of those it built from nothing rather than extending another.
#[cfg(test)]
pub(super) fn rebuilds() -> usize {
    REBUILDS.with(std::cell::Cell::get)
}

/// The history [`build`] builds for `mutation`, made from a remembered
/// history that is exactly the start of it; `None` when there is none, or
/// when the publication's changes cannot be listed.
///
/// The order in which a history receives its changes is worked out from the
/// changes' dependencies alone ([`applied_order`]); the history is then a
/// copy of the remembered one with the rest applied in that order, one at a
/// time, so each applies as it arrives.
pub(super) fn extended(mutation: &DocumentMutation, admitted: &Automerge) -> Option<Automerge> {
    let DocumentMutation::Incremental(bytes) = mutation else {
        return None;
    };
    let own = Automerge::parse_changes(bytes).ok()?;
    let order = applied_order(&own, admitted)?;
    let base = remembered_prefix(&order)?;
    let mut history = Automerge::clone(&base);
    let suffix = order.get(base.change_hashes().len()..)?;
    let wanted = suffix.iter().copied().collect::<HashSet<_>>();
    let mut changes = admitted
        .get_changes(&base.get_heads())
        .into_iter()
        .filter(|change| wanted.contains(&change.hash()))
        .map(|change| (change.hash(), change))
        .collect::<HashMap<_, _>>();
    changes.extend(own.into_iter().map(|change| (change.hash(), change)));
    for hash in suffix {
        history.apply_changes([changes.remove(hash)?]).ok()?;
    }
    (history.change_hashes() == order.as_slice()
        && history
            .get_heads()
            .iter()
            .all(|head| admitted.get_change_meta_by_hash(head).is_some()))
    .then_some(history)
}

/// The order in which the walk's history applies the publication's changes
/// `own` (in the order of its bytes) and their dependencies from `admitted`:
/// the walk replayed on dependencies alone.
///
/// Every call that hands the history changes queues them behind those
/// already waiting, and the history then applies what can apply as
/// `ChangeQueue::pop_topo_sorted_ready` takes it: Kahn's algorithm over the
/// queue in its order, a change released when the last of its dependencies
/// applies, released changes taken first in, first out, the rest left
/// queued in their order. The first call is the publication's own load; each
/// later one hands over a round of missing dependencies. A call applies
/// anything only when one of the changes it hands over can apply, since what
/// waited before could not and nothing has applied since. `None` when a
/// dependency is not admitted, or a change would stay queued.
fn applied_order(own: &[Change], admitted: &Automerge) -> Option<Vec<ChangeHash>> {
    let mut deps = HashMap::<ChangeHash, Vec<ChangeHash>>::new();
    let mut round = Vec::new();
    for change in own {
        if deps.insert(change.hash(), change.deps().to_vec()).is_none() {
            round.push(change.hash());
        }
    }
    let mut known = round.iter().copied().collect::<HashSet<_>>();
    let mut queue = round.clone();
    let mut applied = HashSet::new();
    let mut order = Vec::new();
    loop {
        if round.iter().any(|hash| {
            deps.get(hash)
                .is_some_and(|change_deps| change_deps.iter().all(|dep| applied.contains(dep)))
        }) {
            pop_ready(&mut queue, &deps, &mut applied, &mut order)?;
        }
        let mut next = round
            .iter()
            .filter_map(|hash| deps.get(hash))
            .flatten()
            .filter(|dep| !known.contains(*dep))
            .copied()
            .collect::<Vec<_>>();
        next.sort_unstable();
        next.dedup();
        if next.is_empty() {
            break;
        }
        for hash in &next {
            let meta = admitted.get_change_meta_by_hash(hash)?;
            deps.insert(*hash, meta.deps);
            known.insert(*hash);
            queue.push(*hash);
        }
        round = next;
    }
    queue.is_empty().then_some(order)
}

/// `ChangeQueue::pop_topo_sorted_ready` on hashes: apply from `queue` what
/// can apply, in its order, onto `applied` and `order`.
fn pop_ready(
    queue: &mut Vec<ChangeHash>,
    deps: &HashMap<ChangeHash, Vec<ChangeHash>>,
    applied: &mut HashSet<ChangeHash>,
    order: &mut Vec<ChangeHash>,
) -> Option<()> {
    let mut unsatisfied = Vec::with_capacity(queue.len());
    let mut waiting = HashMap::<ChangeHash, Vec<usize>>::new();
    for (index, hash) in queue.iter().enumerate() {
        let mut count = 0_usize;
        for dep in deps.get(hash)? {
            if !applied.contains(dep) {
                count = count.saturating_add(1);
                waiting.entry(*dep).or_default().push(index);
            }
        }
        unsatisfied.push(count);
    }
    let mut ready = unsatisfied
        .iter()
        .enumerate()
        .filter(|(_, count)| **count == 0)
        .map(|(index, _)| index)
        .collect::<VecDeque<_>>();
    let mut taken = vec![false; queue.len()];
    while let Some(index) = ready.pop_front() {
        let hash = *queue.get(index)?;
        *taken.get_mut(index)? = true;
        order.push(hash);
        applied.insert(hash);
        for dependent in waiting.remove(&hash).unwrap_or_default() {
            let count = unsatisfied.get_mut(dependent)?;
            *count = count.checked_sub(1)?;
            if *count == 0 {
                ready.push_back(dependent);
            }
        }
    }
    let mut taken = taken.into_iter();
    queue.retain(|_| !taken.next().unwrap_or(false));
    Some(())
}

/// Forget what this thread remembers, so the next answer is computed anew.
#[cfg(test)]
pub(super) fn forget() {
    HISTORIES.with(|histories| histories.borrow_mut().clear());
    MERGES.with(|merges| merges.borrow_mut().clear());
    RESERVATIONS.with(|reservations| reservations.borrow_mut().clear());
}

/// The publication `mutation` and its dependencies from `admitted`, as the
/// dependency walk builds them (see the module documentation).
///
/// # Errors
/// As [`of`].
pub(super) fn build(
    mutation: &DocumentMutation,
    admitted: &Automerge,
) -> Result<Automerge, String> {
    let mut history = match mutation {
        DocumentMutation::Snapshot(bytes) => Automerge::load(bytes).map_err(err)?,
        DocumentMutation::Incremental(bytes) => {
            let mut history = Automerge::new();
            history.load_incremental(bytes).map_err(err)?;
            history
        }
    };
    let missing = history.get_missing_deps(&[]);
    if !missing.is_empty() {
        // The publication's own changes, in the history or queued in it.
        let own = match mutation {
            DocumentMutation::Incremental(bytes) => Automerge::parse_changes(bytes).ok(),
            DocumentMutation::Snapshot(_) => None,
        };
        let mut changes = admitted
            .get_changes(&[])
            .into_iter()
            .map(|change| (change.hash(), change))
            .collect::<HashMap<_, _>>();
        match own {
            Some(own)
                if !own
                    .iter()
                    .any(|change| missing.binary_search(&change.hash()).is_ok()) =>
            {
                walk(&mut history, missing, &own, &mut changes)?;
            }
            // Not a plain list of changes the history holds: walk as written.
            _ => walk_queue(&mut history, missing, &changes)?,
        }
    }
    if !history
        .get_heads()
        .iter()
        .all(|head| admitted.get_change_meta_by_hash(head).is_some())
    {
        return Err("retained publication changes are not in admitted history".into());
    }
    Ok(history)
}

/// The walk, with each round's missing dependencies found among the changes
/// the round before fetched, and fetched changes handed to `history` only
/// once one of them can apply.
fn walk(
    history: &mut Automerge,
    mut missing: Vec<ChangeHash>,
    own: &[Change],
    changes: &mut HashMap<ChangeHash, Change>,
) -> Result<(), String> {
    // Everything the history holds or queues; nothing else can be missing.
    let mut known = history
        .change_hashes()
        .iter()
        .copied()
        .chain(own.iter().map(Change::hash))
        .collect::<HashSet<_>>();
    let mut held = Vec::new();
    while !missing.is_empty() {
        let round = fetch(&missing, changes)?;
        // A queued change applies only once all of its dependencies are in
        // the history; until one of this round's does, the history changes
        // no differently than by queueing.
        let applies = round.iter().any(|change| {
            change
                .deps()
                .iter()
                .all(|dep| history.get_change_meta_by_hash(dep).is_some())
        });
        known.extend(round.iter().map(Change::hash));
        let mut next = round
            .iter()
            .flat_map(|change| change.deps().iter().copied())
            .filter(|dep| !known.contains(dep))
            .collect::<Vec<_>>();
        next.sort_unstable();
        next.dedup();
        held.extend(round);
        if applies {
            history
                .apply_changes(std::mem::take(&mut held))
                .map_err(err)?;
        }
        missing = next;
    }
    if !held.is_empty() {
        history.apply_changes(held).map_err(err)?;
    }
    Ok(())
}

/// The walk as written, for a publication whose changes cannot be listed:
/// each round asks the history what it misses.
fn walk_queue(
    history: &mut Automerge,
    mut missing: Vec<ChangeHash>,
    changes: &HashMap<ChangeHash, Change>,
) -> Result<(), String> {
    while !missing.is_empty() {
        let dependencies = missing
            .iter()
            .map(|hash| {
                changes
                    .get(hash)
                    .cloned()
                    .ok_or("retained publication dependency is not in admitted history")
            })
            .collect::<Result<Vec<_>, _>>()?;
        history.apply_changes(dependencies).map_err(err)?;
        let next = history.get_missing_deps(&[]);
        if next == missing {
            return Err("retained publication dependencies did not advance".into());
        }
        missing = next;
    }
    Ok(())
}

fn fetch(
    missing: &[ChangeHash],
    changes: &mut HashMap<ChangeHash, Change>,
) -> Result<Vec<Change>, String> {
    missing
        .iter()
        .map(|hash| {
            changes
                .remove(hash)
                .ok_or_else(|| "retained publication dependency is not in admitted history".into())
        })
        .collect()
}

struct Remembered {
    key: Key,
    history: Arc<Automerge>,
    ops: u64,
}

/// Runs `with` on what this process remembers. Tests run as parallel
/// threads of one process, so there each thread keeps its own: one test's
/// histories cannot evict another's, and a test counts only its own builds.
fn remembered_in<T: 'static, R>(
    #[cfg(not(test))] store: &'static Mutex<VecDeque<T>>,
    #[cfg(test)] store: &'static std::thread::LocalKey<std::cell::RefCell<VecDeque<T>>>,
    with: impl FnOnce(&mut VecDeque<T>) -> R,
) -> R {
    #[cfg(not(test))]
    {
        with(&mut store.lock_recover())
    }
    #[cfg(test)]
    {
        store.with(|entries| with(&mut entries.borrow_mut()))
    }
}

#[cfg(not(test))]
static HISTORIES: Mutex<VecDeque<Remembered>> = Mutex::new(VecDeque::new());
#[cfg(not(test))]
static MERGES: Mutex<VecDeque<Merged>> = Mutex::new(VecDeque::new());
#[cfg(not(test))]
static RESERVATIONS: Mutex<VecDeque<(Key, usize)>> = Mutex::new(VecDeque::new());
#[cfg(test)]
thread_local! {
    static HISTORIES: std::cell::RefCell<VecDeque<Remembered>> =
        const { std::cell::RefCell::new(VecDeque::new()) };
    static MERGES: std::cell::RefCell<VecDeque<Merged>> =
        const { std::cell::RefCell::new(VecDeque::new()) };
    static RESERVATIONS: std::cell::RefCell<VecDeque<(Key, usize)>> =
        const { std::cell::RefCell::new(VecDeque::new()) };
}

/// The histories of several publications on one route merged, by the
/// publications in merge order.
struct Merged {
    parts: Vec<Key>,
    history: Arc<Automerge>,
    ops: u64,
}

/// The histories `parts` merged in order: the first, then each next one's
/// changes it lacks, as `Automerge::merge` takes them. With `keys`, the
/// publications of `parts`, a merge of all but the last few is remembered:
/// the next edit adds one publication, so its merge is that one's changes.
pub(super) fn merged(
    keys: Option<&[Key]>,
    parts: &[Arc<Automerge>],
) -> Result<Arc<Automerge>, String> {
    let first = parts.first().ok_or("recovery history has no part")?;
    let (done, mut merged) = keys
        .and_then(remembered_merge)
        .unwrap_or_else(|| (1, Arc::clone(first)));
    for part in parts.iter().skip(done) {
        let doc = Arc::make_mut(&mut merged);
        let added = doc.get_changes_added(part);
        doc.apply_changes(added).map_err(err)?;
    }
    if let Some(keys) = keys
        && keys.len() > 1
        && keys.len() == parts.len()
    {
        remember_merge(keys, &merged);
    }
    Ok(merged)
}

/// The remembered merge of the longest start of `keys`, and its length.
fn remembered_merge(keys: &[Key]) -> Option<(usize, Arc<Automerge>)> {
    remembered_in(&MERGES, |merges| {
        merges
            .iter()
            .filter(|entry| keys.starts_with(&entry.parts))
            .max_by_key(|entry| entry.parts.len())
            .map(|entry| (entry.parts.len(), Arc::clone(&entry.history)))
    })
}

fn remember_merge(keys: &[Key], history: &Arc<Automerge>) {
    let ops = history.stats().num_ops;
    if ops > REMEMBERED_OPS {
        return;
    }
    remembered_in(&MERGES, |merges| {
        merges.retain(|entry| entry.parts != keys);
        merges.push_back(Merged {
            parts: keys.to_vec(),
            history: Arc::clone(history),
            ops,
        });
        let mut total = merges.iter().map(|entry| entry.ops).sum::<u64>();
        while merges.len() > REMEMBERED_MERGES || total > REMEMBERED_OPS {
            let Some(oldest) = merges.pop_front() else {
                break;
            };
            total = total.saturating_sub(oldest.ops);
        }
    });
}

fn remembered(key: Key, admitted: &Automerge) -> Option<Arc<Automerge>> {
    let history = remembered_in(&HISTORIES, |histories| {
        let index = histories.iter().position(|entry| entry.key == key)?;
        let entry = histories.remove(index)?;
        let history = Arc::clone(&entry.history);
        histories.push_back(entry);
        Some(history)
    })?;
    // The record holds the heads, so every change of the history: the walk
    // would fetch exactly these changes again.
    history
        .get_heads()
        .iter()
        .all(|head| admitted.get_change_meta_by_hash(head).is_some())
        .then_some(history)
}

/// The longest remembered history whose changes, in order, start `order`.
fn remembered_prefix(order: &[ChangeHash]) -> Option<Arc<Automerge>> {
    remembered_in(&HISTORIES, |histories| {
        histories
            .iter()
            .filter(|entry| {
                !entry.history.change_hashes().is_empty()
                    && order.starts_with(entry.history.change_hashes())
            })
            .max_by_key(|entry| entry.history.change_hashes().len())
            .map(|entry| Arc::clone(&entry.history))
    })
}

fn remember(key: Key, history: &Arc<Automerge>) {
    let ops = history.stats().num_ops;
    if ops > REMEMBERED_OPS {
        return;
    }
    remembered_in(&HISTORIES, |histories| {
        histories.retain(|entry| entry.key != key);
        histories.push_back(Remembered {
            key,
            history: Arc::clone(history),
            ops,
        });
        let mut total = histories.iter().map(|entry| entry.ops).sum::<u64>();
        while histories.len() > REMEMBERED_HISTORIES || total > REMEMBERED_OPS {
            let Some(oldest) = histories.pop_front() else {
                break;
            };
            total = total.saturating_sub(oldest.ops);
        }
    });
}

/// The identity of a route's reservation: everything its encoded size
/// depends on besides the bytes of the histories, which `parts` name.
pub(super) fn reservation_key(
    room: &str,
    app: &str,
    record: &str,
    selected: [u8; 32],
    parts: &[Key],
) -> Key {
    let mut hasher = blake3::Hasher::new_derive_key("tilo recovery reservation v1");
    for text in [room, app, record] {
        hasher.update(&(text.len() as u64).to_le_bytes());
        hasher.update(text.as_bytes());
    }
    hasher.update(&selected);
    for part in parts {
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

pub(super) fn remembered_reservation(key: Key) -> Option<usize> {
    remembered_in(&RESERVATIONS, |reservations| {
        reservations
            .iter()
            .find(|(entry, _)| *entry == key)
            .map(|(_, bytes)| *bytes)
    })
}

pub(super) fn remember_reservation(key: Key, bytes: usize) {
    remembered_in(&RESERVATIONS, |reservations| {
        reservations.retain(|(entry, _)| *entry != key);
        reservations.push_back((key, bytes));
        while reservations.len() > REMEMBERED_RESERVATIONS {
            reservations.pop_front();
        }
    });
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
