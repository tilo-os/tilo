//! The workspace's membership, derived from its operations, the same way on
//! every device.
//!
//! Every receiver keeps every operation it is sent (admission refuses a
//! changed or unproven blob, never a member's own change), so the set of
//! operations a device holds is the workspace's record of who changed what.
//! Replay is a function of that set alone:
//!
//! - the genesis operation gives the membership the legacy rows held, and
//!   names the heads it is read at, so every device projects the same rows;
//! - the rest are applied in one deterministic order: no operation before one
//!   it was written on top of, and operations written concurrently ordered by
//!   their author's standing in the workspace as it stands when they are
//!   reached — the owner's first, then an administrator's, then everyone
//!   else's — and, among equals, by id;
//! - each is decided against the membership at the point it is reached
//!   ([`tilo_action_policy::WorkspacePolicy::apply`]), so an operation a
//!   writer was not allowed to make changes nothing, and every device holding
//!   the same operations applies exactly the same ones.
//!
//! Two devices that hold the same operations therefore hold the same
//! membership. A device that holds fewer catches up as operations arrive; one
//! that holds an operation it cannot apply, because its author had lost the
//! grant it needed when they wrote it, keeps it and is unaffected by it.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use tilo_action_policy::{Change, Context, Grant, GrantId, Role, WorkspacePolicy, roles};

use super::legacy;
use super::op::{MEMBERS_KEY, MembershipOpCommand, OpId, SignedMembershipOp};

/// The operations a workspace document holds, by id.
pub type HeldOps = BTreeMap<OpId, std::sync::Arc<SignedMembershipOp>>;

/// Every operation `doc` holds, by id, that proves its author. A blob that
/// does not decode, or whose signature is not its author device's, is kept in
/// the document and ignored here: admission refuses such a blob, and a device
/// that was sent one another way is still never governed by it.
#[must_use]
pub fn ops(doc: &AutoCommit) -> HeldOps {
    let mut held = BTreeMap::new();
    for bytes in raw(doc).into_values() {
        // ignored-failure: an operation that does not prove its author, or
        // cannot be keyed by its own id, governs nothing
        let Ok((id, op)) = SignedMembershipOp::verified_stored(&bytes) else {
            continue;
        };
        held.insert(id, op);
    }
    held
}

/// The operations no other operation was written on top of: the membership
/// heads a device writing now names as its parents.
#[must_use]
pub fn heads(held: &HeldOps) -> Vec<OpId> {
    let mut heads: BTreeSet<OpId> = held.keys().copied().collect();
    for op in held.values() {
        for parent in &op.parents {
            heads.remove(parent);
        }
    }
    heads.into_iter().collect()
}

/// The id of the genesis operation `held` holds, when it holds one.
#[must_use]
pub fn genesis(held: &HeldOps) -> Option<OpId> {
    held.values()
        .filter(|op| {
            matches!(op.command, MembershipOpCommand::Genesis { .. }) && op.parents.is_empty()
        })
        .filter_map(|op| op.id().ok())
        .min()
}

/// Whether the workspace's log starts in `doc`: it holds a genesis operation
/// its owner's device wrote, at heads this device holds the history of. Where
/// it does not, the document's rows govern the workspace as they did before the
/// log, and the operations are kept for when they start.
#[must_use]
pub fn starts(doc: &AutoCommit, workspace: &str) -> bool {
    remembered(doc, workspace).1
}

/// The workspace's membership as `doc`'s operations make it.
///
/// A workspace whose document holds no genesis operation the owner's device
/// wrote is still governed by its legacy rows, and reads as the projection of
/// them.
#[must_use]
pub fn replay(doc: &AutoCommit, workspace: &str) -> WorkspacePolicy {
    remembered(doc, workspace).0
}

/// The membership a document state gives, as [`remembered`] keeps it.
#[derive(Clone)]
struct Replayed {
    policy: WorkspacePolicy,
    /// Whether the log starts in the state ([`starts`]).
    started: bool,
    /// The document keys of the state's operations.
    op_keys: BTreeSet<String>,
    /// Operations the state's operations were written on top of that it does
    /// not hold: one arriving could let those apply.
    missing: BTreeSet<OpId>,
}

/// The document states whose membership was read, by workspace and heads.
type States =
    std::sync::Mutex<std::collections::VecDeque<(String, Vec<automerge::ChangeHash>, Replayed)>>;

fn states() -> &'static States {
    static STATES: std::sync::OnceLock<States> = std::sync::OnceLock::new();
    STATES.get_or_init(States::default)
}

/// The states a write and the admissions after it read again.
const REMEMBERED: usize = 8;

fn recall(workspace: &str, heads: &[automerge::ChangeHash]) -> Option<Replayed> {
    states().lock().ok().and_then(|states| {
        states
            .iter()
            .find(|(of, at, _)| of == workspace && at.as_slice() == heads)
            .map(|(_, _, replayed)| replayed.clone())
    })
}

fn keep(workspace: &str, heads: Vec<automerge::ChangeHash>, replayed: Replayed) {
    if let Ok(mut states) = states().lock() {
        if states.len() >= REMEMBERED {
            states.pop_front();
        }
        states.push_back((workspace.to_owned(), heads, replayed));
    }
}

/// [`replay`], and whether the log starts in `doc` ([`starts`]).
fn remembered(doc: &AutoCommit, workspace: &str) -> (WorkspacePolicy, bool) {
    // A change is replayed where it is written and again where it is taken
    // in, from the same document state: the membership that state gives is
    // remembered by its heads. A document with a change still open is not
    // the state its heads name, and is replayed as it is.
    if doc.pending_ops() > 0 {
        let replayed = replayed_from(doc, workspace);
        return (replayed.policy, replayed.started);
    }
    let heads = doc.base_heads();
    if let Some(replayed) = recall(workspace, &heads) {
        return (replayed.policy, replayed.started);
    }
    let replayed = replayed_from(doc, workspace);
    let answer = (replayed.policy.clone(), replayed.started);
    keep(workspace, heads, replayed);
    answer
}

/// The document keys a replay reads, besides its operations': the rows the
/// membership starts from, the invitations that admitted people, and the
/// map the operations were kept in before each had a key of its own.
const REPLAY_KEYS: [&str; 6] = [
    tilo_desktop_model::projection::WORKSPACE_OWNER_KEY,
    tilo_desktop_model::projection::WORKSPACE_MEMBERS_KEY,
    tilo_desktop_model::projection::WORKSPACE_MEMBERS_JOINED_KEY,
    tilo_desktop_model::projection::WORKSPACE_GRANTS_KEY,
    "invitation_receipts_v1",
    MEMBERS_KEY,
];

/// Remember the membership of `after`, the state at `before` with `changes`
/// on top, as the one remembered for that state, when the changes wrote
/// nothing a replay reads but operations that change no membership row
/// ([`MembershipOpCommand::Governance`]) and that no operation the state
/// holds waits for.
///
/// Such an operation is applied, whenever it is reached, without moving the
/// membership, and the order of the others is decided by the membership
/// alone, so it leaves every other operation's effect as it was. A name, a
/// device's row or the operation each governance write records then costs
/// no replay of every admission the workspace holds.
pub(crate) fn carry(
    before: &[automerge::ChangeHash],
    after: &AutoCommit,
    changes: &[automerge::Change],
    workspace: &str,
) {
    if after.pending_ops() > 0 {
        return;
    }
    let Some(mut replayed) = recall(workspace, before) else {
        return;
    };
    let Some(touched) = crate::workspace::rows::touched(after, changes) else {
        return;
    };
    for key in touched.keys() {
        if REPLAY_KEYS.contains(&key.as_str()) {
            return;
        }
        if super::op::keyed(key).is_none() {
            continue;
        }
        if !replayed.op_keys.insert(key.clone()) {
            // An operation the state held, written over.
            return;
        }
        let Some((Value::Scalar(value), _)) = after.get(ROOT, key.as_str()).ok().flatten() else {
            return;
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return;
        };
        // ignored-failure: an operation that does not prove its writer is
        // never applied, so it moves nothing
        if let Ok((id, op)) = SignedMembershipOp::verified_stored(bytes) {
            if !matches!(op.command, MembershipOpCommand::Governance { .. })
                || replayed.missing.contains(&id)
            {
                return;
            }
            replayed.missing.extend(op.parents.iter().copied());
        }
    }
    // Every carry a test makes is checked against replaying the state.
    #[cfg(test)]
    {
        let fresh = read_replay(after, workspace);
        assert!(
            fresh.policy == replayed.policy && fresh.started == replayed.started,
            "a carried membership is the one its state replays to"
        );
    }
    keep(workspace, after.base_heads(), replayed);
}

/// [`remembered`], read from the document.
fn replayed_from(doc: &AutoCommit, workspace: &str) -> Replayed {
    crate::governance_work::replayed();
    read_replay(doc, workspace)
}

/// [`replayed_from`], uncounted.
fn read_replay(doc: &AutoCommit, workspace: &str) -> Replayed {
    let raw = raw(doc);
    let op_keys = raw
        .keys()
        .map(|key| format!("{MEMBERS_KEY}/{key}"))
        .collect();
    let held: HeldOps = raw
        .into_values()
        // ignored-failure: an operation that does not prove its author, or
        // cannot be keyed by its own id, governs nothing
        .filter_map(|bytes| SignedMembershipOp::verified_stored(&bytes).ok())
        .collect();
    let missing = held
        .values()
        .flat_map(|op| op.parents.iter().copied())
        .filter(|parent| !held.contains_key(parent))
        .collect();
    let (policy, started) = replay_held(doc, workspace, &held);
    Replayed {
        policy,
        started,
        op_keys,
        missing,
    }
}

/// The membership `held`, `doc`'s operations, give, and whether the log
/// starts.
fn replay_held(doc: &AutoCommit, workspace: &str, held: &HeldOps) -> (WorkspacePolicy, bool) {
    let Some(mut policy) = start(doc, workspace, held) else {
        // No log to replay: the rows govern, read the one order every device
        // reads them in.
        let mut policy = crate::workspace::policy_of(doc, workspace);
        order(&mut policy);
        return (policy, false);
    };
    redeem(doc, workspace, &mut policy);
    let Some(genesis) = genesis(held) else {
        order(&mut policy);
        return (policy, true);
    };
    order(&mut policy);
    // Each operation waits for the parents it was written on top of; one
    // whose parents are all applied is ready. Of the ready ones the next is
    // the one the order reaches first. An operation written on top of one
    // this device does not hold, or one never applied, waits for good.
    let mut waiting: BTreeMap<OpId, usize> = BTreeMap::new();
    let mut children: BTreeMap<OpId, Vec<OpId>> = BTreeMap::new();
    let mut ready: BTreeSet<OpId> = BTreeSet::new();
    for (id, op) in held {
        if *id == genesis {
            continue;
        }
        let parents: BTreeSet<OpId> = op
            .parents
            .iter()
            .copied()
            .filter(|parent| *parent != genesis)
            .collect();
        if parents.is_empty() {
            ready.insert(*id);
            continue;
        }
        for parent in &parents {
            children.entry(*parent).or_default().push(*id);
        }
        waiting.insert(*id, parents.len());
    }
    while let Some(next) = ready
        .iter()
        .filter_map(|id| held.get(id).map(|op| (*id, op)))
        .min_by_key(|(id, op)| (tier(&policy, &op.person()), *id))
        .map(|(id, _)| id)
    {
        ready.remove(&next);
        if let Some(op) = held.get(&next) {
            op_effect(&mut policy, op);
            order(&mut policy);
        }
        for child in children.remove(&next).unwrap_or_default() {
            if let Some(count) = waiting.get_mut(&child) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    waiting.remove(&child);
                    ready.insert(child);
                }
            }
        }
    }
    (policy, true)
}

/// The membership `doc`'s rows hold, its grants in the one order the log's
/// replay holds them in: which order the rows are read in (the grants this
/// build gives every workspace come last) is no part of what the workspace
/// allows, so the two compare as the same membership when they hold the same.
#[must_use]
pub fn rows(doc: &AutoCommit, workspace: &str) -> WorkspacePolicy {
    let mut policy = crate::workspace::policy_of(doc, workspace);
    order(&mut policy);
    policy
}

/// The grants of a membership in the one order every device reads them in: by
/// id, the order the document holds their rows in. Which grant comes first is
/// no part of what a workspace allows.
fn order(policy: &mut WorkspacePolicy) {
    policy
        .grants
        .sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
}

/// The membership `held`'s genesis operation starts the log from, when there
/// is one the workspace can start from: written by the owner's device (the
/// legacy `owner` row names them, and nothing but the owner's devices writes
/// that), at heads this device holds the history of. A genesis by anyone
/// else's device, or naming a history this device does not hold, starts
/// nothing.
fn start(doc: &AutoCommit, workspace: &str, held: &HeldOps) -> Option<WorkspacePolicy> {
    let owner = crate::workspace::policy_of(doc, workspace).owner?;
    let op = held.get(&genesis(held)?)?;
    if op.person() != owner {
        return None;
    }
    let MembershipOpCommand::Genesis {
        legacy_heads,
        membership,
    } = &op.command
    else {
        return None;
    };
    if !legacy_heads.is_empty() && legacy::at_heads(doc, legacy_heads).is_none() {
        return None;
    }
    // The built-in roles as this build defines them, as the rows read them:
    // a log an earlier build started holds that build's definitions, which
    // no operation may rewrite, so read as written they would set the log
    // apart from the rows for good and refuse every change on top.
    Some(membership.policy().with_current_roles())
}

/// Fold the workspace's invitation receipts into `policy`: a redemption the
/// workspace holds is its joiner's admission at the time it records them
/// joining, whoever wrote it. A receipt whose joiner the workspace no longer
/// records a time for is one whose admission was taken back; it admits no one.
///
/// The admission is not an operation: whoever redeems it writes the receipt
/// and the joining time into the workspace's rows, and every device folds
/// them here. The joining time is the one the workspace holds for them, and
/// the rows are where it lives until an operation records the membership that
/// includes them, so a redemption reaches the membership before any operation
/// names it.
///
/// Whether the workspace honours an invitation is what its rows record of who
/// may invite — the grants an operation writes after the log starts are in
/// them, and the membership the log starts from need not name them yet. A
/// grant the workspace's rows do not hold admits no one here, as at the
/// admission each device received.
fn redeem(doc: &AutoCommit, workspace: &str, policy: &mut WorkspacePolicy) {
    use base64::Engine as _;
    // ignored-failure: a ledger this device cannot read admits no one
    let Ok(receipts) = crate::workspace::invitation_receipts(doc) else {
        return;
    };
    let rows = crate::workspace::policy_of(doc, workspace);
    for (_invite, values) in receipts {
        let mut values = values;
        // The order the ledger holds an invitation's receipts in is the order
        // they were written in, which is not the same on every device.
        values.sort();
        for value in values {
            // ignored-failure: a receipt that does not decode admits no one
            let Ok(receipt) = serde_json::from_slice::<crate::state::RedemptionReceipt>(&value)
            else {
                continue;
            };
            let Some(text) = receipt.invitation.as_deref() else {
                continue;
            };
            // ignored-failure: an invitation that does not decode admits no one
            let Ok(signed) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text) else {
                continue;
            };
            if *blake3::hash(&signed).as_bytes() != receipt.invite_id {
                continue;
            }
            let person = hex::encode(receipt.user_public_key);
            let Some(joined_ms) = policy
                .joined
                .get(&person)
                .or_else(|| rows.joined.get(&person))
                .copied()
            else {
                continue;
            };
            let at_ns = joined_ms.saturating_mul(1_000_000);
            // ignored-failure: an invitation that does not admit its joiner admits no one
            let Ok(grant) = crate::workspace::membership::invitation_admits(
                &rows,
                workspace,
                &signed,
                receipt.user_public_key,
                at_ns,
            ) else {
                continue;
            };
            if rows.is_owner(&person) || rows.is_banned(&person) {
                continue;
            }
            // A redemption is how someone entered the workspace, not what
            // role they hold now: the membership reached before it decides.
            policy.joined.entry(person.clone()).or_insert(joined_ms);
            policy
                .members
                .entry(person)
                .or_insert_with(|| Role::from(grant.role));
        }
    }
}

/// An author's standing in the workspace as the order reaches them: an owner
/// first, then an administrator, then everyone else. Operations written
/// concurrently are applied in this order, so two devices that wrote without
/// seeing each other resolve the same way on every device.
///
/// Ownership is a role: the person the workspace was made for holds it, and so
/// does anyone else its rows name as an owner.
fn tier(policy: &WorkspacePolicy, person: &str) -> u8 {
    if policy.role_of(person) == Some(Role::Owner) {
        return 0;
    }
    if policy
        .grants
        .iter()
        .any(|grant| grant.id == roles::administrator_grant_id(person))
    {
        return 1;
    }
    2
}

/// Fold one operation into the membership: the changes it carries, in order,
/// each decided against the membership as the operations before it left it. An
/// operation with a change its author's grants do not allow changes nothing:
/// one refused step takes the whole operation with it, so a writer cannot slip
/// a change past the membership by putting an allowed one in front of it.
fn op_effect(policy: &mut WorkspacePolicy, op: &SignedMembershipOp) {
    // An operation that changes no row of the membership leaves it as it is,
    // without copying it: a workspace's log holds one for every admission.
    if !matches!(op.command, MembershipOpCommand::Project { .. }) {
        return;
    }
    // ignored-failure: a change this author's grants do not allow takes the
    // whole operation with it, and it has no effect
    if let Ok(next) = effect(policy, op) {
        *policy = next;
    }
}

/// The membership `op` leaves `policy` at ([`op_effect`]), or the first of its
/// changes its author's grants do not allow, with why.
fn effect(
    policy: &WorkspacePolicy,
    op: &SignedMembershipOp,
) -> Result<WorkspacePolicy, Box<(Change, tilo_action_policy::Refusal)>> {
    let person = op.person();
    let bound = roles::foreign_device_bound();
    let cx = Context::at(op.at_ns / 1_000_000);
    match &op.command {
        // A governance operation changes no row of the membership: what it
        // touched is judged where its batch is admitted.
        MembershipOpCommand::Genesis { .. } | MembershipOpCommand::Governance { .. } => {
            Ok(policy.clone())
        }
        MembershipOpCommand::Project { changes } => {
            let mut next = policy.clone();
            let foreign = op.author.capabilities.foreign && !next.is_owner(&person);
            for change in changes {
                // The change is decided against the membership as the changes
                // before it left it, and applied to the operation's own copy.
                let mut after = next.clone();
                let authority = next.person_on_device(&person, foreign, &bound);
                after
                    .apply(&person, &authority, change.clone(), &cx, false)
                    .map_err(|refusal| Box::new((change.clone(), refusal)))?;
                next = after;
            }
            Ok(next)
        }
    }
}

/// Why `op`, written on top of the membership `policy`, changes nothing: the
/// first of its changes its author's grants do not allow, and the answer it
/// got. `None` when it applies.
#[must_use]
pub fn refused(policy: &WorkspacePolicy, op: &SignedMembershipOp) -> Option<String> {
    effect(policy, op)
        .err()
        .map(|refused| format!("{:?} was refused: {:?}", refused.0, refused.1))
}

/// Why `op`, the steps from the membership the log holds (`held`) to the
/// rows' (`rows`), does not reach the rows: which step the log refuses and
/// whose it is, and where the log differs from the rows. An operator reads it
/// to tell a change this device may not make from rows the log never wrote.
#[must_use]
pub fn refusal(held: &WorkspacePolicy, rows: &WorkspacePolicy, op: &SignedMembershipOp) -> String {
    let why = refused(held, op).unwrap_or_else(|| "the log does not reach the rows".to_owned());
    format!(
        "the membership change is not this device's to make, as device {} of {}: {why}; the log differs from the rows in: {}",
        hex::encode(op.author.device_public_key),
        op.person(),
        differences(held, rows).join("; ")
    )
}

/// Why rows that differ from the log (`held`) only in what no membership step
/// writes cannot be written: where they differ.
#[must_use]
pub fn unwritten(held: &WorkspacePolicy, rows: &WorkspacePolicy) -> String {
    format!(
        "the membership change is not this device's to make: the log and the rows differ in what no step writes: {}",
        differences(held, rows).join("; ")
    )
}

/// Where two memberships of one workspace differ, in words an operator reads:
/// who is in only one, whose role or admission time differs, which grants
/// only one holds or holds on other terms.
#[must_use]
pub fn differences(one: &WorkspacePolicy, other: &WorkspacePolicy) -> Vec<String> {
    let mut said = Vec::new();
    if one.owner != other.owner {
        said.push(format!("owner {:?} against {:?}", one.owner, other.owner));
    }
    for (person, role) in &one.members {
        match other.members.get(person) {
            None => said.push(format!("member {person} ({role:?}) only in the first")),
            Some(theirs) if theirs != role => {
                said.push(format!("member {person} is {role:?} against {theirs:?}"));
            }
            Some(_) => {}
        }
    }
    for (person, role) in &other.members {
        if !one.members.contains_key(person) {
            said.push(format!("member {person} ({role:?}) only in the second"));
        }
    }
    for (person, at) in &one.joined {
        if other.joined.get(person) != Some(at) {
            said.push(format!(
                "{person} joined at {at} against {:?}",
                other.joined.get(person)
            ));
        }
    }
    for person in other.joined.keys() {
        if !one.joined.contains_key(person) {
            said.push(format!("{person} has a joining time only in the second"));
        }
    }
    let theirs: BTreeMap<&GrantId, &Grant> = other
        .grants
        .iter()
        .map(|grant| (&grant.id, grant))
        .collect();
    let ours: BTreeMap<&GrantId, &Grant> =
        one.grants.iter().map(|grant| (&grant.id, grant)).collect();
    for (id, grant) in &ours {
        match theirs.get(id) {
            None => said.push(format!("grant {} only in the first", id.as_str())),
            Some(held) if held != grant => {
                said.push(format!("grant {} on other terms", id.as_str()));
            }
            Some(_) => {}
        }
    }
    for id in theirs.keys() {
        if !ours.contains_key(id) {
            said.push(format!("grant {} only in the second", id.as_str()));
        }
    }
    said
}

/// The changes that turn `from` into `to`, as the steps an operation carries,
/// or why they are not a set of governance changes: a different workspace or
/// owner (both structural), a grant rewritten under an id it already has, or an
/// admission time someone would be given back or taken away.
///
/// This is the writer's side of [`replay`]: a device that changed the
/// workspace's rows writes the steps that lead from the membership it held to
/// the one it holds now, and every device reads them back with [`replay`].
pub fn steps(from: &WorkspacePolicy, to: &WorkspacePolicy) -> Result<Vec<Change>, String> {
    if from.workspace != to.workspace || from.owner != to.owner {
        return Err("a workspace's owner and identity are not a membership change".into());
    }
    let mut steps = Vec::new();
    for person in from.members.keys() {
        if !to.members.contains_key(person) {
            steps.push(Change::RemoveMember {
                person: person.clone(),
            });
        }
    }
    for (person, role) in &to.members {
        match from.members.get(person) {
            Some(held) if held != role => steps.push(Change::SetRole {
                person: person.clone(),
                role: *role,
            }),
            Some(_) => {}
            None => steps.push(Change::AddMember {
                person: person.clone(),
                role: *role,
            }),
        }
    }
    let before: BTreeMap<&GrantId, &Grant> =
        from.grants.iter().map(|grant| (&grant.id, grant)).collect();
    let after: BTreeMap<&GrantId, &Grant> =
        to.grants.iter().map(|grant| (&grant.id, grant)).collect();
    for (id, grant) in &before {
        // A grant naming a member who leaves leaves with them: their removal
        // drops it, so the operation does not name it again.
        if matches!(&grant.subject, tilo_action_policy::Subject::Person(person)
            if from.members.contains_key(person) && !to.members.contains_key(person))
        {
            continue;
        }
        match after.get(id) {
            Some(held) if held == grant => {}
            // A grant kept under the id it had with different terms is one
            // revoked and given again: a grant is the terms it names, not the
            // id it was kept under.
            _ => steps.push(Change::Revoke((*id).clone())),
        }
    }
    for (id, grant) in &after {
        match before.get(id) {
            Some(held) if held == grant => {}
            _ => steps.push(Change::Grant((*grant).clone())),
        }
    }
    // A member's admission time is recorded once, when they are written in;
    // changing it afterwards is no governance change.
    for (person, at) in &from.joined {
        if to.joined.get(person).is_some_and(|held| held != at) {
            return Err(format!(
                "{person}'s admission time is not a governance change"
            ));
        }
    }
    Ok(steps)
}

/// Whether `candidate` keeps every operation `before` holds and adds only
/// operations that prove their writer, and how many it adds.
///
/// Admission refuses a batch that changes or removes a held operation: an
/// operation is immutable, and a device that cannot drop one cannot drop the
/// change it made.
pub fn growth(before: Option<&AutoCommit>, candidate: &AutoCommit) -> Result<usize, String> {
    let held = raw(candidate);
    let before = before.map(raw).unwrap_or_default();
    for (key, bytes) in &before {
        match held.get(key) {
            Some(held) if held == bytes => {}
            Some(_) => return Err("the workspace's membership operations were changed".into()),
            None => {
                return Err("the workspace's membership operations were removed".into());
            }
        }
    }
    let mut added: usize = 0;
    for (key, bytes) in held {
        if before.contains_key(&key) {
            continue;
        }
        // ignored-failure: an operation that does not prove its writer is refused
        let (id, _) = SignedMembershipOp::verified_stored(&bytes)?;
        // ignored-failure: an operation stored under a key that is not its own
        // id is refused
        if id.to_key() != key {
            return Err("a membership operation is stored under another operation's id".into());
        }
        added = added.saturating_add(1);
    }
    Ok(added)
}

/// The workspace document's membership operations as stored, by operation key:
/// each under its own root key ([`super::op::op_key`]), and — in a document
/// written before operations had a key each — in the map [`MEMBERS_KEY`] names.
pub(crate) fn raw(doc: &AutoCommit) -> BTreeMap<String, Vec<u8>> {
    let mut held = BTreeMap::new();
    for key in doc.keys(ROOT) {
        let Some(op_key) = super::op::keyed(&key) else {
            continue;
        };
        if let Some((Value::Scalar(value), _)) = doc.get(ROOT, key.as_str()).ok().flatten()
            && let ScalarValue::Bytes(bytes) = value.as_ref()
        {
            held.insert(op_key.to_owned(), bytes.clone());
        }
    }
    if let Some((Value::Object(automerge::ObjType::Map), map)) =
        doc.get(ROOT, MEMBERS_KEY).ok().flatten()
    {
        for key in doc.keys(&map) {
            if let Some((Value::Scalar(value), _)) = doc.get(&map, key.as_str()).ok().flatten()
                && let ScalarValue::Bytes(bytes) = value.as_ref()
            {
                held.insert(key, bytes.clone());
            }
        }
    }
    held
}

/// Project the membership the operations give into the document's rows, so a
/// store, an older build and anything reading a workspace document without its
/// operations reads the membership they give.
///
/// A workspace whose operations do not start yet is left as it is: its rows
/// govern until its owner's device starts its log.
pub fn settle(doc: &mut AutoCommit, workspace: &str) -> Result<(), String> {
    let (policy, started) = remembered(doc, workspace);
    if !started {
        return Ok(());
    }
    materialise(doc, &policy)?;
    Ok(())
}

/// The rows of `doc` that hold the membership, as `policy` names it: written
/// so a device on an older build, a store and a relay that keep only the
/// document all read the same membership the operations give.
///
/// They are a projection and never an authority: a device re-projects them
/// from the operations it holds.
pub fn materialise(doc: &mut AutoCommit, policy: &WorkspacePolicy) -> Result<bool, String> {
    use tilo_desktop_model::projection::{
        WORKSPACE_GRANTS_KEY, WORKSPACE_MEMBERS_JOINED_KEY, WORKSPACE_MEMBERS_KEY,
    };
    let mut changed = false;
    let members = policy
        .members
        .iter()
        .filter_map(|(person, role)| Some((person.clone(), serde_json::to_vec(role).ok()?)))
        .collect();
    changed |= project_rows(doc, WORKSPACE_MEMBERS_KEY, members)?;
    let joined = policy
        .joined
        .iter()
        .filter_map(|(person, at)| Some((person.clone(), serde_json::to_vec(at).ok()?)))
        .collect();
    changed |= project_rows(doc, WORKSPACE_MEMBERS_JOINED_KEY, joined)?;
    let grants = policy
        .grants
        .iter()
        .filter_map(|grant| {
            Some((
                grant.id.as_str().to_owned(),
                serde_json::to_vec(grant).ok()?,
            ))
        })
        .collect();
    changed |= project_rows(doc, WORKSPACE_GRANTS_KEY, grants)?;
    Ok(changed)
}

/// Make the map at `key` hold exactly `rows`.
fn project_rows(
    doc: &mut AutoCommit,
    key: &str,
    rows: BTreeMap<String, Vec<u8>>,
) -> Result<bool, String> {
    let mut changed = false;
    let map = match doc.get(ROOT, key).map_err(|error| error.to_string())? {
        Some((Value::Object(automerge::ObjType::Map), map)) => map,
        Some(_) => return Err(format!("the workspace's {key} is not a map")),
        None if rows.is_empty() => return Ok(false),
        None => {
            changed = true;
            doc.put_object(ROOT, key, automerge::ObjType::Map)
                .map_err(|error| error.to_string())?
        }
    };
    let held: Vec<String> = doc.keys(&map).collect();
    for name in held {
        if rows.contains_key(&name) {
            continue;
        }
        doc.delete(&map, name.as_str())
            .map_err(|error| error.to_string())?;
        changed = true;
    }
    for (name, bytes) in rows {
        let held = match doc
            .get(&map, name.as_str())
            .map_err(|error| error.to_string())?
        {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(held) => Some(held.clone()),
                _ => None,
            },
            _ => None,
        };
        if held.as_ref() == Some(&bytes) {
            continue;
        }
        doc.put(&map, name, ScalarValue::Bytes(bytes))
            .map_err(|error| error.to_string())?;
        changed = true;
    }
    Ok(changed)
}
