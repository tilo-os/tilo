//! Governance changes a member's device writes itself.
//!
//! A room's members, their roles, the grants on it, the devices each member
//! holds it on and its own settings (its name, saved grant sets and its
//! apps' sandbox capabilities) are changed by actions, under the action
//! policy (`tilo_action_policy::RoomPolicy`). The owner's devices write
//! every part of the room's governance; anyone else writes only these, and
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
//! exactly the membership the batch wrote. Grants a saved grant set expands
//! to are its assigner's when they may assign that set ([`assignments`]).
//! The room's settings are checked each by the action that governs it
//! ([`settings`], [`sandbox`]). A share's grants are its sharer's, given
//! and ended as [`super::share`] decides. Anything else the batch changes in
//! governance is the owner's.
//!
//! An admission through an invitation is the inviter's: any participant that
//! may invite writes it, with the joiner's device and the redemption receipt
//! carrying the signed invitation, and every receiver checks that invitation
//! itself ([`invitation_admits`]). A device certified by a member's root is
//! that member's device: any participant writes it in.

use std::collections::BTreeMap;

use automerge::AutoCommit;
use tilo_action_policy::{
    Change, Context, Grant, GrantId, Role, RoomPolicy, roles, roles::governance,
};

use super::{RoomGovernance, governance_of};
use crate::state::{GovernedIdentity, RedemptionReceipt};

/// How far ahead of a receiver's clock an admission may say it was made:
/// devices' clocks differ, and a receiver judges an invitation's expiry at
/// the admission's own time so every receiver, early or late, agrees.
const ADMISSION_CLOCK_SKEW_NS: u64 = 5 * 60 * 1_000_000_000;

/// Why a member's governance batch was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipRefusal {
    /// The batch changes more of governance than a member writes.
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

/// Whether `person`, from their device `device` (hex; `foreign` when it is
/// not one of theirs), may make the governance change from `at` to `after`
/// in `room`.
///
/// # Errors
/// The first refusal that applies.
pub fn authorize(
    room: &str,
    (person, device, foreign): (&[u8; 32], &str, bool),
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
    {
        return Err(MembershipRefusal::NotMembership);
    }
    let held = tilo_desktop_model::projection::room_policy_of(at, room);
    let wanted = tilo_desktop_model::projection::room_policy_of(after, room);
    let actor = hex::encode(person);
    let bound = roles::foreign_device_bound();
    let authority = held.person_on_device(&actor, foreign, &bound);
    let cx = Context::at_secs(now_ns / 1_000_000_000);
    let writer = (actor.as_str(), &authority, &cx);
    settings(room, at, after, &held, writer)?;
    sandbox(&before, &written, &held, writer)?;
    let invited = redemptions(room, &held, &wanted, &before, &written, now_ns)?;
    if !invited.is_empty() {
        held.check_action(&authority, governance::INVITE, &cx, false)
            .map_err(|refusal| MembershipRefusal::Refused(refusal.to_string()))?;
    }
    let assigned = assignments(room, at, &held, &wanted, writer)?;
    let (shared, unshared) = super::share::changed(
        (at, &held, &wanted),
        (actor.as_str(), device, &authority, &cx),
    )
    .map_err(MembershipRefusal::Refused)?;
    // What the invitations admit is the inviters', a set's grants its
    // assigner's and a share's its sharer's; the rest is the writer's.
    let mut replay = held.clone();
    for (person, (role, _)) in &invited {
        replay.members.insert(person.clone(), *role);
    }
    replay.grants.retain(|grant| {
        !unshared.contains(&grant.id) && !shared.iter().any(|given| given.id == grant.id)
    });
    replay.grants.extend(assigned);
    replay.grants.extend(shared);
    let base = replay.clone();
    for change in changes(&base, &wanted) {
        replay
            .apply(&actor, &authority, change, &cx, false)
            .map_err(|refusal| MembershipRefusal::Refused(refusal.to_string()))?;
    }
    if replay.members != wanted.members || sorted(&replay.grants) != sorted(&wanted.grants) {
        return Err(MembershipRefusal::Mismatch);
    }
    identities(
        &held,
        &wanted,
        &invited,
        (&actor, &authority, &cx),
        (&before, &written),
    )
}

/// The invitation `signed`, redeemed by `joiner` at `at_ns`, when it admits
/// them to `room` as `policy` stands: it verifies and has not expired then,
/// names this room and, when narrowed, the joiner, and its issuer is a member
/// who may invite with its role.
///
/// # Errors
/// The first of those that does not hold.
pub fn invitation_admits(
    policy: &tilo_action_policy::RoomPolicy,
    room: &str,
    signed: &[u8],
    joiner: [u8; 32],
    at_ns: u64,
) -> Result<crate::join::InviteGrant, MembershipRefusal> {
    let refused = |error: String| MembershipRefusal::Refused(format!("invitation: {error}"));
    let join = crate::join::decode_and_verify_invitation(
        signed,
        Some(joiner),
        &tilo_user_identity::RevocationList::new(),
        at_ns,
    )
    .map_err(refused)?;
    let grant = join.grant().clone();
    if grant.room != room {
        return Err(refused("it is for another room".into()));
    }
    let issuer = hex::encode(grant.issuer.user_public_key);
    let bound = roles::foreign_device_bound();
    // The owner grants itself anything, from any device of theirs, so the
    // foreign-device bound does not narrow what they invite.
    let foreign = grant.issuer.capabilities.foreign && !policy.is_owner(&issuer);
    let authority = policy.person_on_device(&issuer, foreign, &bound);
    policy
        .check_invite(
            &issuer,
            &authority,
            grant.role.into(),
            &Context::at_secs(at_ns / 1_000_000_000),
            false,
        )
        .map_err(|refusal| refused(format!("its issuer may not invite: {refusal}")))?;
    Ok(grant)
}

/// The people the batch admits through invitations, each with the role
/// their invitation gives and the device that redeemed it. Every receipt
/// the batch adds to the room's ledger must be one: the invitation it
/// carries admits its joiner at the time the room records them joining
/// ([`invitation_admits`]), the joiner's device row holds the receipt, and
/// the room gives them its role. A receipt the room holds stays as it is,
/// so a spent invitation stays spent.
fn redemptions(
    room: &str,
    held: &RoomPolicy,
    wanted: &RoomPolicy,
    before: &RoomGovernance,
    written: &RoomGovernance,
    now_ns: u64,
) -> Result<BTreeMap<String, (Role, String)>, MembershipRefusal> {
    let malformed = |error: String| MembershipRefusal::Malformed(error);
    for (invite, values) in &before.invitation_receipts {
        let kept = written.invitation_receipts.get(invite);
        if values
            .iter()
            .any(|value| !kept.is_some_and(|kept| kept.contains(value)))
        {
            return Err(MembershipRefusal::Refused(format!(
                "the redemption of invitation {invite} is the room's record"
            )));
        }
    }
    let mut invited = BTreeMap::new();
    for (invite, values) in &written.invitation_receipts {
        let held_values = before.invitation_receipts.get(invite);
        for value in values {
            if held_values.is_some_and(|held| held.contains(value)) {
                continue;
            }
            let receipt: RedemptionReceipt =
                serde_json::from_slice(value).map_err(|error| malformed(error.to_string()))?;
            if *invite != hex::encode(receipt.invite_id) {
                return Err(malformed("an invitation receipt under another key".into()));
            }
            let signed = receipt
                .invitation
                .as_deref()
                .ok_or_else(|| malformed("an invitation receipt without its invitation".into()))
                .and_then(|text| {
                    use base64::Engine as _;
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(text)
                        .map_err(|error| malformed(error.to_string()))
                })?;
            if *blake3::hash(&signed).as_bytes() != receipt.invite_id {
                return Err(malformed(
                    "an invitation receipt names another invitation".into(),
                ));
            }
            let person = hex::encode(receipt.user_public_key);
            let joined_ms = wanted.joined_at(&person).ok_or_else(|| {
                MembershipRefusal::Refused(format!("{person} is admitted at no recorded time"))
            })?;
            let at_ns = joined_ms.saturating_mul(1_000_000);
            if at_ns > now_ns.saturating_add(ADMISSION_CLOCK_SKEW_NS) {
                return Err(MembershipRefusal::Refused(format!(
                    "{person} is admitted in the future"
                )));
            }
            let grant = invitation_admits(held, room, &signed, receipt.user_public_key, at_ns)?;
            let role: Role = grant.role.into();
            if grant.role != receipt.role || wanted.role_of(&person) != Some(role) {
                return Err(MembershipRefusal::Refused(format!(
                    "{person} does not take the role their invitation gives"
                )));
            }
            if held.is_banned(&person) || held.is_owner(&person) {
                return Err(MembershipRefusal::Refused(format!(
                    "{person} is not admitted through an invitation"
                )));
            }
            let device = hex::encode(receipt.device_public_key);
            let row = written
                .identities
                .get(&device)
                .map(|bytes| serde_json::from_slice::<GovernedIdentity>(bytes))
                .transpose()
                .map_err(|error| malformed(error.to_string()))?;
            let on_row = receipt.on_row();
            if !row.is_some_and(|row| {
                row.device_cert.user_public_key == receipt.user_public_key
                    && row
                        .admission
                        .iter()
                        .chain(&row.admissions)
                        .any(|kept| *kept == on_row)
            }) {
                return Err(MembershipRefusal::Refused(format!(
                    "device {device} does not hold its redemption"
                )));
            }
            invited.insert(person, (role, device));
        }
    }
    Ok(invited)
}

/// Every governance part a member never writes is the same in both.
fn only_membership_differs(before: &RoomGovernance, written: &RoomGovernance) -> bool {
    before.owner == written.owner
        && before.coverage == written.coverage
        && before.media == written.media
}

/// The writer of a batch: their person, their authority on the device that
/// wrote it, and when.
type Writer<'a> = (
    &'a str,
    &'a tilo_action_policy::Authority<'a>,
    &'a Context<'a>,
);

/// The room's name and saved grant sets, as the batch changes them: the name
/// a valid one, given by someone who governs the room (a Moderator or above,
/// or one granted its settings) when it shares its members' layout, as a room
/// behind a public door does not; the grant sets by one granted its settings.
fn settings(
    room: &str,
    at: &AutoCommit,
    after: &AutoCommit,
    held: &RoomPolicy,
    (actor, authority, cx): Writer<'_>,
) -> Result<(), MembershipRefusal> {
    use tilo_desktop_model::projection::{ROOM_NAME_KEY, RoomLayout, room_layout};
    let parts = |doc: &AutoCommit| {
        super::settings_of(Some(doc))
            .into_iter()
            .partition::<Vec<_>, _>(|(key, _)| key == ROOM_NAME_KEY)
    };
    let (held_name, held_presets) = parts(at);
    let (name, presets) = parts(after);
    let settles = held
        .check_action(authority, governance::SETTINGS, cx, false)
        .is_ok();
    if held_name != name {
        let governs = held
            .active_role(actor)
            .is_some_and(|role| role >= Role::Moderator);
        if !(governs || settles) || room_layout(at) == RoomLayout::Personal {
            return Err(MembershipRefusal::Refused(format!(
                "renaming room {room} is not the writer's"
            )));
        }
        let named = match automerge::ReadDoc::get(after, automerge::ROOT, ROOM_NAME_KEY) {
            Ok(Some((automerge::Value::Scalar(value), _))) => match value.as_ref() {
                automerge::ScalarValue::Str(name) => Some(name.to_string()),
                _ => None,
            },
            _ => None,
        };
        named
            .as_deref()
            .ok_or_else(|| "the room's name is not text".to_owned())
            .and_then(|name| crate::workspace_membership::valid_room_name(name).map(drop))
            .map_err(MembershipRefusal::Malformed)?;
    }
    if held_presets != presets && !settles {
        return Err(MembershipRefusal::Refused(
            "the room's grant sets are not the writer's to change".into(),
        ));
    }
    Ok(())
}

/// The sandbox capabilities the batch records or takes back: each recorded
/// one lasting past a restart and recorded as the writer's, who may grant in
/// the room; each taken back by one who may revoke there.
fn sandbox(
    before: &RoomGovernance,
    written: &RoomGovernance,
    held: &RoomPolicy,
    (actor, authority, cx): Writer<'_>,
) -> Result<(), MembershipRefusal> {
    let may = |action: &str| {
        held.check_action(authority, action, cx, false)
            .map_err(|refusal| MembershipRefusal::Refused(refusal.to_string()))
    };
    for (id, bytes) in &written.sandbox {
        if before.sandbox.get(id) == Some(bytes) {
            continue;
        }
        may(governance::GRANT)?;
        let capability: collab_types::caps::CapOverride = serde_json::from_slice(bytes)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        if !capability.persists_across_restart() {
            return Err(MembershipRefusal::Refused(format!(
                "sandbox capability {id} ends with a session or a single use"
            )));
        }
        if capability.granted_by != Some(collab_types::caps::Principal::Person(actor.to_owned())) {
            return Err(MembershipRefusal::Refused(format!(
                "sandbox capability {id} is not recorded as the writer's"
            )));
        }
    }
    if before
        .sandbox
        .keys()
        .any(|id| !written.sandbox.contains_key(id))
    {
        may(governance::REVOKE)?;
    }
    Ok(())
}

/// The grants the batch adds that assign one of the room's saved grant sets
/// (`preset:<id>:<set>:<rule>`), written by someone who may assign that set
/// by a grant on it ([`tilo_desktop_model::preset_assign_node`]) and given
/// by them: each one of the set's rules placed in this room, no more than
/// they hold. Any other grant is left to the room's grant rule.
fn assignments(
    room: &str,
    at: &AutoCommit,
    held: &RoomPolicy,
    wanted: &RoomPolicy,
    (actor, authority, cx): Writer<'_>,
) -> Result<Vec<Grant>, MembershipRefusal> {
    let presets = tilo_desktop_model::projection::workspace_presets_of(at);
    let room_node = tilo_action_policy::TreePath::room(room);
    let mut assigned = Vec::new();
    for grant in &wanted.grants {
        if held.grants.iter().any(|kept| kept.id == grant.id) {
            continue;
        }
        let Some((set, index)) = grant
            .id
            .as_str()
            .strip_prefix("preset:")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(_, rest)| rest.rsplit_once(':'))
        else {
            continue;
        };
        let Some(preset) = presets
            .iter()
            .find(|preset| preset.name.trim().to_lowercase() == set)
        else {
            continue;
        };
        let on_set = authority
            .decide(
                &tilo_desktop_model::preset_assign_node(room, &preset.name),
                roles::GOVERNANCE_KIND,
                cx,
            )
            .answer
            == tilo_action_policy::Answer::Allow;
        if !on_set {
            continue;
        }
        let rule = index
            .parse::<usize>()
            .ok()
            .and_then(|index| preset.rules.get(index));
        if !rule.is_some_and(|rule| {
            grant.node == room_node.join(&rule.node)
                && grant.kinds == rule.kinds
                && grant.answer == rule.answer
        }) || grant.granted_by != tilo_action_policy::Subject::Person(actor.to_owned())
        {
            return Err(MembershipRefusal::Refused(format!(
                "grant {} is not of the set it names",
                grant.id.as_str()
            )));
        }
        RoomPolicy::check_within(authority, grant, cx)
            .map_err(|refusal| MembershipRefusal::Refused(refusal.to_string()))?;
        assigned.push(grant.clone());
    }
    Ok(assigned)
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
/// the room as written, and the writer's own, one an invitation admits, a
/// further device of someone already a member, or one they may admit.
fn identities(
    held: &RoomPolicy,
    wanted: &RoomPolicy,
    invited: &BTreeMap<String, (Role, String)>,
    (actor, authority, cx): (&str, &tilo_action_policy::Authority<'_>, &Context<'_>),
    (before, written): (&RoomGovernance, &RoomGovernance),
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
        let redeemed = invited
            .get(&person)
            .is_some_and(|(_, redeemer)| redeemer == device);
        // A device certified by a member's root is that member's: any
        // participant writes it in when it first asks.
        let further = !before.identities.contains_key(device)
            && held.role_of(&person).is_some()
            && identity.admission.is_none()
            && identity.admissions.is_empty()
            && identity.delegate.is_none()
            && identity.workspace_delegations.is_empty();
        if person != actor
            && !redeemed
            && !further
            && !may(governance::ADD_MEMBER)
            && !may(governance::INVITE)
        {
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
