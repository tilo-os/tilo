//! A member admitted later must not receive what was withdrawn before, and
//! an Automerge change keeps every value it ever wrote. So a workspace document
//! starts over at each admission from a base, its state in one change with no
//! history; members adopt it and build on it, and keep what came before.
use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, ChangeHash, ROOT, ReadDoc, ScalarValue, Value};

/// The root key holding a document's base generation; a document that never
/// had a base reads as generation `0`.
pub const BASE_KEY: &str = "base_v1";

/// `doc`'s base generation.
#[must_use]
pub fn generation(doc: &AutoCommit) -> u64 {
    match doc.get(ROOT, BASE_KEY) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Uint(generation) => *generation,
            ScalarValue::Int(generation) => u64::try_from(*generation).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    }
}

/// The root key of the map a base keeps of the changes it holds: each
/// actor's highest sequence number in the document it was made from. A
/// device adopting the base reads there which of its own changes the base
/// does not hold ([`carried`]).
pub const COVERS_KEY: &str = "base_covers_v1";

/// The root key of the governance heads a base replaced: the first bytes of
/// the hash of every change the history before it held
/// ([`collab_types::governance_head`]), those of the bases before it included,
/// sorted, one after the other. A write names the governance it was written
/// under by those bytes; one naming a head a base replaced was written before
/// that base ([`replaced`]).
pub const REPLACED_KEY: &str = "base_replaced_v1";

/// The governance heads the bases `doc` was built on replaced
/// ([`REPLACED_KEY`]): none for a document that never had a base, or whose
/// base was made before bases said.
#[must_use]
pub fn replaced(doc: &AutoCommit) -> std::collections::BTreeSet<[u8; 4]> {
    match doc.get(ROOT, REPLACED_KEY) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => bytes
                .chunks_exact(4)
                .filter_map(|head| head.try_into().ok())
                .collect(),
            _ => std::collections::BTreeSet::new(),
        },
        _ => std::collections::BTreeSet::new(),
    }
}

/// The root key of the list a base keeps of the named changes the history
/// before it held ([`KeptChange`]): every change whose message names the
/// action it carries out, in the order it was made, those the bases before it
/// kept first. A base drops a history's values, which may hold what a later
/// member must not receive, but never which action was taken, by whom and
/// when: replay order decides the workspace's administration, and its members
/// and history read every one of them ([`named_changes`]).
pub const KEPT_KEY: &str = "base_kept_v1";

/// One named change of a workspace's governance as a reader lists it: the
/// change's hash, the message naming its action, and the time its commit
/// carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptChange {
    pub change: ChangeHash,
    pub message: String,
    pub time: i64,
}

/// The named changes `doc`'s bases kept ([`KEPT_KEY`]), oldest first.
fn kept(doc: &AutoCommit) -> Vec<KeptChange> {
    let Ok(Some((Value::Object(automerge::ObjType::List), list))) = doc.get(ROOT, KEPT_KEY) else {
        return Vec::new();
    };
    let text = |entry: &automerge::ObjId, key: &str| match doc.get(entry, key) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    };
    (0..doc.length(&list))
        .filter_map(|index| {
            let Ok(Some((Value::Object(_), entry))) = doc.get(&list, index) else {
                return None;
            };
            let change = text(&entry, "change")?.parse::<ChangeHash>().ok()?;
            let message = text(&entry, "message")?;
            let time = match doc.get(&entry, "time") {
                Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                    ScalarValue::Int(time) => *time,
                    _ => 0,
                },
                _ => 0,
            };
            Some(KeptChange {
                change,
                message,
                time,
            })
        })
        .collect()
}

/// Every named change of `doc`'s governance, oldest first: those its bases
/// kept, then each change of its history since whose message names one. A
/// rebase changes none of them.
#[must_use]
pub fn named_changes(doc: &AutoCommit) -> Vec<KeptChange> {
    let mut named = kept(doc);
    let mut seen: std::collections::HashSet<ChangeHash> =
        named.iter().map(|kept| kept.change).collect();
    for change in doc.clone().get_changes(&[]) {
        let Some(message) = change
            .message()
            .filter(|message| collab_types::Stamp::of_message(message).is_some())
        else {
            continue;
        };
        if seen.insert(change.hash()) {
            named.push(KeptChange {
                change: change.hash(),
                message: message.to_owned(),
                time: change.timestamp(),
            });
        }
    }
    named
}

/// A document holding `doc`'s state as it stands, in one change by `actor`
/// with no dependencies, at the next base generation. What the history held
/// is gone from it, but not which actions it took ([`KEPT_KEY`]).
pub fn rebase(doc: &AutoCommit, actor: ActorId) -> Result<AutoCommit, automerge::AutomergeError> {
    let next = generation(doc).saturating_add(1);
    let named = named_changes(doc);
    let automerge::hydrate::Value::Map(mut state) = doc.hydrate(ROOT, None)? else {
        return Err(automerge::AutomergeError::InvalidOp(
            automerge::ObjType::Map,
        ));
    };
    state.remove(KEPT_KEY);
    // The signature of the base this one replaces is of that base alone.
    state.remove(super::compaction::COMPACTION_KEY);
    let mut covered = std::collections::BTreeMap::<String, u64>::new();
    let mut heads = replaced(doc);
    for change in doc.clone().get_changes(&[]) {
        heads.insert(collab_types::governance_head(&change.hash().0));
        let seq = covered
            .entry(change.actor_id().to_hex_string())
            .or_default();
        *seq = (*seq).max(change.seq());
    }
    let mut base = AutoCommit::new().with_actor(actor);
    base.init_root_from_hydrate(&state)?;
    base.put(ROOT, BASE_KEY, ScalarValue::Uint(next))?;
    let covers = base.put_object(ROOT, COVERS_KEY, automerge::ObjType::Map)?;
    for (actor, seq) in covered {
        base.put(&covers, actor, ScalarValue::Uint(seq))?;
    }
    base.put(
        ROOT,
        REPLACED_KEY,
        ScalarValue::Bytes(heads.into_iter().flatten().collect()),
    )?;
    if !named.is_empty() {
        let list = base.put_object(ROOT, KEPT_KEY, automerge::ObjType::List)?;
        for (index, kept) in named.into_iter().enumerate() {
            let entry = base.insert_object(&list, index, automerge::ObjType::Map)?;
            base.put(&entry, "change", kept.change.to_string())?;
            base.put(&entry, "message", kept.message)?;
            base.put(&entry, "time", ScalarValue::Int(kept.time))?;
        }
    }
    base.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    Ok(base)
}

/// The hash of `doc`'s base change, when it has a base.
#[must_use]
pub fn root(doc: &mut AutoCommit) -> Option<ChangeHash> {
    if generation(doc) == 0 {
        return None;
    }
    doc.get_changes(&[])
        .into_iter()
        .find(|change| change.deps().is_empty())
        .map(|change| change.hash())
}

/// Carry onto `adopted`, a newer base, the changes `held_as` made in `before`
/// that the base does not hold, as `actor`'s own change: what a device wrote
/// against the history the base replaced is written again on the base, not
/// dropped with that history. `held_as` is the actor the device's changes have in the history
/// the base replaced—each generation of a history numbers a writer's changes under an actor of
/// its own—and `actor` is the actor of the base's generation, which the change written here
/// takes. The base names what it holds ([`COVERS_KEY`]); the difference `held_as`'s other changes
/// made is taken from `before` itself, between its state without them and with them, so whatever
/// the base's maker wrote meanwhile stays as the base holds it. Returns whether anything was
/// carried.
///
/// # Errors
/// `before` could not be read at the heads it is compared at, or `adopted`
/// could not be written.
pub fn carried(
    before: &AutoCommit,
    adopted: &mut AutoCommit,
    (held_as, actor): (&ActorId, &ActorId),
) -> Result<bool, automerge::AutomergeError> {
    let Some(covered) = covered_seq(adopted, held_as) else {
        // A base that names nothing it holds was made before bases said:
        // there is no telling which of its changes it holds.
        return Ok(false);
    };
    let mut before = before.clone();
    let changes = before.get_changes(&[]);
    let uncovered: std::collections::HashSet<ChangeHash> = changes
        .iter()
        .filter(|change| change.actor_id() == held_as && change.seq() > covered)
        .map(automerge::Change::hash)
        .collect();
    if uncovered.is_empty() {
        return Ok(false);
    }
    // Another's change built on one of `held_as`'s uncovered changes is not
    // their author's to write again: the state compared against leaves it out too.
    let mut excluded = uncovered.clone();
    loop {
        let grown = changes.iter().filter(|change| {
            !excluded.contains(&change.hash())
                && change.deps().iter().any(|dep| excluded.contains(dep))
        });
        let grown: Vec<ChangeHash> = grown.map(automerge::Change::hash).collect();
        if grown.is_empty() {
            break;
        }
        excluded.extend(grown);
    }
    let without: Vec<&automerge::Change> = changes
        .iter()
        .filter(|change| !excluded.contains(&change.hash()))
        .collect();
    let with: Vec<&automerge::Change> = changes
        .iter()
        .filter(|change| !excluded.contains(&change.hash()) || uncovered.contains(&change.hash()))
        .collect();
    let state_at = |doc: &mut AutoCommit, at: &[&automerge::Change]| {
        let heads = heads_of(at);
        let forked = doc.fork_at(&heads)?;
        match forked.hydrate(ROOT, None)? {
            automerge::hydrate::Value::Map(map) => Ok(map),
            _ => Err(automerge::AutomergeError::InvalidOp(
                automerge::ObjType::Map,
            )),
        }
    };
    let was = state_at(&mut before, &without)?;
    let is = state_at(&mut before, &with)?;
    adopted.set_actor(actor.clone());
    let wrote = carry_map(adopted, &ROOT, &was, &is, true)?;
    if wrote {
        adopted.commit();
    }
    Ok(wrote)
}

/// `actor`'s highest sequence number `base` says it holds, `0` for none;
/// `None` for a base that says nothing of what it holds.
fn covered_seq(base: &AutoCommit, actor: &ActorId) -> Option<u64> {
    let Ok(Some((Value::Object(automerge::ObjType::Map), covers))) = base.get(ROOT, COVERS_KEY)
    else {
        return None;
    };
    Some(match base.get(&covers, actor.to_hex_string()) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Uint(seq) => *seq,
            ScalarValue::Int(seq) => u64::try_from(*seq).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    })
}

/// Whether `key` of a document's root is its base's own: what the base says of
/// itself, never a write to carry onto another.
fn is_base_key(key: &str) -> bool {
    key == BASE_KEY
        || key == COVERS_KEY
        || key == REPLACED_KEY
        || key == super::compaction::COMPACTION_KEY
}

/// The changes of `set` no other change of it builds on.
fn heads_of(set: &[&automerge::Change]) -> Vec<ChangeHash> {
    let deps: std::collections::HashSet<&ChangeHash> =
        set.iter().flat_map(|change| change.deps()).collect();
    set.iter()
        .map(|change| change.hash())
        .filter(|hash| !deps.contains(hash))
        .collect()
}

/// Write into `obj` of `doc` what changed from `was` to `is`, key by key:
/// a key both hold as maps is walked into, any other changed value is
/// written whole, and a key `is` no longer holds is deleted. The base's own
/// keys are the base's.
fn carry_map(
    doc: &mut AutoCommit,
    obj: &automerge::ObjId,
    was: &automerge::hydrate::Map,
    is: &automerge::hydrate::Map,
    root: bool,
) -> Result<bool, automerge::AutomergeError> {
    let mut wrote = false;
    for (key, value) in is.iter() {
        if root && is_base_key(key) {
            continue;
        }
        let value = &value.value;
        match (was.get(key), value) {
            (Some(prior), _) if prior == value => {}
            (Some(automerge::hydrate::Value::Map(prior)), automerge::hydrate::Value::Map(now)) => {
                if let Some((Value::Object(automerge::ObjType::Map), inner)) = doc.get(obj, key)? {
                    wrote |= carry_map(doc, &inner, prior, now, false)?;
                } else {
                    doc.batch_create_object(obj, key.as_str(), value, false)?;
                    wrote = true;
                }
            }
            (_, automerge::hydrate::Value::Scalar(scalar)) => {
                doc.put(obj, key.as_str(), scalar.clone())?;
                wrote = true;
            }
            _ => {
                doc.batch_create_object(obj, key.as_str(), value, false)?;
                wrote = true;
            }
        }
    }
    for (key, _) in was.iter() {
        if root && is_base_key(key) {
            continue;
        }
        if is.get(key).is_none() && doc.get(obj, key)?.is_some() {
            doc.delete(obj, key.as_str())?;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// How a batch relates to the base of the document it is admitted to.
pub(crate) enum Lineage {
    /// It builds on the receiver's base, or neither has one: `changes` are
    /// admitted onto the document as it is, once the changes waiting in it
    /// that `replaced` names are dropped.
    Same {
        changes: Vec<Vec<u8>>,
        replaced: Vec<ChangeHash>,
    },
    /// It carries a newer base, which replaces the receiver's document: the
    /// document the base builds, and the changes of the batch beside the base
    /// not shown to belong to another history. The base is built once, here,
    /// where its generation is read: a newcomer's base holds the whole
    /// workspace, and building it a second time to admit it was a second load
    /// of everything a workspace of hundreds contains.
    Newer {
        base: Box<AutoCommit>,
        changes: Vec<Vec<u8>>,
    },
    /// It starts a history of another founding of the workspace: a document
    /// some device founded afresh under the workspace's id, naming a founder
    /// other than the one the receiver's document names. A workspace is
    /// founded once, and what that founding holds is never replaced by
    /// another's, whatever its generation, nor merged beside it as a second
    /// root whose rows would compete with the held ones.
    Refounded,
}

/// Who `doc` names as the workspace's founder ([`crate::state::seed::FOUNDER`]):
/// the founding device, or the person a workspace of their own is founded by.
/// Every base carries it on from the state it was made of.
fn founder(doc: &AutoCommit) -> Option<String> {
    match doc.get(ROOT, crate::state::seed::FOUNDER) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(founder) => Some(founder.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Sort `changes` against `before`. A change with no dependencies starts a
/// history: the base of a newer generation replaces `before`; any other is
/// dropped once `before` has a base, since applied beside it, an older
/// history would merge back what the base left behind.
///
/// Each generation of a workspace's history numbers its writers under actors of its own
/// ([`crate::workspace_host_actor_at`]), so a change of the history a base replaced carries
/// different actors than the current history's and is never taken for one of the same number. A
/// waiting change is dropped only where it is shown to belong to another history:
///
/// - it starts another history, or builds on a change that does;
/// - or it waits for a change the base replaced ([`REPLACED_KEY`]): that change never comes, as
///   the base holds what it held.
///
/// This applies to the batch and to the changes already waiting in
/// `before`, and to what builds on such a change.
///
/// A document that never had a base takes the batch as it comes, unless the
/// batch starts a history of another founding: then it is
/// [`Lineage::Refounded`], however new its base.
pub(crate) fn lineage(before: Option<&AutoCommit>, changes: &[Vec<u8>]) -> Lineage {
    let held = before.map_or(0, generation);
    let founded = before.and_then(founder);
    let decoded: Vec<Option<automerge::Change>> = changes
        .iter()
        .map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
        .collect();
    let mut newest: Option<(u64, usize, AutoCommit)> = None;
    for (index, (change, bytes)) in decoded.iter().zip(changes).enumerate() {
        let Some(change) = change else {
            continue;
        };
        if !change.deps().is_empty() {
            continue;
        }
        let Ok(started) = super::candidate(None, std::slice::from_ref(bytes)) else {
            continue;
        };
        if founded.is_some()
            && founder(&started) != founded
            && !applied(before.map(AutoCommit::committed_document), &change.hash())
        {
            return Lineage::Refounded;
        }
        let at = generation(&started);
        if at > held
            && newest
                .as_ref()
                .is_none_or(|(generation, ..)| at > *generation)
        {
            newest = Some((at, index, started));
        }
    }
    if let Some((_, base, built)) = newest {
        let (kept, _) = sorted(changes, &decoded, Some(base), None, || replaced(&built));
        let base_bytes = changes.get(base);
        return Lineage::Newer {
            base: Box::new(built),
            changes: kept
                .into_iter()
                .filter(|bytes| Some(bytes) != base_bytes)
                .collect(),
        };
    }
    let Some(before) = before.filter(|_| held > 0) else {
        return Lineage::Same {
            changes: changes.to_vec(),
            replaced: Vec::new(),
        };
    };
    let (changes, replaced) = sorted(
        changes,
        &decoded,
        None,
        Some(before.committed_document()),
        || replaced(before),
    );
    Lineage::Same { changes, replaced }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// Built on the base: on `base`, or on what `before` holds applied.
    Built,
    /// Not shown to belong to either history yet: it waits.
    Waiting,
    /// Shown to belong to another history.
    Other,
}

/// The changes of `changes` to admit, each in the order it came, and the
/// changes waiting in `before` to drop first. The base is `changes[base]`
/// when the batch carries a newer one, else the base `before` holds;
/// `replaced` answers the heads that base replaced ([`REPLACED_KEY`]), read
/// only when a change waits. A change that does not decode is kept, for the
/// admission to refuse as malformed.
fn sorted(
    changes: &[Vec<u8>],
    decoded: &[Option<automerge::Change>],
    base: Option<usize>,
    before: Option<&automerge::Automerge>,
    replaced: impl FnOnce() -> std::collections::BTreeSet<[u8; 4]>,
) -> (Vec<Vec<u8>>, Vec<ChangeHash>) {
    let mut sort = started(decoded, base, before);
    build_on_base(decoded, &mut sort, before);
    let queued: Vec<&automerge::Change> = before
        .map(|doc| doc.queued_changes().collect())
        .unwrap_or_default();
    let mut other = Other::default();
    waiting_on_replaced(decoded, &mut sort, &queued, before, replaced, &mut other);
    built_on_other(decoded, &mut sort, &queued, &mut other);
    let kept = changes
        .iter()
        .zip(&sort)
        .filter(|&(_, sort)| *sort != Sort::Other)
        .map(|(bytes, _)| bytes.clone())
        .collect();
    (kept, other.replaced)
}

/// Whether `before` holds the change `hash` applied.
fn applied(before: Option<&automerge::Automerge>, hash: &ChangeHash) -> bool {
    before.is_some_and(|doc| doc.get_change_meta_by_hash(hash).is_some())
}

/// How each change stands before anything it builds on is looked at: the
/// base and what does not decode are built, a change starting a history other
/// than the base is of another, and the rest wait.
fn started(
    decoded: &[Option<automerge::Change>],
    base: Option<usize>,
    before: Option<&automerge::Automerge>,
) -> Vec<Sort> {
    decoded
        .iter()
        .enumerate()
        .map(|(index, change)| match change {
            None => Sort::Built,
            Some(_) if Some(index) == base => Sort::Built,
            Some(change) if change.deps().is_empty() => {
                if applied(before, &change.hash()) {
                    Sort::Built
                } else {
                    Sort::Other
                }
            }
            Some(_) => Sort::Waiting,
        })
        .collect()
}

/// Marks what builds on the base. A batch is mostly in causal order; a change
/// ahead of what it builds on is taken on a later pass.
fn build_on_base(
    decoded: &[Option<automerge::Change>],
    sort: &mut [Sort],
    before: Option<&automerge::Automerge>,
) {
    let mut built: std::collections::HashSet<ChangeHash> = std::collections::HashSet::new();
    let mut grew = true;
    while grew {
        grew = false;
        for (change, sort) in decoded.iter().zip(sort.iter_mut()) {
            let Some(change) = change else { continue };
            if *sort == Sort::Built {
                grew |= built.insert(change.hash());
            } else if *sort == Sort::Waiting
                && change
                    .deps()
                    .iter()
                    .all(|dep| built.contains(dep) || applied(before, dep))
            {
                *sort = Sort::Built;
                built.insert(change.hash());
                grew = true;
            }
        }
    }
}

/// The changes shown to belong to another history, and those of them waiting
/// in the receiver's document.
#[derive(Default)]
struct Other {
    hashes: std::collections::HashSet<ChangeHash>,
    replaced: Vec<ChangeHash>,
}

/// Marks as of another history each waiting change, in the batch or in
/// `queued`, that waits for a change the base replaced: one that is neither
/// applied nor built on the base with this batch, and whose head the base
/// names among those it replaced.
fn waiting_on_replaced(
    decoded: &[Option<automerge::Change>],
    sort: &mut [Sort],
    queued: &[&automerge::Change],
    before: Option<&automerge::Automerge>,
    replaced: impl FnOnce() -> std::collections::BTreeSet<[u8; 4]>,
    other: &mut Other,
) {
    let waits = sort.contains(&Sort::Waiting);
    if !waits
        && queued
            .iter()
            .all(|change| other.hashes.contains(&change.hash()))
    {
        return;
    }
    let replaced = replaced();
    if replaced.is_empty() {
        return;
    }
    let built: std::collections::HashSet<ChangeHash> = decoded
        .iter()
        .zip(sort.iter())
        .filter(|&(_, sort)| *sort == Sort::Built)
        .filter_map(|(change, _)| change.as_ref().map(automerge::Change::hash))
        .collect();
    let for_replaced = |change: &automerge::Change| {
        change.deps().iter().any(|dep| {
            !built.contains(dep)
                && !applied(before, dep)
                && replaced.contains(&collab_types::governance_head(&dep.0))
        })
    };
    for (change, sort) in decoded.iter().zip(sort.iter_mut()) {
        let Some(change) = change else { continue };
        if *sort == Sort::Waiting && for_replaced(change) {
            *sort = Sort::Other;
            other.hashes.insert(change.hash());
        }
    }
    for change in queued {
        if !other.hashes.contains(&change.hash()) && for_replaced(change) {
            other.hashes.insert(change.hash());
            other.replaced.push(change.hash());
        }
    }
}

/// Marks what builds on another history, in the batch or in `queued`, as of
/// it.
fn built_on_other(
    decoded: &[Option<automerge::Change>],
    sort: &mut [Sort],
    queued: &[&automerge::Change],
    other: &mut Other,
) {
    let mut grew = true;
    while grew {
        grew = false;
        for (change, sort) in decoded.iter().zip(sort.iter_mut()) {
            let Some(change) = change else { continue };
            if *sort == Sort::Waiting && change.deps().iter().any(|dep| other.hashes.contains(dep))
            {
                *sort = Sort::Other;
                other.hashes.insert(change.hash());
                grew = true;
            }
        }
        for change in queued {
            if !other.hashes.contains(&change.hash())
                && change.deps().iter().any(|dep| other.hashes.contains(dep))
            {
                other.hashes.insert(change.hash());
                other.replaced.push(change.hash());
                grew = true;
            }
        }
    }
}

/// `before` without the changes waiting in it that `replaced` names, when it
/// names any: what [`Lineage::Same`] admits its changes onto.
pub(crate) fn pruned(before: Option<&AutoCommit>, replaced: Vec<ChangeHash>) -> Option<AutoCommit> {
    before.filter(|_| !replaced.is_empty()).map(|before| {
        let mut doc = before.clone();
        doc.discard_queued(replaced);
        doc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(doc: &mut AutoCommit, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        doc.get_changes(since)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// A change of `doc` named as the action `verb`, as a governance action
    /// commits it.
    fn named(doc: &mut AutoCommit, verb: &str, value: &str, time: i64) {
        doc.put(ROOT, "members", value).unwrap();
        let stamp = collab_types::Stamp {
            verb: verb.to_owned(),
            ..collab_types::Stamp::default()
        };
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(stamp.message())
                .with_time(time),
        );
    }

    #[test]
    fn every_base_keeps_the_actions_the_history_before_it_took() {
        let mut doc = AutoCommit::new();
        named(&mut doc, "add-member", "ada", 1);
        named(&mut doc, "remove-member", "", 2);
        // Bookkeeping names no action, and no base lists it.
        doc.put(ROOT, "checkpoints", "kept").unwrap();
        doc.commit();
        let mut first = rebase(&doc, ActorId::random()).unwrap();
        named(&mut first, "add-member", "cy", 3);
        let second = rebase(&first, ActorId::random()).unwrap();
        assert_eq!(second.clone().get_changes(&[]).len(), 1);
        let listed: Vec<(String, i64)> = named_changes(&second)
            .into_iter()
            .map(|kept| {
                let stamp = collab_types::Stamp::of_message(&kept.message).unwrap();
                (stamp.verb, kept.time)
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("add-member".to_owned(), 1),
                ("remove-member".to_owned(), 2),
                ("add-member".to_owned(), 3),
            ],
            "each action once, in order, through both bases"
        );
        assert_eq!(
            named_changes(&second)
                .iter()
                .map(|kept| kept.change)
                .collect::<Vec<_>>(),
            named_changes(&first)
                .iter()
                .map(|kept| kept.change)
                .collect::<Vec<_>>(),
            "a base names each action by the change that took it"
        );
    }

    tilo_test_env::scenario! {
        fn a_base_replaces_the_history_before_it;

        phase a_base_holds_the_state_and_none_of_the_history;
        {
            let mut doc = AutoCommit::new();
            let strips = doc
                .put_object(ROOT, "strips", automerge::ObjType::Map)
                .unwrap();
            doc.put(&strips, "1", "ssh prod-db").unwrap();
            doc.commit();
            doc.delete(&strips, "1").unwrap();
            doc.put(&strips, "2", "notes").unwrap();
            doc.commit();

            let mut base = rebase(&doc, ActorId::random()).unwrap();
            assert_eq!(generation(&base), 1);
            let history = base.get_changes(&[]);
            assert_eq!(history.len(), 1);
            assert!(!format!("{:?}", history[0].decode()).contains("ssh prod-db"));
            let strips = base.get(ROOT, "strips").unwrap().unwrap().1;
            assert_eq!(
                base.get(&strips, "2").unwrap().unwrap().0.as_str(),
                Some("notes")
            );
            assert_eq!(root(&mut base), Some(history[0].hash()));
            assert_eq!(generation(&rebase(&base, ActorId::random()).unwrap()), 2);
        }

        phase a_newer_base_replaces_and_an_older_history_is_left_out;
        {
            let mut old = AutoCommit::new();
            old.put(ROOT, "name", "Team").unwrap();
            old.commit();
            let history = changes(&mut old, &[]);
            let mut base = rebase(&old, ActorId::random()).unwrap();
            let based = changes(&mut base, &[]);

            for held in [Some(&old), None] {
                let Lineage::Newer {
                    base: mut built,
                    changes,
                } = lineage(held, &based)
                else {
                    panic!("a newer base replaces what is held");
                };
                assert!(changes.is_empty(), "the base is all the batch carries");
                assert_eq!(built.get_heads(), base.get_heads());
            }
            assert!(matches!(
                lineage(Some(&base), &history),
                Lineage::Same { changes: kept, replaced } if kept.is_empty() && replaced.is_empty()
            ));
            assert!(matches!(
                lineage(Some(&base), &based),
                Lineage::Same { changes: kept, replaced } if kept == based && replaced.is_empty()
            ));
            let heads = base.get_heads();
            base.put(ROOT, "name", "Crew").unwrap();
            base.commit();
            let after = changes(&mut base, &heads);
            assert!(matches!(
                lineage(Some(&old), &after),
                Lineage::Same { changes: kept, replaced } if kept == after && replaced.is_empty()
            ));
        }

        phase a_change_of_the_history_a_base_replaced_never_holds_back_the_owners_next;
        // The host writes each base's history afresh under its own actor,
        // so the history a base replaced numbers its changes as the new one
        // does. A change of the old history that reaches a member after the
        // base, or with it, never holds the host's change at its sequence
        // number back: one waiting for a change the base replaced is dropped,
        // and so is one whose number a change built on the base holds,
        // waiting or arriving.
        {
            let RebasedWorkspace {
                old,
                history,
                base,
                based,
                new_history,
            } = rebased_workspace();
            let old_third = vec![history[2].clone()];
            let seq = |bytes: &Vec<u8>| change(bytes).seq();
            assert_eq!(
                (seq(&old_third[0]), seq(&new_history[1])),
                (3, 3),
                "both histories hold the host's third change"
            );

            // The old change after the base, before the host's third: it
            // waits for a change the base replaced, which never comes, so it
            // is left out at once.
            let member = take(None, &base);
            let left_out = take(Some(&member), &old_third);
            assert_eq!(left_out.committed_heads(), member.committed_heads());
            assert_eq!(left_out.committed_document().queued_changes().count(), 0);

            // One already waiting (a document that took it in before it was
            // left out) goes once the host's third change, built on the
            // base, claims its number.
            let waiting = super::super::candidate(Some(&member), &old_third).unwrap();
            assert_eq!(waiting.committed_document().queued_changes().count(), 1);
            let member = take(Some(&waiting), &new_history);
            assert_eq!(member.committed_heads(), based.committed_heads());
            assert_eq!(member.committed_document().queued_changes().count(), 0);
            assert_eq!(
                member.get(ROOT, "grace").unwrap().unwrap().0.as_str(),
                Some("agent")
            );

            // The host's third change ahead of its second, while the old
            // third waits: it waits too, and the old one, which waits for a
            // change the base replaced, goes rather than the new one being
            // refused as a duplicate of its number.
            let ahead = take(Some(&waiting), &new_history[1..]);
            assert_eq!(ahead.committed_document().queued_changes().count(), 2);
            assert!(
                ahead
                    .committed_document()
                    .queued_changes()
                    .all(|queued| queued.hash() != change(&old_third[0]).hash()),
                "the old third is gone"
            );
            let caught_up = take(Some(&ahead), &new_history[..1]);
            assert_eq!(caught_up.committed_heads(), based.committed_heads());
            assert_eq!(caught_up.committed_document().queued_changes().count(), 0);
            // Both in one batch, the old one first: it is left out.
            let mut batch = old_third.clone();
            batch.extend(new_history[1..].iter().cloned());
            let ahead = take(Some(&take(None, &base)), &batch);
            assert_eq!(ahead.committed_document().queued_changes().count(), 2);

            // The old change after the host's third: left out, the batch
            // is not refused.
            let late = take(Some(&member), &old_third);
            assert_eq!(late.committed_heads(), based.committed_heads());
            assert_eq!(late.committed_document().queued_changes().count(), 0);

            // The old change with what is left of its history, in the same batch
            // as the base: only what builds on the base is applied, and the old
            // changes, built on what the base replaced, are left out.
            let mut batch = history[1..].to_vec();
            batch.extend(base.iter().cloned());
            let member = take(Some(&old), &batch);
            assert_eq!(member.committed_heads(), vec![change(&base[0]).hash()]);
            assert_eq!(member.committed_document().queued_changes().count(), 0);
            let member = take(Some(&member), &new_history);
            assert_eq!(member.committed_heads(), based.committed_heads());
            assert_eq!(member.committed_document().queued_changes().count(), 0);
        }

        phase a_change_of_the_bases_history_ahead_of_its_parent_applies_once_the_parent_arrives;
        // A change of the base's own history that arrives before what it builds
        // on waits, and applies once that arrives: nothing is dropped for being
        // early.
        {
            let RebasedWorkspace {
                base,
                based,
                new_history,
                ..
            } = rebased_workspace();
            let member = take(None, &base);

            let ahead = take(Some(&member), &new_history[1..]);
            assert_eq!(ahead.committed_document().queued_changes().count(), 2);
            let member = take(Some(&ahead), &new_history[..1]);
            assert_eq!(member.committed_heads(), based.committed_heads());
            assert_eq!(member.committed_document().queued_changes().count(), 0);
            assert_eq!(
                member.get(ROOT, "grace").unwrap().unwrap().0.as_str(),
                Some("agent")
            );

            // The same in one batch, last first.
            let reversed: Vec<Vec<u8>> = new_history.iter().rev().cloned().collect();
            let member = take(Some(&take(None, &base)), &reversed);
            assert_eq!(member.committed_heads(), based.committed_heads());
        }
    }

    /// What a receiver holding `doc` keeps of `batch`, as the admission
    /// takes it.
    fn take(doc: Option<&AutoCommit>, batch: &[Vec<u8>]) -> AutoCommit {
        match lineage(doc, batch) {
            Lineage::Same { changes, replaced } => {
                let pruned = pruned(doc, replaced);
                super::super::candidate(pruned.as_ref().or(doc), &changes).unwrap()
            }
            Lineage::Newer { base, changes } => {
                super::super::candidate_on(*base, &changes).unwrap()
            }
            Lineage::Refounded => panic!("a workspace of one founding takes the batch"),
        }
    }

    fn change(bytes: &[u8]) -> automerge::Change {
        automerge::Change::try_from(bytes).unwrap()
    }

    /// A host's workspace before and after a base, as `HostedWorkspace::commit`
    /// makes it: the base is the host's first change, and its history
    /// goes on under the same actor.
    struct RebasedWorkspace {
        /// The workspace before the base.
        old: AutoCommit,
        /// The changes of the history the base replaced.
        history: Vec<Vec<u8>>,
        /// The base, one change.
        base: Vec<Vec<u8>>,
        /// The workspace after the base and its new history.
        based: AutoCommit,
        /// The new history's changes after the base.
        new_history: Vec<Vec<u8>>,
    }

    fn rebased_workspace() -> RebasedWorkspace {
        let owner_copy = ActorId::from(vec![7; 32]);
        let mut old = AutoCommit::new().with_actor(owner_copy.clone());
        for (key, value) in [("name", "Team"), ("invited", "grace"), ("grace", "invited")] {
            old.put(ROOT, key, value).unwrap();
            old.commit();
        }
        let history = changes(&mut old, &[]);
        let mut based = rebase(&old, owner_copy).unwrap();
        let base = changes(&mut based, &[]);
        let heads = based.get_heads();
        for value in ["viewer", "removed", "agent"] {
            based.put(ROOT, "grace", value).unwrap();
            based.commit();
        }
        let new_history = changes(&mut based, &heads);
        RebasedWorkspace {
            old,
            history,
            base,
            based,
            new_history,
        }
    }

    tilo_test_env::scenario! {
        fn a_holder_keeps_what_it_wrote_against_the_history_a_base_replaced;

        phase what_the_base_lacks_is_written_again_and_the_makers_change_stays;
        {
            let (maker, holder) = (
                ActorId::from(b"maker".as_slice()),
                ActorId::from(b"holder".as_slice()),
            );
            let mut shared = AutoCommit::new().with_actor(maker.clone());
            let leases = shared
                .put_object(ROOT, "leases", automerge::ObjType::Map)
                .unwrap();
            shared.put(&leases, "update", "nas").unwrap();
            shared.put(ROOT, "name", "Servers").unwrap();
            shared.commit();

            // The holder lets its lease go on the history both hold; the maker,
            // before that reaches it, starts the workspace over and renames it.
            let mut held = shared.fork().with_actor(holder.clone());
            let leases_here = held.get(ROOT, "leases").unwrap().unwrap().1;
            held.delete(&leases_here, "update").unwrap();
            held.put(&leases_here, "backup", "nas").unwrap();
            held.commit();
            let mut base = rebase(&shared, maker).unwrap();
            base.put(ROOT, "name", "Our servers").unwrap();
            base.commit();

            let mut adopted = base.clone();
            assert!(carried(&held, &mut adopted, (&holder, &holder)).unwrap());
            let leases = adopted.get(ROOT, "leases").unwrap().unwrap().1;
            assert!(adopted.get(&leases, "update").unwrap().is_none());
            assert_eq!(
                adopted.get(&leases, "backup").unwrap().unwrap().0.into_string().ok(),
                Some("nas".to_owned())
            );
            assert_eq!(
                adopted.get(ROOT, "name").unwrap().unwrap().0.into_string().ok(),
                Some("Our servers".to_owned()),
                "what the maker wrote on the base stays"
            );
            // Carried as the holder's own change, on the base.
            let last = adopted.get_changes(&base.get_heads());
            assert_eq!(last.len(), 1);
            assert_eq!(last[0].actor_id(), &holder);
        }

        phase a_base_that_holds_everything_carries_nothing;
        {
            let holder = ActorId::from(b"holder".as_slice());
            let mut doc = AutoCommit::new().with_actor(holder.clone());
            doc.put(ROOT, "name", "Team").unwrap();
            doc.commit();
            let mut base = rebase(&doc, ActorId::random()).unwrap();
            let mut adopted = base.clone();
            assert!(!carried(&doc, &mut adopted, (&holder, &holder)).unwrap());
            assert_eq!(adopted.get_heads(), base.get_heads());
        }
    }
}
