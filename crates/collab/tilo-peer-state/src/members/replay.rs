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

/// Every operation `doc` holds, by id, that proves its author. A blob that
/// does not decode, or whose signature is not its author device's, is kept in
/// the document and ignored here: admission refuses such a blob, and a device
/// that was sent one another way is still never governed by it.
#[must_use]
pub fn ops(doc: &AutoCommit) -> BTreeMap<OpId, SignedMembershipOp> {
    let mut held = BTreeMap::new();
    let Some((Value::Object(automerge::ObjType::Map), map)) =
        doc.get(ROOT, MEMBERS_KEY).ok().flatten()
    else {
        return held;
    };
    for key in doc.keys(&map) {
        let Some((Value::Scalar(value), _)) = doc.get(&map, key.as_str()).ok().flatten() else {
            continue;
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            continue;
        };
        // ignored-failure: an operation that does not prove its author governs nothing
        let Ok(op) = SignedMembershipOp::verified(bytes) else {
            continue;
        };
        // ignored-failure: an operation that cannot be keyed by its own id governs nothing
        let Ok(id) = op.id() else {
            continue;
        };
        held.insert(id, op);
    }
    held
}

/// The operations no other operation was written on top of: the membership
/// heads a device writing now names as its parents.
#[must_use]
pub fn heads(held: &BTreeMap<OpId, SignedMembershipOp>) -> Vec<OpId> {
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
pub fn genesis(held: &BTreeMap<OpId, SignedMembershipOp>) -> Option<OpId> {
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
    start(doc, workspace, &ops(doc)).is_some()
}

/// The workspace's membership as `doc`'s operations make it.
///
/// A workspace whose document holds no genesis operation the owner's device
/// wrote is still governed by its legacy rows, and reads as the projection of
/// them.
#[must_use]
pub fn replay(doc: &AutoCommit, workspace: &str) -> WorkspacePolicy {
    let held = ops(doc);
    let Some(mut policy) = start(doc, workspace, &held) else {
        // No log to replay: the rows govern, read the one order every device
        // reads them in.
        let mut policy = tilo_desktop_model::projection::workspace_policy_of(doc, workspace);
        order(&mut policy);
        return policy;
    };
    redeem(doc, workspace, &mut policy);
    let Some(genesis) = genesis(&held) else {
        order(&mut policy);
        return policy;
    };
    let mut applied: BTreeSet<OpId> = BTreeSet::from([genesis]);
    order(&mut policy);
    let mut pending: BTreeMap<OpId, &SignedMembershipOp> = held
        .iter()
        .filter(|(id, _)| **id != genesis)
        .map(|(id, op)| (*id, op))
        .collect();
    while !pending.is_empty() {
        let reached: Vec<OpId> = pending
            .iter()
            .filter(|(_, op)| op.parents.iter().all(|parent| applied.contains(parent)))
            .map(|(id, _)| *id)
            .collect();
        if reached.is_empty() {
            // Every operation left was written on top of one this device does
            // not hold, or on top of one that was never applied: it waits.
            break;
        }
        let Some((next, _)) = reached
            .into_iter()
            .filter_map(|id| pending.get(&id).map(|op| (id, op)))
            .min_by_key(|(id, op)| (tier(&policy, &op.person()), *id))
        else {
            break;
        };
        let Some(op) = pending.remove(&next) else {
            break;
        };
        op_effect(&mut policy, op);
        order(&mut policy);
        applied.insert(next);
    }
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
fn start(
    doc: &AutoCommit,
    workspace: &str,
    held: &BTreeMap<OpId, SignedMembershipOp>,
) -> Option<WorkspacePolicy> {
    let owner = tilo_desktop_model::projection::workspace_policy_of(doc, workspace).owner?;
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
    Some(membership.policy())
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
fn redeem(doc: &AutoCommit, workspace: &str, policy: &mut WorkspacePolicy) {
    use base64::Engine as _;
    // ignored-failure: a ledger this device cannot read admits no one
    let Ok(receipts) = crate::workspace::invitation_receipts(doc) else {
        return;
    };
    let rows = tilo_desktop_model::projection::workspace_policy_of(doc, workspace);
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
                policy,
                workspace,
                &signed,
                receipt.user_public_key,
                at_ns,
            ) else {
                continue;
            };
            if policy.is_owner(&person) || policy.is_banned(&person) {
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
    let person = op.person();
    let bound = roles::foreign_device_bound();
    let cx = Context::at(op.at_ns / 1_000_000);
    match &op.command {
        // The genesis projection is the starting membership, not a change to it.
        MembershipOpCommand::Genesis { .. } => {}
        MembershipOpCommand::Project { changes } => {
            let mut next = policy.clone();
            let foreign = op.author.capabilities.foreign && !next.is_owner(&person);
            for change in changes {
                // The change is decided against the membership as the changes
                // before it left it, and applied to the operation's own copy:
                // one refused change takes the whole operation with it.
                let mut after = next.clone();
                let authority = next.person_on_device(&person, foreign, &bound);
                // ignored-failure: a change this author's grants do not allow
                // takes the whole operation with it, and it has no effect
                if after
                    .apply(&person, &authority, change.clone(), &cx, false)
                    .is_err()
                {
                    return;
                }
                next = after;
            }
            *policy = next;
        }
    }
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
    if let Some(before) = before {
        for (key, bytes) in raw(before) {
            match held.get(&key) {
                Some(held) if held == &bytes => {}
                Some(_) => return Err("the workspace's membership operations were changed".into()),
                None => return Err("the workspace's membership operations were removed".into()),
            }
        }
    }
    let mut added: usize = 0;
    for (key, bytes) in held {
        if before.is_some_and(|before| raw(before).contains_key(&key)) {
            continue;
        }
        // ignored-failure: an operation that does not prove its writer is refused
        let op = SignedMembershipOp::verified(&bytes)?;
        // ignored-failure: an operation stored under a key that is not its own
        // id is refused
        if op.id()?.to_key() != key {
            return Err("a membership operation is stored under another operation's id".into());
        }
        added = added.saturating_add(1);
    }
    Ok(added)
}

/// The workspace document's membership operations as stored, by key.
fn raw(doc: &AutoCommit) -> BTreeMap<String, Vec<u8>> {
    let mut held = BTreeMap::new();
    let Some((Value::Object(automerge::ObjType::Map), map)) =
        doc.get(ROOT, MEMBERS_KEY).ok().flatten()
    else {
        return held;
    };
    for key in doc.keys(&map) {
        if let Some((Value::Scalar(value), _)) = doc.get(&map, key.as_str()).ok().flatten()
            && let ScalarValue::Bytes(bytes) = value.as_ref()
        {
            held.insert(key, bytes.clone());
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
    let held = ops(doc);
    if start(doc, workspace, &held).is_none() {
        return Ok(());
    }
    let policy = replay(doc, workspace);
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
