//! Membership changes a member's device writes itself.
//!
//! A room's members, their roles, the grants on it and the devices each
//! member holds it on are changed by actions, under the action policy
//! (`tilo_action_policy::RoomPolicy`). The owner's devices write every part
//! of the room's governance; anyone else writes only its membership, and
//! only what their own grants let them.
//!
//! A batch is judged where its writer made it: against the room as it stood
//! at the batch's own dependencies, not the receiver's latest state. Every
//! receiver holding the same changes reaches the same answer, whichever
//! order they arrived in: a change made before its writer lost a grant
//! stands, one made after does not.
//!
//! The batch's membership effect is replayed as the policy's own changes
//! (`Change`): removals and bans first, then admissions and roles, then
//! grants. Each must be allowed to the writer, and together they must give
//! exactly the membership the batch wrote. Anything else the batch changes in
//! governance is the owner's.

use std::collections::BTreeMap;

use automerge::AutoCommit;
use tilo_action_policy::{
    Change, Context, Grant, GrantId, Role, RoomPolicy, roles, roles::governance,
};

use super::{RoomGovernance, governance_of};
use crate::state::GovernedIdentity;

/// Why a member's membership batch was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipRefusal {
    /// The batch changes more of governance than membership.
    NotMembership,
    /// Its dependencies are not all held, or it does not apply on them.
    Position(String),
    /// A stored role, grant or identity does not decode.
    Malformed(String),
    /// One of the changes is not the writer's to make.
    Refused(String),
    /// The changes the writer may make do not give what the batch wrote.
    Mismatch,
}

impl std::fmt::Display for MembershipRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMembership => f.write_str("governance writer is not the owner's device"),
            Self::Position(error) => write!(f, "membership change does not apply: {error}"),
            Self::Malformed(error) => write!(f, "membership change is malformed: {error}"),
            Self::Refused(error) => write!(f, "membership change refused: {error}"),
            Self::Mismatch => f.write_str("membership change is not what its actions make"),
        }
    }
}

/// The room as `changes`' writer saw it, and with the changes on it: `before`
/// at the dependencies the batch names outside itself.
///
/// # Errors
/// A dependency is not in `before`, or the batch does not apply.
pub fn position(
    before: &AutoCommit,
    changes: &[Vec<u8>],
) -> Result<(AutoCommit, AutoCommit), MembershipRefusal> {
    let decoded = changes
        .iter()
        .map(|bytes| automerge::Change::from_bytes(bytes.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MembershipRefusal::Position(error.to_string()))?;
    let own: std::collections::BTreeSet<_> = decoded.iter().map(automerge::Change::hash).collect();
    let mut heads: Vec<_> = decoded
        .iter()
        .flat_map(|change| change.deps().iter().copied())
        .filter(|dep| !own.contains(dep))
        .collect();
    heads.sort();
    heads.dedup();
    let mut at = before.clone();
    let at = at
        .fork_at(&heads)
        .map_err(|error| MembershipRefusal::Position(error.to_string()))?;
    let after = super::candidate(Some(&at), changes).map_err(MembershipRefusal::Position)?;
    Ok((at, after))
}

/// Whether `person`, from a device of theirs (`foreign` when it is not),
/// may make the membership change from `at` to `after` in `room`.
///
/// # Errors
/// The first refusal that applies.
pub fn authorize(
    room: &str,
    person: &[u8; 32],
    foreign: bool,
    at: &AutoCommit,
    after: &AutoCommit,
    now_ns: u64,
) -> Result<(), MembershipRefusal> {
    let before = governance_of(Some(at));
    let written = governance_of(Some(after));
    if !only_membership_differs(&before, &written)
        || crate::durability::governed_bytes(at) != crate::durability::governed_bytes(after)
        || crate::document_import::governed_bytes(Some(at))
            != crate::document_import::governed_bytes(Some(after))
        || super::settings_of(Some(at)) != super::settings_of(Some(after))
    {
        return Err(MembershipRefusal::NotMembership);
    }
    let held = tilo_desktop_model::projection::room_policy_of(at, room);
    let wanted = tilo_desktop_model::projection::room_policy_of(after, room);
    let actor = hex::encode(person);
    let bound = roles::foreign_device_bound();
    let authority = held.person_on_device(&actor, foreign, &bound);
    let cx = Context::at_secs(now_ns / 1_000_000_000);
    let mut replay = held.clone();
    for change in changes(&held, &wanted) {
        replay
            .apply(&actor, &authority, change, &cx, false)
            .map_err(|refusal| MembershipRefusal::Refused(refusal.to_string()))?;
    }
    if replay.members != wanted.members || sorted(&replay.grants) != sorted(&wanted.grants) {
        return Err(MembershipRefusal::Mismatch);
    }
    identities(&held, &wanted, &actor, &authority, &cx, &before, &written)
}

/// Every governance part but membership is the same in both.
fn only_membership_differs(before: &RoomGovernance, written: &RoomGovernance) -> bool {
    before.owner == written.owner
        && before.sandbox == written.sandbox
        && before.coverage == written.coverage
        && before.media == written.media
}

/// The policy changes that take `held` to `wanted`, in the order they apply:
/// removals and bans, admissions and roles, then the grants left to change.
fn changes(held: &RoomPolicy, wanted: &RoomPolicy) -> Vec<Change> {
    let wanted_grants: BTreeMap<&GrantId, &Grant> = wanted
        .grants
        .iter()
        .map(|grant| (&grant.id, grant))
        .collect();
    let mut out = Vec::new();
    let mut banned = Vec::new();
    for person in held
        .members
        .keys()
        .filter(|person| !wanted.members.contains_key(*person))
    {
        if wanted_grants.contains_key(&roles::block_grant_id(person)) {
            banned.push(roles::block_grant_id(person));
            out.push(Change::Ban {
                person: person.clone(),
            });
        } else {
            out.push(Change::RemoveMember {
                person: person.clone(),
            });
        }
    }
    for (person, role) in &wanted.members {
        match held.members.get(person) {
            None => out.push(Change::AddMember {
                person: person.clone(),
                role: *role,
            }),
            Some(held_role) if held_role != role => out.push(Change::SetRole {
                person: person.clone(),
                role: *role,
            }),
            Some(_) => {}
        }
    }
    // What the removals above take with them is not changed again.
    let mut remaining = held.clone();
    for change in &out {
        if let Change::RemoveMember { person } | Change::Ban { person } = change {
            let subject = tilo_action_policy::Subject::Person(person.clone());
            remaining.grants.retain(|grant| grant.subject != subject);
        }
    }
    let held_grants: BTreeMap<&GrantId, &Grant> = remaining
        .grants
        .iter()
        .map(|grant| (&grant.id, grant))
        .collect();
    for id in held_grants.keys() {
        if wanted_grants.contains_key(id) {
            continue;
        }
        out.push(match grant_person(id, "block:") {
            Some(person) => Change::Unban { person },
            None => match grant_person(id, "restrict:") {
                Some(person) => Change::Unrestrict { person },
                None => Change::Revoke((*id).clone()),
            },
        });
    }
    for (id, grant) in &wanted_grants {
        if held_grants.get(id) == Some(grant) || banned.contains(id) {
            continue;
        }
        if let Some(person) = grant_person(id, "restrict:")
            && let tilo_action_policy::Lifetime::Until(until_ms) = grant.lifetime
        {
            out.push(Change::Restrict { person, until_ms });
            continue;
        }
        if held_grants.contains_key(id) {
            out.push(Change::Revoke((*id).clone()));
        }
        out.push(Change::Grant((*grant).clone()));
    }
    out
}

/// The person a moderation grant `id` of the kind `prefix` names.
fn grant_person(id: &GrantId, prefix: &str) -> Option<String> {
    let person = id.as_str().strip_prefix(prefix)?;
    let named = match prefix {
        "block:" => roles::block_grant_id(person),
        _ => roles::restriction_grant_id(person),
    };
    (&named == id).then(|| person.to_owned())
}

fn sorted(grants: &[Grant]) -> Vec<&Grant> {
    let mut grants: Vec<&Grant> = grants.iter().collect();
    grants.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    grants
}

/// The devices the batch admits or removes: each a device of a member of
/// the room as written, and the writer's own or one they may admit.
fn identities(
    held: &RoomPolicy,
    wanted: &RoomPolicy,
    actor: &str,
    authority: &tilo_action_policy::Authority<'_>,
    cx: &Context<'_>,
    before: &RoomGovernance,
    written: &RoomGovernance,
) -> Result<(), MembershipRefusal> {
    let person_of = |bytes: &[u8]| {
        serde_json::from_slice::<GovernedIdentity>(bytes)
            .map(|identity| hex::encode(identity.device_cert.user_public_key))
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))
    };
    let may = |action: &str| held.check_action(authority, action, cx, false).is_ok();
    for (device, bytes) in &written.identities {
        if before.identities.get(device) == Some(bytes) {
            continue;
        }
        let person = person_of(bytes)?;
        let identity: GovernedIdentity = serde_json::from_slice(bytes)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        tilo_user_identity::verify_device_cert(&identity.device_cert)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        if wanted.role_of(&person).is_none() {
            return Err(MembershipRefusal::Refused(format!(
                "device {device} is not of a member"
            )));
        }
        if person != actor && !may(governance::ADD_MEMBER) && !may(governance::INVITE) {
            return Err(MembershipRefusal::Refused(format!(
                "device {device} is not the writer's to admit"
            )));
        }
    }
    for (device, bytes) in &before.identities {
        if written.identities.contains_key(device) {
            continue;
        }
        let person = person_of(bytes)?;
        if person != actor && !may(governance::REMOVE_MEMBER) {
            return Err(MembershipRefusal::Refused(format!(
                "device {device} is not the writer's to remove"
            )));
        }
    }
    // An owner's role is the room's, never a membership change.
    if wanted.members.values().any(|role| *role == Role::Owner)
        && held.members.values().all(|role| *role != Role::Owner)
    {
        return Err(MembershipRefusal::Refused("the owner's role".into()));
    }
    Ok(())
}
