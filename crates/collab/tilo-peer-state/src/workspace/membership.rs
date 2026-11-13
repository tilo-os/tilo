//! Governance changes a member's device writes itself, beyond the membership.
//!
//! Members, roles, grants and shares are the membership operations' effect
//! ([`crate::members`]). The rest of a workspace's governance (the devices
//! each member holds it on, its name and saved grant sets, its apps' sandbox
//! capabilities, its media policy, checkpoints and hosts) is changed by a batch
//! that carries an operation naming each part it changes
//! ([`admit_operations`]). Each part is judged by the action that governs it,
//! against its writer's role where they made the batch: a role holds what it
//! is given, so the owner's role holds every part by default, and anyone else
//! writes what their role and grants let them ([`settings`], [`sandbox`]).
//!
//! An admission through an invitation is the inviter's: any participant that
//! may invite writes it, with the joiner's device and the redemption receipt
//! carrying the signed invitation, and every receiver checks that invitation
//! itself ([`invitation_admits`]). A device certified by a member's root is
//! that member's device: any participant writes it in.

use std::collections::BTreeMap;

use automerge::AutoCommit;
use tilo_action_policy::{Context, Role, WorkspacePolicy, roles, roles::governance};

use super::{GovernanceRow, WorkspaceGovernance, governance_of};
use crate::members::{MembershipOpCommand, SignedMembershipOp};
use crate::state::GovernedIdentity;

/// How far ahead of a receiver's clock an admission may say it was made:
/// devices' clocks differ, and a receiver judges an invitation's expiry at
/// the admission's own time so every receiver, early or late, agrees.
const ADMISSION_CLOCK_SKEW_NS: u64 = 5 * 60 * 1_000_000_000;

/// Why a member's governance batch was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipRefusal {
    /// The batch changes governance its writer's operations do not name.
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
            Self::NotMembership => {
                f.write_str("governance changed beyond what its writer's operations name")
            }
            Self::Position(error) => write!(f, "membership change does not apply: {error}"),
            Self::Malformed(error) => write!(f, "membership change is malformed: {error}"),
            Self::Refused(error) => write!(f, "membership change refused: {error}"),
            Self::Mismatch => f.write_str("membership change is not what its actions make"),
        }
    }
}

/// The workspace as `changes`' writer saw it: `before` at the dependencies the
/// batch names outside itself.
///
/// A batch may carry changes `before` already holds — a delta that reaches a
/// device again, or one written on another's — and those describe where the
/// history stood before, not where the writer stood: where the writer made the
/// changes it adds is what their dependencies name, followed through the
/// batch's own changes back to the ones `before` holds. A batch that adds
/// nothing is judged where `before` stands.
///
/// # Errors
/// A dependency is not in `before`, or the changes do not decode.
pub fn dependency_state(
    before: &AutoCommit,
    changes: &[Vec<u8>],
) -> Result<AutoCommit, MembershipRefusal> {
    use automerge::ReadDoc as _;
    let decoded = changes
        .iter()
        .map(|bytes| automerge::Change::from_bytes(bytes.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MembershipRefusal::Position(error.to_string()))?;
    let own: BTreeMap<_, _> = decoded
        .iter()
        .map(|change| (change.hash(), change))
        .collect();
    let mut heads: Vec<_> = Vec::new();
    let mut walked: std::collections::BTreeSet<_> = std::collections::BTreeSet::new();
    let mut pending: Vec<&automerge::Change> = decoded
        .iter()
        .filter(|change| before.get_change_by_hash(&change.hash()).is_none())
        .collect();
    if pending.is_empty() {
        return Ok(before.clone());
    }
    while let Some(change) = pending.pop() {
        for dep in change.deps() {
            if before.get_change_by_hash(dep).is_some() {
                heads.push(*dep);
            } else if let Some(parent) = own.get(dep) {
                if walked.insert(*dep) {
                    pending.push(parent);
                }
            } else {
                heads.push(*dep);
            }
        }
    }
    heads.sort();
    heads.dedup();
    let mut before = before.clone();
    before
        .fork_at(&heads)
        .map_err(|error| MembershipRefusal::Position(error.to_string()))
}

/// The workspace as `changes`' writer saw it, and with the changes on it: `before`
/// at the dependencies the batch names outside itself.
///
/// # Errors
/// A dependency is not in `before`, or the batch does not apply.
pub fn position(
    before: &AutoCommit,
    changes: &[Vec<u8>],
) -> Result<(AutoCommit, AutoCommit), MembershipRefusal> {
    let at = dependency_state(before, changes)?;
    let after = super::candidate(Some(&at), changes).map_err(MembershipRefusal::Position)?;
    Ok((at, after))
}

/// Whether a batch that adds operations changes only what they admit: every
/// governed part it changes is one an operation it adds names, and its
/// author's role permits it.
///
/// # Errors
/// A part is changed without an operation naming it, or by an author whose role
/// does not permit it.
pub fn admit_operations(
    before: Option<&AutoCommit>,
    candidate: &AutoCommit,
    changes: &[Vec<u8>],
    workspace: &str,
    now_ns: u64,
) -> Result<(), MembershipRefusal> {
    // A workspace this device does not hold yet is its governor's to send: the
    // operations of a first answer from anyone else govern nothing here.
    let Some(before) = before else {
        return Err(MembershipRefusal::NotMembership);
    };
    let (at, after) = position(before, changes)?;
    let mut held_rows = super::changed_rows(Some(before), Some(candidate), false);
    held_rows.sort_unstable();
    let mut declared: Vec<GovernanceRow> = Vec::new();
    let mut admitted = false;
    for op in added_ops(Some(before), candidate) {
        let rows: &[GovernanceRow] = match &op.command {
            MembershipOpCommand::Governance { rows } => rows,
            // An admission writes the device it admits, and names that row
            // itself: each admission of a batch writes its own device.
            MembershipOpCommand::Admit { .. } => {
                if held_rows.contains(&GovernanceRow::Identities) {
                    let identities = [GovernanceRow::Identities];
                    rows_permitted(workspace, (&at, &after), &op, &identities, now_ns)?;
                    admitted = true;
                }
                continue;
            }
            MembershipOpCommand::Project { .. } => continue,
        };
        for row in rows {
            if declared.contains(row) {
                return Err(MembershipRefusal::Refused(format!(
                    "{} is changed twice",
                    row_name(*row)
                )));
            }
            declared.push(*row);
        }
        rows_permitted(workspace, (&at, &after), &op, rows, now_ns)?;
    }
    if admitted && !declared.contains(&GovernanceRow::Identities) {
        declared.push(GovernanceRow::Identities);
    }
    declared.sort_unstable();
    if declared != held_rows {
        return Err(MembershipRefusal::NotMembership);
    }
    Ok(())
}

/// The operations `candidate` adds to the ones `before` holds.
fn added_ops(before: Option<&AutoCommit>, candidate: &AutoCommit) -> Vec<SignedMembershipOp> {
    let held = before.map_or_else(BTreeMap::new, crate::members::ops);
    crate::members::ops(candidate)
        .into_iter()
        .filter(|(id, _)| !held.contains_key(id))
        .map(|(_, op)| std::sync::Arc::unwrap_or_clone(op))
        .collect()
}

/// Whether `op`'s author may change `rows`, from `at` to `after`.
fn rows_permitted(
    workspace: &str,
    (at, after): (&AutoCommit, &AutoCommit),
    op: &SignedMembershipOp,
    rows: &[GovernanceRow],
    now_ns: u64,
) -> Result<(), MembershipRefusal> {
    let held = crate::workspace::policy_of(at, workspace);
    let wanted = crate::workspace::policy_of(after, workspace);
    let actor = op.person();
    let foreign = op.author.capabilities.foreign && !held.is_owner(&actor);
    let bound = roles::foreign_device_bound();
    let authority = held.person_on_device(&actor, foreign, bound);
    let cx = Context::at(op.at_ns / 1_000_000);
    let writer = (actor.as_str(), &authority, &cx);
    let (before, written) = (governance_of(Some(at)), governance_of(Some(after)));
    for row in rows {
        if let Some(action) = row.action() {
            held.check_action(&authority, action, &cx, false)
                .map_err(|refusal| {
                    MembershipRefusal::Refused(format!(
                        "{} takes {action}: {refusal}",
                        row_name(*row)
                    ))
                })?;
            continue;
        }
        match row {
            GovernanceRow::Settings => settings(workspace, at, after, &held, writer)?,
            GovernanceRow::Sandbox => sandbox(&before, &written, &held, writer)?,
            GovernanceRow::Identities => {
                let invited = redemptions(workspace, &held, &wanted, &before, &written, now_ns)?;
                identities(&held, &wanted, &invited, writer, (&before, &written))?;
            }
            _ => {
                return Err(MembershipRefusal::Refused(format!(
                    "{} is not a change an operation makes",
                    row_name(*row)
                )));
            }
        }
    }
    Ok(())
}

/// The name a receiver's refusal gives `row`.
fn row_name(row: GovernanceRow) -> &'static str {
    match row {
        GovernanceRow::Owner => "the workspace's owner",
        GovernanceRow::Membership => "the workspace's membership",
        GovernanceRow::Identities => "the devices of the workspace",
        GovernanceRow::Settings => "the workspace's settings",
        GovernanceRow::Media => "the media policy",
        GovernanceRow::Versions => "the app versions the workspace runs",
        GovernanceRow::Bots => "the workspace's bots",
        GovernanceRow::Checkpoints => "the workspace's checkpoints",
        GovernanceRow::Hosts => "the workspace's hosts",
        GovernanceRow::Log => "the workspace's operations",
        GovernanceRow::Sandbox => "the capabilities of its apps' sandboxes",
    }
}

/// The invitation `signed`, redeemed by `joiner` at `at_ns`, when it admits
/// them to `workspace` as `policy` stands: it verifies and has not expired then,
/// names this workspace and, when narrowed, the joiner, and its issuer is a member
/// who may invite with its role.
pub fn invitation_admits(
    policy: &tilo_action_policy::WorkspacePolicy,
    workspace: &str,
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
    if grant.room != workspace {
        return Err(refused("it is for another workspace".into()));
    }
    let issuer = hex::encode(grant.issuer.user_public_key);
    let bound = roles::foreign_device_bound();
    // The owner grants itself anything, from any device of theirs, so the
    // foreign-device bound does not narrow what they invite.
    let foreign = grant.issuer.capabilities.foreign && !policy.is_owner(&issuer);
    let authority = policy.person_on_device(&issuer, foreign, bound);
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
/// the batch adds to the workspace's ledger must be one: the invitation it
/// carries admits its joiner at the time the workspace records them joining
/// ([`invitation_admits`]), the joiner's device row holds the receipt, and
/// the workspace gives them its role. Every invitation part the batch adds
/// is kept under its own digest and completes a receipt it adds. That an
/// invitation is redeemed once, and a part once kept stays, is every
/// writer's rule, checked for the whole candidate
/// ([`super::redemptions_kept`]).
fn redemptions(
    workspace: &str,
    held: &WorkspacePolicy,
    wanted: &WorkspacePolicy,
    before: &WorkspaceGovernance,
    written: &WorkspaceGovernance,
    now_ns: u64,
) -> Result<BTreeMap<String, (Role, String)>, MembershipRefusal> {
    use crate::state::invitation_ledger::{part_in, part_key};
    let malformed = |error: String| MembershipRefusal::Malformed(error);
    let mut invited = BTreeMap::new();
    // The parts the batch adds, until a receipt it adds names them.
    let mut new_parts = std::collections::BTreeSet::new();
    for (key, values) in &written.invitation_parts {
        let held = before.invitation_parts.get(key);
        for value in values {
            if held.is_some_and(|held| held.contains(value)) {
                continue;
            }
            if *key != part_key(blake3::hash(value).as_bytes()) {
                return Err(malformed("an invitation part under another key".into()));
            }
            new_parts.insert(key.clone());
        }
    }
    for (invite, values) in &written.invitation_receipts {
        let held_values = before.invitation_receipts.get(invite);
        for value in values {
            if held_values.is_some_and(|held| held.contains(value)) {
                continue;
            }
            let ledgered = crate::state::LedgerReceipt::decode(value)
                .map_err(|error| malformed(error.to_string()))?;
            let receipt = &ledgered.receipt;
            if *invite != hex::encode(receipt.invite_id) {
                return Err(malformed("an invitation receipt under another key".into()));
            }
            let signed = ledgered
                .invitation(|digest| part_in(&written.invitation_parts, digest))
                .ok_or_else(|| {
                    malformed("an invitation receipt names no invitation it holds".into())
                })?;
            for digest in ledgered.shared() {
                new_parts.remove(&part_key(digest));
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
            let grant =
                invitation_admits(held, workspace, &signed, receipt.user_public_key, at_ns)?;
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
                .map(|bytes| GovernedIdentity::decode(bytes))
                .transpose()
                .map_err(|error| malformed(error.to_string()))?;
            if !row.is_some_and(|row| {
                row.device_cert.user_public_key == receipt.user_public_key
                    && row
                        .admission
                        .iter()
                        .chain(&row.admissions)
                        .any(|kept| kept == receipt)
            }) {
                return Err(MembershipRefusal::Refused(format!(
                    "device {device} does not hold its redemption"
                )));
            }
            invited.insert(person, (role, device));
        }
    }
    if !new_parts.is_empty() {
        return Err(malformed(
            "an invitation part no receipt of the batch names".into(),
        ));
    }
    Ok(invited)
}

/// The writer of a batch: their person, their authority on the device that
/// wrote it, and when.
type Writer<'a> = (
    &'a str,
    &'a tilo_action_policy::Authority<'a>,
    &'a Context<'a>,
);

/// The workspace's name and its settings — its saved grant sets and its door —
/// as the batch changes them: the name a valid one, given by someone who
/// governs the workspace (a Moderator or above, or one granted its settings);
/// the settings by one granted its settings.
fn settings(
    workspace: &str,
    at: &AutoCommit,
    after: &AutoCommit,
    held: &WorkspacePolicy,
    (actor, authority, cx): Writer<'_>,
) -> Result<(), MembershipRefusal> {
    use tilo_desktop_model::projection::WORKSPACE_NAME_KEY;
    let parts = |doc: &AutoCommit| {
        super::settings_of(Some(doc))
            .into_iter()
            .partition::<Vec<_>, _>(|(key, _)| key == WORKSPACE_NAME_KEY)
    };
    let (held_name, held_settings) = parts(at);
    let (name, settings) = parts(after);
    let settles = held
        .check_action(authority, governance::SETTINGS, cx, false)
        .is_ok();
    if held_name != name {
        let governs = held
            .active_role(actor)
            .is_some_and(|role| role >= Role::Moderator);
        if !(governs || settles) {
            return Err(MembershipRefusal::Refused(format!(
                "renaming workspace {workspace} is not the writer's"
            )));
        }
        let named = match automerge::ReadDoc::get(after, automerge::ROOT, WORKSPACE_NAME_KEY) {
            Ok(Some((automerge::Value::Scalar(value), _))) => match value.as_ref() {
                automerge::ScalarValue::Str(name) => Some(name.to_string()),
                _ => None,
            },
            _ => None,
        };
        named
            .as_deref()
            .ok_or_else(|| "the workspace's name is not text".to_owned())
            .and_then(|name| crate::workspace_membership::valid_workspace_name(name).map(drop))
            .map_err(MembershipRefusal::Malformed)?;
    }
    if held_settings != settings && !settles {
        return Err(MembershipRefusal::Refused(
            "the workspace's settings are not the writer's to change".into(),
        ));
    }
    Ok(())
}

/// The devices the batch admits or removes: each a device of a member of
/// the workspace as written — or of one the batch admitted and removed again —
/// and the writer's own, one an invitation admits, a further device of
/// someone already a member, or one they may admit.
fn identities(
    held: &WorkspacePolicy,
    wanted: &WorkspacePolicy,
    invited: &BTreeMap<String, (Role, String)>,
    (actor, authority, cx): (&str, &tilo_action_policy::Authority<'_>, &Context<'_>),
    (before, written): (&WorkspaceGovernance, &WorkspaceGovernance),
) -> Result<(), MembershipRefusal> {
    let person_of = |bytes: &[u8]| {
        GovernedIdentity::decode(bytes)
            .map(|identity| hex::encode(identity.device_cert.user_public_key))
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))
    };
    let may = |action: &str| held.check_action(authority, action, cx, false).is_ok();
    for (device, bytes) in &written.identities {
        if before.identities.get(device) == Some(bytes) {
            continue;
        }
        let person = person_of(bytes)?;
        let identity = GovernedIdentity::decode(bytes)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        tilo_user_identity::verify_device_cert(&identity.device_cert)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        if wanted.role_of(&person).is_none() && !invited.contains_key(&person) {
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
            && identity.delegate.is_none();
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
    // An owner's role is the workspace's, never a membership change.
    if wanted.members.values().any(|role| *role == Role::Owner)
        && held.members.values().all(|role| *role != Role::Owner)
    {
        return Err(MembershipRefusal::Refused("the owner's role".into()));
    }
    Ok(())
}

/// The sandbox capabilities the batch records or takes back: each recorded
/// one lasting past a restart and recorded as the writer's, who may grant in
/// the workspace; each taken back by one who may revoke there.
fn sandbox(
    before: &WorkspaceGovernance,
    written: &WorkspaceGovernance,
    held: &WorkspacePolicy,
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

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{ROOT, ReadDoc as _};

    /// What `run` writes on top of `doc`, as the batch sends it.
    fn written(doc: &mut AutoCommit, run: impl FnOnce(&mut AutoCommit)) -> Vec<Vec<u8>> {
        let heads = doc.get_heads();
        run(doc);
        doc.commit();
        doc.get_changes(&heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// A batch may carry a change its receiver already holds beside the one it
    /// adds: where the writer made the change it adds is what that names, not
    /// the older dependency of the change it re-sends. Read from there, the
    /// receiver's own newer state would read as the writer's change.
    #[test]
    fn a_batch_that_carries_what_the_receiver_holds_is_judged_where_it_adds_a_change() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "one", 1_u64).unwrap();
        doc.commit();
        let resent = written(&mut doc, |doc| {
            doc.put(ROOT, "two", 2_u64).unwrap();
        });
        let mut holder = doc.clone();
        let added = written(&mut doc, |doc| {
            doc.put(ROOT, "three", 3_u64).unwrap();
        });
        let mut batch = resent.clone();
        batch.extend(added);

        let mut at = dependency_state(&holder, &batch).unwrap();
        assert_eq!(
            at.get_heads(),
            holder.get_heads(),
            "the writer stood where the change it adds was made"
        );
        assert_eq!(
            at.get(ROOT, "two").unwrap().unwrap().0.as_u64(),
            Some(2),
            "what it re-sent is no part of where it stood"
        );

        // Two changes the batch adds on the one it re-sends are one position:
        // what each of them builds on is walked once.
        let mut theirs = holder.clone().with_actor(automerge::ActorId::random());
        let mut mine = holder.clone().with_actor(automerge::ActorId::random());
        let left = written(&mut theirs, |doc| {
            doc.put(ROOT, "left", 4_u64).unwrap();
        });
        let right = written(&mut mine, |doc| {
            doc.put(ROOT, "right", 5_u64).unwrap();
        });
        let mut forked = resent;
        forked.extend(left);
        forked.extend(right);
        let mut fork = dependency_state(&holder, &forked).unwrap();
        assert_eq!(fork.get_heads(), holder.get_heads());
    }
}
