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
//! A device writes its own way in, and no device writes another's: an
//! admission through an invitation is the joining device's, with its row and
//! the redemption receipt carrying the signed invitation, which every receiver
//! checks itself ([`invitation_admits`]); a device certified by a member's
//! root writes its own row as that member's device. Another device's row
//! changes only as it takes that device out. A holder of the workspace is
//! named by its owner or an administrator in the workspace's hosts, not on its
//! member list, and that naming gives it no person and no role.

use super::AdmissionFailure;
use std::collections::BTreeMap;

use automerge::AutoCommit;
use tilo_action_policy::{Context, Role, WorkspacePolicy, roles::governance};

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
    /// This device could not decide the batch: its isolated worker failed.
    /// Nothing is known about what the writer sent.
    Device(String),
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
            Self::Device(reason) => f.write_str(reason),
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
        .filter(|change| !before.has_change(&change.hash()))
        .collect();
    if pending.is_empty() {
        return Ok(before.clone());
    }
    while let Some(change) = pending.pop() {
        for dep in change.deps() {
            if before.has_change(dep) {
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
    let after = super::candidate(Some(&at), changes).map_err(|failure| match failure {
        AdmissionFailure::Device(reason) => MembershipRefusal::Device(reason),
        AdmissionFailure::Refused(refusal) => MembershipRefusal::Position(refusal.to_string()),
    })?;
    Ok((at, after))
}

/// Whether a batch that adds operations changes only what they admit: every
/// governed part it changes is one an operation it adds names, and its
/// author's role permits it.
///
/// # Errors
/// A part is changed without an operation naming it, or by an author whose role
/// does not permit it.
///
/// With `keeps_checkpoints`, the sender keeps the workspace's checkpoints, which
/// it writes as it publishes them with no operation naming them
/// ([`GovernanceRow::Checkpoints`]).
pub fn admit_operations(
    before: Option<&AutoCommit>,
    candidate: &AutoCommit,
    changes: &[Vec<u8>],
    workspace: &str,
    (now_ns, keeps_checkpoints): (u64, bool),
) -> Result<(), MembershipRefusal> {
    // A workspace this device does not hold yet is its governor's to send: the
    // operations of a first answer from anyone else govern nothing here.
    let Some(before) = before else {
        return Err(MembershipRefusal::NotMembership);
    };
    let (at, after) = position(before, changes)?;
    let written: Vec<automerge::Change> = changes
        .iter()
        .filter_map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
        .collect();
    operations_name_rows(
        (before, candidate),
        (&at, &after),
        Some(&written),
        workspace,
        (now_ns, keeps_checkpoints),
    )
}

/// Whether a newer base a device keeping the workspace's history made changes only what the
/// operations it adds admit, as [`admit_operations`] decides for a batch: each judged by its
/// author's grants as `before` holds them, the copy that adopts the base. A base does not
/// apply on `before`, so the operations are not positioned at their own dependencies, and
/// the two are read whole; one whose author `before` gives no grant for what it changes is
/// refused.
///
/// # Errors
/// As [`admit_operations`].
pub fn admit_base_operations(
    before: &AutoCommit,
    candidate: &AutoCommit,
    workspace: &str,
    (now_ns, keeps_checkpoints): (u64, bool),
) -> Result<(), MembershipRefusal> {
    operations_name_rows(
        (before, candidate),
        (before, candidate),
        None,
        workspace,
        (now_ns, keeps_checkpoints),
    )
}

/// Whether every governed part `candidate` changes from `before` is one an operation it adds
/// names, permitted to its author from `at` to `after`. `written` is what the batch wrote on
/// `before`, read where it wrote it; with none, both are read whole.
fn operations_name_rows(
    (before, candidate): (&AutoCommit, &AutoCommit),
    (at, after): (&AutoCommit, &AutoCommit),
    written: Option<&[automerge::Change]>,
    workspace: &str,
    (now_ns, keeps_checkpoints): (u64, bool),
) -> Result<(), MembershipRefusal> {
    let mut held_rows = match written {
        Some(written) => super::changed_rows_by(before, candidate, written, false),
        None => super::changed_rows(Some(before), Some(candidate), false),
    };
    if keeps_checkpoints {
        held_rows.retain(|row| *row != GovernanceRow::Checkpoints);
    }
    held_rows.sort_unstable();
    // What the batch wrote, read where it wrote it: a change to a workspace of
    // many costs what it wrote, not a reading of every member's row.
    let governance = written
        .and_then(|written| written_rows((at, after), written))
        .unwrap_or_else(|| (governance_of(Some(at)), governance_of(Some(after))));
    let mut declared: Vec<GovernanceRow> = Vec::new();
    let mut admitted = false;
    let ops = added_ops(before, candidate, written);
    let readmitted = readmissions(workspace, (at, after), &governance, &ops)?;
    let writers = own_row_writers(&ops, after);
    // The member list rows each operation was judged on, as its own author wrote them.
    let mut judged_rows: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for op in ops {
        let rows: &[GovernanceRow] = match &op.command {
            MembershipOpCommand::Governance { rows } => rows,
            // An admission writes the device it admits, and names that row
            // itself: each admission of a batch writes its own device.
            MembershipOpCommand::Admit { person, .. }
            | MembershipOpCommand::Enter { person, .. } => {
                // An admission is written by the device it admits, the
                // invitation or the entry its authority: no device signs
                // anyone else in.
                if *person != op.author.user_public_key {
                    return Err(MembershipRefusal::Refused(
                        "an admission is written by the device it admits".into(),
                    ));
                }
                if held_rows.contains(&GovernanceRow::Identities) {
                    let identities = [GovernanceRow::Identities];
                    let judged = own_rows(&governance, (at, after), written, &op);
                    judged_rows.extend(changed_identities(&judged));
                    rows_permitted(
                        workspace,
                        (at, after),
                        (&judged.0, &judged.1),
                        &op,
                        &identities,
                        (now_ns, &judged_elsewhere(&op, &readmitted, &writers)),
                    )?;
                    admitted = true;
                }
                continue;
            }
            MembershipOpCommand::Readmit { .. }
            | MembershipOpCommand::Project { .. }
            | MembershipOpCommand::Compact { .. } => continue,
        };
        // A batch carries every change its writer made since the receiver's
        // heads: several of them may name one part, each judged under its
        // own author.
        for row in rows {
            if !declared.contains(row) {
                declared.push(*row);
            }
        }
        let judged = own_rows(&governance, (at, after), written, &op);
        if rows.contains(&GovernanceRow::Identities) {
            judged_rows.extend(changed_identities(&judged));
        }
        rows_permitted(
            workspace,
            (at, after),
            (&judged.0, &judged.1),
            &op,
            rows,
            (now_ns, &judged_elsewhere(&op, &readmitted, &writers)),
        )?;
    }
    if admitted && !declared.contains(&GovernanceRow::Identities) {
        declared.push(GovernanceRow::Identities);
    }
    // Each member list row the batch changed is one an operation was judged on as its own
    // author's, or one a readmission took back: a row no operation's author wrote is judged by
    // none, and refused.
    if let Some(device) = changed_identities(&governance)
        .into_iter()
        .find(|device| !judged_rows.contains(device) && !readmitted.contains(device))
    {
        return Err(MembershipRefusal::Refused(format!(
            "device {device} writes its own row: no other device writes it in"
        )));
    }
    if !readmitted.is_empty() && !declared.contains(&GovernanceRow::Identities) {
        declared.push(GovernanceRow::Identities);
    }
    if keeps_checkpoints {
        declared.retain(|row| *row != GovernanceRow::Checkpoints);
    }
    declared.sort_unstable();
    // Every part the batch changes is one an operation names, and every part an
    // operation names is one the batch changes. A batch can write a part and
    // put it back as it was, as a server detached and named again among the
    // holders: the operations that wrote it still name it, and each was judged
    // on it above.
    let wrote = written
        .and_then(|written| super::rows::written_rows(candidate, written))
        .unwrap_or_default();
    if held_rows.iter().any(|row| !declared.contains(row))
        || declared
            .iter()
            .any(|row| !held_rows.contains(row) && !wrote.contains(row))
    {
        return Err(MembershipRefusal::NotMembership);
    }
    Ok(())
}

/// The devices `ops` take back onto the member list, each readmitted first: the rows a
/// readmission takes back are its own, and the batch's other operations are judged on the rest of
/// the member list.
fn readmissions(
    workspace: &str,
    (at, after): (&AutoCommit, &AutoCommit),
    governance: &(WorkspaceGovernance, WorkspaceGovernance),
    ops: &[SignedMembershipOp],
) -> Result<Vec<String>, MembershipRefusal> {
    let mut readmitted: Vec<String> = Vec::new();
    for op in ops {
        if let MembershipOpCommand::Readmit { device, root } = &op.command {
            readmits(
                workspace,
                (at, after),
                (&governance.0, &governance.1),
                op,
                (device, root),
            )?;
            readmitted.push(hex::encode(device));
        }
    }
    Ok(readmitted)
}

/// The devices that write their own row in `ops`: each such row is judged under its own device's
/// operation, and under no other, where the one that carried that operation wrote it.
fn own_row_writers(ops: &[SignedMembershipOp], after: &AutoCommit) -> Vec<[u8; 32]> {
    ops.iter()
        .filter(|op| match &op.command {
            MembershipOpCommand::Governance { rows } => rows.contains(&GovernanceRow::Identities),
            MembershipOpCommand::Admit { .. } => true,
            _ => false,
        })
        .filter(|op| vouched(after, op))
        .map(|op| op.author.device_public_key)
        .collect()
}

/// The devices whose member list row `governance`'s two sides hold differently.
fn changed_identities(governance: &(WorkspaceGovernance, WorkspaceGovernance)) -> Vec<String> {
    let (was, is) = (&governance.0.identities, &governance.1.identities);
    was.keys()
        .chain(is.keys())
        .filter(|device| was.get(*device) != is.get(*device))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// `governance`, the rows a batch wrote, with its member list narrowed to the rows `op`'s author
/// wrote and those written in the change that keeps `op`: a batch carries the changes of every
/// device since the receiver's heads, and each operation is judged on the member list rows written
/// with it, not on another device's own row written beside it. Where the batch's changes are not
/// at hand, the whole batch.
fn own_rows(
    governance: &(WorkspaceGovernance, WorkspaceGovernance),
    (at, after): (&AutoCommit, &AutoCommit),
    written: Option<&[automerge::Change]>,
    op: &SignedMembershipOp,
) -> (WorkspaceGovernance, WorkspaceGovernance) {
    // The changes its author's device wrote, and the one that keeps the operation itself, whoever's
    // actor wrote it: a host takes a device's signed entry in under its own.
    let device = op.author.device_public_key;
    let actors = [
        automerge::ActorId::from(device.to_vec()),
        crate::state::workspace_host_actor(&device),
    ];
    let key = op.id().ok().map(crate::members::op::op_key);
    let own: Option<Vec<automerge::Change>> = written.map(|written| {
        written
            .iter()
            .filter(|change| {
                actors.contains(change.actor_id())
                    || key.as_ref().is_some_and(|key| {
                        super::rows::touched(after, std::slice::from_ref(change))
                            .is_some_and(|touched| touched.contains_key(key))
                    })
            })
            .cloned()
            .collect()
    });
    let (mut was, mut is) = governance.clone();
    if let Some((own_was, own_is)) = own.and_then(|own| written_rows((at, after), &own)) {
        was.identities = own_was.identities;
        is.identities = own_is.identities;
    }
    // Its author's own row is its to answer for, wherever the batch carried it, when the one that
    // carried the operation wrote the row as it stands.
    let author = hex::encode(device);
    if !vouched(after, op) {
        return (was, is);
    }
    for (row, whole) in [(&mut was, &governance.0), (&mut is, &governance.1)] {
        match whole.identities.get(&author) {
            Some(bytes) => {
                row.identities.insert(author.clone(), bytes.clone());
            }
            None => {
                row.identities.remove(&author);
            }
        }
    }
    (was, is)
}

/// Whether the member list row of `op`'s author, as `after` holds it, was written by the actor
/// that keeps `op` itself: the row stands as the one who carried the device's signed operation
/// wrote it, never as another device wrote it beside that.
fn vouched(after: &AutoCommit, op: &SignedMembershipOp) -> bool {
    use automerge::ReadDoc as _;
    let actor_of = |id: automerge::ObjId| match id {
        automerge::ObjId::Id(_, actor, _) => Some(actor),
        automerge::ObjId::Root => None,
    };
    let Ok(id) = op.id() else {
        return false;
    };
    let carrier = after
        .get(automerge::ROOT, crate::members::op::op_key(id).as_str())
        .ok()
        .flatten()
        .and_then(|(_, id)| actor_of(id));
    let row = after
        .get(automerge::ROOT, "peer_identities_v1")
        .ok()
        .flatten()
        .and_then(|(_, identities)| {
            after
                .get(
                    &identities,
                    hex::encode(op.author.device_public_key).as_str(),
                )
                .ok()
                .flatten()
        })
        .and_then(|(_, id)| actor_of(id));
    carrier.is_some() && carrier == row
}

/// The member list rows `op` is not judged on, though they stand in what it is judged against:
/// those a readmission of the batch took back, and those of every other device that writes its
/// own row in the batch ([`vouched`]), which answers for that row under its own operation.
fn judged_elsewhere(
    op: &SignedMembershipOp,
    readmitted: &[String],
    writers: &[[u8; 32]],
) -> Vec<String> {
    readmitted
        .iter()
        .cloned()
        .chain(
            writers
                .iter()
                .filter(|writer| **writer != op.author.device_public_key)
                .map(hex::encode),
        )
        .collect()
}

/// The operations `candidate`, `before` with `written` on it, adds to the
/// ones `before` holds: each one kept under a key `written` wrote that
/// `before` does not hold, or with nothing written, every one `before` does
/// not hold.
fn added_ops(
    before: &AutoCommit,
    candidate: &AutoCommit,
    written: Option<&[automerge::Change]>,
) -> Vec<SignedMembershipOp> {
    let Some(touched) = written.and_then(|written| super::rows::touched(candidate, written)) else {
        let held = crate::members::ops(before);
        return crate::members::ops(candidate)
            .into_iter()
            .filter(|(id, _)| !held.contains_key(id))
            .map(|(_, op)| std::sync::Arc::unwrap_or_clone(op))
            .collect();
    };
    touched
        .keys()
        .filter(|key| crate::members::op::keyed(key).is_some())
        .filter(|key| values(before, &[], key).is_empty())
        .flat_map(|key| values(candidate, &[], key))
        .filter_map(|bytes| {
            tilo_failure::ignored!(
                "an operation that does not prove its author, or cannot be keyed by its own \
                 id, governs nothing",
                SignedMembershipOp::verified_stored(&bytes).ok()
            )
        })
        .map(|(_, op)| std::sync::Arc::unwrap_or_clone(op))
        .collect()
}

/// The byte values `doc` keeps under `key` of the map at `path` (the document
/// itself when empty), every one two writers put there at once.
pub(super) fn values(doc: &AutoCommit, path: &[&str], key: &str) -> Vec<Vec<u8>> {
    use automerge::ReadDoc as _;
    let mut object = automerge::ROOT;
    for step in path {
        match doc.get(&object, *step) {
            Ok(Some((automerge::Value::Object(_), child))) => object = child,
            _ => return Vec::new(),
        }
    }
    doc.get_all(&object, key)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(value, _)| match value {
            automerge::Value::Scalar(value) => match value.as_ref() {
                automerge::ScalarValue::Bytes(bytes) => Some(bytes.clone()),
                automerge::ScalarValue::Str(text) => Some(text.as_bytes().to_vec()),
                _ => None,
            },
            automerge::Value::Object(_) => None,
        })
        .collect()
}

/// The scalar that wins under `entry` of the map `doc` keeps at `key`.
pub(super) fn winner(doc: &AutoCommit, key: &str, entry: &str) -> Option<automerge::ScalarValue> {
    use automerge::ReadDoc as _;
    let (automerge::Value::Object(_), map) = doc.get(automerge::ROOT, key).ok().flatten()? else {
        return None;
    };
    match doc.get(&map, entry).ok().flatten()? {
        (automerge::Value::Scalar(value), _) => Some(value.into_owned()),
        _ => None,
    }
}

/// [`governance_of`] `at` and `after`, `at` with `written` on it, of only
/// what `written` changed: the device rows, names, sandbox entries and
/// receipts it wrote, the invitation parts it wrote and those its receipts
/// name. Every other entry reads the same in both, so each check comparing
/// the two decides as it does over the whole governance. `None` when
/// `written` rewrote one of those maps whole, or wrote where this reading
/// cannot place.
fn written_rows(
    (at, after): (&AutoCommit, &AutoCommit),
    written: &[automerge::Change],
) -> Option<(WorkspaceGovernance, WorkspaceGovernance)> {
    use crate::state::invitation_receipts::{PARTS_KEY, RECEIPTS_KEY, part_key};
    let touched = super::rows::touched(after, written)?;
    let entries = |key: &str| -> Option<Vec<String>> {
        match touched.get(key) {
            None => Some(Vec::new()),
            Some(super::rows::Wrote::Entries(entries)) => Some(entries.iter().cloned().collect()),
            Some(super::rows::Wrote::Whole) => None,
        }
    };
    let (identities, peers, sandbox, receipts) = (
        entries("peer_identities_v1")?,
        entries("peers")?,
        entries("sandbox")?,
        entries(RECEIPTS_KEY)?,
    );
    let mut parts: std::collections::BTreeSet<String> = entries(PARTS_KEY)?.into_iter().collect();
    for invite in &receipts {
        for doc in [at, after] {
            for bytes in values(doc, &[RECEIPTS_KEY], invite) {
                let logged = tilo_failure::ignored!(
                    "a receipt that does not decode shares no part",
                    crate::state::StoredReceipt::decode(&bytes).ok()
                );
                parts.extend(
                    logged
                        .iter()
                        .flat_map(|logged| logged.shared().map(part_key)),
                );
            }
        }
    }
    let read = |doc: &AutoCommit| {
        let mut governance = WorkspaceGovernance::absent();
        let winner = |key: &str, entry: &str| match winner(doc, key, entry)? {
            automerge::ScalarValue::Bytes(bytes) => Some(bytes),
            automerge::ScalarValue::Str(text) => Some(text.as_bytes().to_vec()),
            _ => None,
        };
        for device in &identities {
            if let Some(bytes) = winner("peer_identities_v1", device) {
                governance.identities.insert(device.clone(), bytes);
            }
        }
        for key in &peers {
            if let Some(name) = winner("peers", key).and_then(|bytes| {
                tilo_failure::ignored!(
                    "a name that is not text names nobody",
                    String::from_utf8(bytes).ok()
                )
            }) {
                governance.peers.insert(key.clone(), name);
            }
        }
        for id in &sandbox {
            if let Some(bytes) = winner("sandbox", id) {
                governance.sandbox.insert(id.clone(), bytes);
            }
        }
        for invite in &receipts {
            let kept = values(doc, &[RECEIPTS_KEY], invite);
            if !kept.is_empty() {
                governance.invitation_receipts.insert(invite.clone(), kept);
            }
        }
        for part in &parts {
            let kept = values(doc, &[PARTS_KEY], part);
            if !kept.is_empty() {
                governance.invitation_parts.insert(part.clone(), kept);
            }
        }
        governance
    };
    Some((read(at), read(after)))
}

/// What a device reads when it presents an entry admission another device's
/// receipt admits: the entry admits a person through their own device, and a
/// device of a person already in comes in as theirs, granted by one of their
/// own.
pub const ENTRY_NAMES_ANOTHER_DEVICE: &str = "this device joins as one of yours: on a device \
     of yours that is already in the workspace, add it to your person's devices, and it comes in as \
     theirs";

/// What a device reads when the entry it named is not the workspace's, or
/// takes nobody who asks.
pub const ENTRY_TAKES_NOBODY: &str =
    "the workspace's entry does not take this person in at the role it names";

/// The device row an entry admission adds, checked: `op` names the entry
/// `entry` its author came in under, and the row the same change writes must
/// carry that entry's receipt for that very device, at the role the entry
/// held where the operation sits. No invitation is behind it. Answers the
/// role and the person, for the membership the batch may list.
fn entered(
    op: &SignedMembershipOp,
    entry: [u8; 32],
    person: [u8; 32],
    at: &AutoCommit,
    (before, written): (&WorkspaceGovernance, &WorkspaceGovernance),
) -> Result<(Role, String), MembershipRefusal> {
    let refused = |message: &str| MembershipRefusal::Refused(message.to_owned());
    let author = hex::encode(op.author.device_public_key);
    let Some(row) = written
        .identities
        .get(&author)
        .filter(|row| before.identities.get(&author) != Some(*row))
    else {
        return Err(refused(ENTRY_NAMES_ANOTHER_DEVICE));
    };
    let identity = GovernedIdentity::decode(row)
        .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
    let Some(receipt) = identity.entry else {
        return Err(refused(ENTRY_NAMES_ANOTHER_DEVICE));
    };
    if identity.device_cert.device_public_key != op.author.device_public_key
        || identity.device_cert.user_public_key != person
        || receipt.entry != entry
    {
        return Err(refused(ENTRY_NAMES_ANOTHER_DEVICE));
    }
    if identity.device_cert.capabilities.agent.is_some()
        || identity.device_cert.capabilities.stand_in
    {
        return Err(MembershipRefusal::Refused(
            "a key acting for someone comes into a workspace by its delegation, not through the entry"
                .into(),
        ));
    }
    let at_entry = tilo_desktop_model::projection::workspace_entry_of(at);
    if crate::state::entry_digest(&at_entry).map_err(MembershipRefusal::Malformed)? != entry {
        return Err(refused(ENTRY_TAKES_NOBODY));
    }
    let Some(role) = at_entry.open_role() else {
        return Err(refused(ENTRY_TAKES_NOBODY));
    };
    if role != receipt.role {
        return Err(refused(ENTRY_TAKES_NOBODY));
    }
    Ok((role, hex::encode(person)))
}

/// Whether `op`'s author may change `rows`, from `at` to `after`.
fn rows_permitted(
    workspace: &str,
    (at, after): (&AutoCommit, &AutoCommit),
    (before, written): (&WorkspaceGovernance, &WorkspaceGovernance),
    op: &SignedMembershipOp,
    rows: &[GovernanceRow],
    (now_ns, elsewhere): (u64, &[String]),
) -> Result<(), MembershipRefusal> {
    let held = crate::workspace::policy_of(at, workspace);
    let wanted = crate::workspace::policy_of(after, workspace);
    let actor = op.person();
    let foreign = op.author.capabilities.foreign && !held.is_owner(&actor);
    let authority = held.person_on_device(&actor, foreign);
    let cx = Context::at(op.at_ns / 1_000_000);
    let writer = (actor.as_str(), &authority, &cx);
    for row in rows {
        if let Some(action) = row.action() {
            // A person records the cuts of their own devices' removals: that is their own list
            // of devices, which they keep without the workspace's administration.
            if *row == GovernanceRow::Removals && own_device_cuts((at, after), before, &actor) {
                continue;
            }
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
            GovernanceRow::Sandbox => sandbox(before, written, &held, writer)?,
            GovernanceRow::Standing => standing(workspace, at, after)?,
            GovernanceRow::Identities => {
                let invited = match &op.command {
                    MembershipOpCommand::Enter { entry, person } => {
                        let (role, person) = entered(op, *entry, *person, at, (before, written))?;
                        BTreeMap::from([(person, (role, "entry".to_owned()))])
                    }
                    _ => redemptions(
                        workspace,
                        (&held, &wanted),
                        (before, written),
                        (op.at_ns, now_ns),
                    )?,
                };
                identities(
                    &held,
                    &wanted,
                    &invited,
                    (writer, &op.author.device_public_key),
                    (before, written),
                    elsewhere,
                )?;
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

/// Whether the cuts `after` adds to `at` are of devices the workspace certifies for `actor`: a
/// person records the removal of their own devices, and takes no one else's.
fn own_device_cuts(
    (at, after): (&AutoCommit, &AutoCommit),
    before: &WorkspaceGovernance,
    actor: &str,
) -> bool {
    let (Ok(held), Ok(written)) = (
        crate::removals::recorded(at),
        crate::removals::recorded(after),
    ) else {
        return false;
    };
    let added: Vec<&[u8; 32]> = written.difference(&held).collect();
    !added.is_empty()
        && added.iter().all(|device| {
            before
                .identities
                .get(&hex::encode(device))
                .and_then(|bytes| crate::workspace::identity_row(bytes).ok())
                .is_some_and(|row| hex::encode(row.device_cert.user_public_key) == actor)
        })
}

/// The name a receiver's refusal gives `row`.
fn row_name(row: GovernanceRow) -> &'static str {
    match row {
        GovernanceRow::Owner => "the workspace's owner",
        GovernanceRow::Membership => "the workspace's membership",
        GovernanceRow::Identities => "the devices of the workspace",
        GovernanceRow::Settings => "the workspace's settings",
        GovernanceRow::RetiredMedia => "the call policy older builds kept",
        GovernanceRow::Versions => "the app versions the workspace runs",
        GovernanceRow::PackageTrust => {
            "the attestors the workspace trusts and the publishers it refuses"
        }
        GovernanceRow::PackageRegistries => "the registries the workspace fetches apps from",
        GovernanceRow::Checkpoints => "the workspace's checkpoints",
        GovernanceRow::Hosts => "the workspace's hosts",
        GovernanceRow::Log => "the workspace's operations",
        GovernanceRow::Sandbox => "the capabilities of its apps' sandboxes",
        GovernanceRow::Removals => "the cuts of its devices' removals",
        GovernanceRow::Standing => "the history kept of standing taken away",
        GovernanceRow::Calls => "what the workspace's calls allow",
    }
}

/// Whether the change from `at` to `after` cuts only whom it takes standing
/// from ([`crate::standing::taken_from`]): a cut is the record of a change
/// that took standing away, written by whoever may make that change, and of
/// no one else.
///
/// A batch can carry a change that took a standing away and a later one that
/// gave it back, as a server detached and named again among the holders: read
/// across the whole batch, nothing is taken from it. Such a cut is judged at
/// the write that made it, which must have taken the standing it records: a
/// governance write commits what it changes and then the cuts and operations
/// recording it, so that write is the change holding the cut together with
/// its parent when the same device made both.
fn standing(workspace: &str, at: &AutoCommit, after: &AutoCommit) -> Result<(), MembershipRefusal> {
    let taken = crate::standing::taken_from(workspace, at, after);
    let cut = crate::standing::subjects_cut(at, after).map_err(MembershipRefusal::Refused)?;
    let untaken: Vec<[u8; 32]> = cut
        .into_iter()
        .filter(|subject| !taken.contains(subject))
        .collect();
    if untaken.is_empty() {
        return Ok(());
    }
    let mut history = after.clone();
    let mut from = at.clone();
    let changes = history.get_changes(&from.get_heads());
    for subject in untaken {
        let taken_where_cut = changes.iter().any(|change| {
            let start = match change.deps() {
                [parent] => changes
                    .iter()
                    .find(|earlier| {
                        earlier.hash() == *parent && earlier.actor_id() == change.actor_id()
                    })
                    .unwrap_or(change),
                _ => change,
            };
            let (Ok(before), Ok(written)) = (
                history.fork_at(start.deps()),
                history.fork_at(&[change.hash()]),
            ) else {
                return false;
            };
            crate::standing::subjects_cut(&before, &written).is_ok_and(|cut| cut.contains(&subject))
                && crate::standing::taken_from(workspace, &before, &written).contains(&subject)
        });
        if !taken_where_cut {
            return Err(MembershipRefusal::Refused(format!(
                "a cut of {}, whose standing the change takes nothing of",
                hex::encode(subject)
            )));
        }
    }
    Ok(())
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
    invitation_admission(policy, workspace, signed, joiner, at_ns).map(|admitted| admitted.grant)
}

/// What an invitation admits its joiner as: its grant, and, for an
/// administrator's invitation, the person whose making of an administrator
/// it carries (the inviter it names, else its issuer's person).
pub struct Admitted {
    pub grant: crate::join::InviteGrant,
    pub administrator_by: Option<String>,
}

/// [`invitation_admits`], with whether the invitation makes its joiner an
/// administrator. An administrator's invitation admits only when everyone
/// whose authority it carries ([`crate::join::VerifiedJoin::granted_by`]:
/// its issuer, and the person a host signed it for) may make administrators
/// as `policy` stands, so an invitation an Editor's device signed, or a host
/// signed for someone who may not make administrators, makes nobody one.
pub fn invitation_admission(
    policy: &tilo_action_policy::WorkspacePolicy,
    workspace: &str,
    signed: &[u8],
    joiner: [u8; 32],
    at_ns: u64,
) -> Result<Admitted, MembershipRefusal> {
    admission_of(policy, workspace, signed, joiner, at_ns, true)
}

/// [`invitation_admits`] for `joiner` as the workspace's owner, whose device comes in on its
/// certificate: the invitation must be one an inviter of the workspace signed, however long
/// ago it expired.
pub(crate) fn owner_invitation_admits(
    policy: &tilo_action_policy::WorkspacePolicy,
    workspace: &str,
    signed: &[u8],
    joiner: [u8; 32],
    at_ns: u64,
) -> Result<crate::join::InviteGrant, MembershipRefusal> {
    admission_of(policy, workspace, signed, joiner, at_ns, false).map(|admitted| admitted.grant)
}

/// [`invitation_admission`], its invitation's expiry enforced only when `expires` says so.
fn admission_of(
    policy: &tilo_action_policy::WorkspacePolicy,
    workspace: &str,
    signed: &[u8],
    joiner: [u8; 32],
    at_ns: u64,
    expires: bool,
) -> Result<Admitted, MembershipRefusal> {
    let refused = |error: String| MembershipRefusal::Refused(format!("invitation: {error}"));
    let verify = if expires {
        crate::join::decode_and_verify_invitation
    } else {
        crate::join::decode_and_verify_invitation_past_expiry
    };
    let join = verify(
        signed,
        Some(joiner),
        &tilo_user_identity::RevocationList::new(),
        at_ns,
    )
    .map_err(refused)?;
    let grant = join.grant().clone();
    if grant.workspace_id.to_string() != workspace {
        return Err(refused("it is for another workspace".into()));
    }
    let issuer = hex::encode(grant.issuer.user_public_key);
    // The owner grants itself anything, from any device of theirs, so the
    // foreign-device bound does not narrow what they invite.
    let foreign = grant.issuer.capabilities.foreign && !policy.is_owner(&issuer);
    let cx = Context::at_secs(at_ns / 1_000_000_000);
    let authority = policy.person_on_device(&issuer, foreign);
    policy
        .check_admission(&authority, grant.role, &cx, false)
        .map_err(|refusal| refused(format!("its issuer may not invite: {refusal}")))?;
    if !join.administrator() {
        return Ok(Admitted {
            grant,
            administrator_by: None,
        });
    }
    let mut administrator_by = issuer.clone();
    for person in join.granted_by() {
        let person = hex::encode(person);
        // The issuer signs on its own device; a person it signs for asked
        // it, and holds what they hold on their own.
        let authority = if person == issuer {
            policy.person_on_device(&issuer, foreign)
        } else {
            policy.person_on_device(&person, false)
        };
        policy
            .check_administrator_invite(&person, &authority, &cx, false)
            .map_err(|refusal| {
                refused(format!(
                    "it makes an administrator {person} may not make: {refusal}"
                ))
            })?;
        administrator_by = person;
    }
    Ok(Admitted {
        grant,
        administrator_by: Some(administrator_by),
    })
}

/// When `person` is admitted: the batch's `written_ns` for the workspace's owner, else the time
/// the workspace recorded them joining; refused when that is in the future past `now_ns`.
fn admitted_at(
    wanted: &WorkspacePolicy,
    person: &str,
    owner: bool,
    (written_ns, now_ns): (u64, u64),
) -> Result<u64, MembershipRefusal> {
    let at_ns = if owner {
        written_ns
    } else {
        let joined_ms = wanted.joined_at(person).ok_or_else(|| {
            MembershipRefusal::Refused(format!("{person} is admitted at no recorded time"))
        })?;
        joined_ms.saturating_mul(1_000_000)
    };
    if at_ns > now_ns.saturating_add(ADMISSION_CLOCK_SKEW_NS) {
        return Err(MembershipRefusal::Refused(format!(
            "{person} is admitted in the future"
        )));
    }
    Ok(at_ns)
}

/// The people the batch admits through invitations, each with the role
/// their invitation gives and the device that redeemed it. Every receipt
/// the batch adds to the workspace's log must be one: the invitation it
/// carries admits its joiner at the time the workspace records them joining
/// ([`invitation_admits`]), the joiner's device row holds the receipt, and
/// the workspace gives them its role. The owner's device redeems one too,
/// admitting nobody new: the invitation admits it when the batch was
/// written (`written_ns`), and its row holds the receipt. Every invitation
/// part the batch adds
/// is kept under its own digest and completes a receipt it adds. That an
/// invitation is redeemed once, and a part once kept stays, is every
/// writer's rule, checked for the whole candidate
/// ([`super::redemptions_kept`]).
fn redemptions(
    workspace: &str,
    (held, wanted): (&WorkspacePolicy, &WorkspacePolicy),
    (before, written): (&WorkspaceGovernance, &WorkspaceGovernance),
    (written_ns, now_ns): (u64, u64),
) -> Result<BTreeMap<String, (Role, String)>, MembershipRefusal> {
    use crate::state::invitation_receipts::{part_in, part_key};
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
            let logged = crate::state::StoredReceipt::decode(value)
                .map_err(|error| malformed(error.to_string()))?;
            let receipt = &logged.receipt;
            if *invite != hex::encode(receipt.invite_id) {
                return Err(malformed("an invitation receipt under another key".into()));
            }
            let signed = logged
                .invitation(|digest| part_in(&written.invitation_parts, digest))
                .ok_or_else(|| {
                    malformed("an invitation receipt names no invitation it holds".into())
                })?;
            for digest in logged.shared() {
                new_parts.remove(&part_key(digest));
            }
            let person = hex::encode(receipt.user_public_key);
            // The workspace's owner holds it from its beginning, so a device
            // of theirs that redeems an invitation (a host's page joining the
            // workspace it handed them) comes in on its certificate: the
            // invitation still has to be one an inviter of the workspace signed,
            // and gives them no role. Its expiry does not bind them: the
            // certificate admits them, however long after the invitation was
            // issued the change that records it was written.
            let owner = held.is_owner(&person) && wanted.is_owner(&person);
            let at_ns = admitted_at(wanted, &person, owner, (written_ns, now_ns))?;
            let grant = admission_of(
                held,
                workspace,
                &signed,
                receipt.user_public_key,
                at_ns,
                !owner,
            )?
            .grant;
            let role: Role = grant.role;
            if !owner && (grant.role != receipt.role || wanted.role_of(&person) != Some(role)) {
                return Err(MembershipRefusal::Refused(format!(
                    "{person} does not take the role their invitation gives"
                )));
            }
            if held.is_banned(&person) || (held.is_owner(&person) && !owner) {
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
            if !owner {
                invited.insert(person, (role, device));
            }
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

/// The workspace's name and its settings — its saved grant sets and its entry —
/// as the batch changes them: the name a valid one, given by one granted
/// renaming the workspace or its settings; the settings by one granted its
/// settings.
fn settings(
    workspace: &str,
    at: &AutoCommit,
    after: &AutoCommit,
    held: &WorkspacePolicy,
    (_, authority, cx): Writer<'_>,
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
        let renames = held
            .check_action(authority, governance::RENAME, cx, false)
            .is_ok();
        if !(renames || settles) {
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
    ((actor, authority, cx), author): (
        (&str, &tilo_action_policy::Authority<'_>, &Context<'_>),
        &[u8; 32],
    ),
    (before, written): (&WorkspaceGovernance, &WorkspaceGovernance),
    elsewhere: &[String],
) -> Result<(), MembershipRefusal> {
    let decoded = |bytes: &[u8]| {
        GovernedIdentity::decode(bytes)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))
    };
    let person_of =
        |bytes: &[u8]| decoded(bytes).map(|row| hex::encode(row.device_cert.user_public_key));
    let may = |action: &str| held.check_action(authority, action, cx, false).is_ok();
    let author = hex::encode(author);
    for (device, bytes) in &written.identities {
        let held_row = before.identities.get(device);
        // A row another operation of the batch answers for is judged under that one.
        if held_row == Some(bytes) || elsewhere.contains(device) {
            continue;
        }
        let person = person_of(bytes)?;
        let identity = decoded(bytes)?;
        tilo_user_identity::verify_device_cert(&identity.device_cert)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        // A device writes its own row, and no device writes another's in:
        // another device's row changes only as it takes that device out.
        if *device != author {
            let takes_out = match held_row {
                Some(held_row) => takes_out(&decoded(held_row)?, identity.clone())?,
                None => identity.removed,
            };
            if !takes_out {
                return Err(MembershipRefusal::Refused(format!(
                    "device {device} writes its own row: no other device writes it in"
                )));
            }
            if person != actor && !may(governance::REMOVE_MEMBER) {
                return Err(MembershipRefusal::Refused(format!(
                    "device {device} is not the writer's to remove"
                )));
            }
            continue;
        }
        if person != actor {
            return Err(MembershipRefusal::Refused(format!(
                "device {device} is listed under another person than its writer's"
            )));
        }
        if wanted.role_of(&person).is_none() && !invited.contains_key(&person) {
            return Err(MembershipRefusal::Refused(format!(
                "device {device} is not of a member"
            )));
        }
    }
    for (device, bytes) in &before.identities {
        if written.identities.contains_key(device) || elsewhere.contains(device) {
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

/// Whether `op` may take `device`, a device the workspace removed, back onto its member list with
/// the mark `root`: its author's device keeps its person's root, as `root` (that root's own
/// signature) shows, and its person may add members where the batch was made. The row it writes
/// gives the device its place back and nothing else: the row the workspace marked removed with
/// the mark taken off, or where the removal took the row out, a bare row of the device's own
/// certificate for a person the workspace has. The mirror of [`takes_out`]: a removed device
/// never writes itself back in.
fn readmits(
    workspace: &str,
    (at, after): (&AutoCommit, &AutoCommit),
    (before, written): (&WorkspaceGovernance, &WorkspaceGovernance),
    op: &SignedMembershipOp,
    (device, root): (&[u8; 32], &[u8; 64]),
) -> Result<(), MembershipRefusal> {
    if !tilo_user_identity::root_holder::keeps_root(&op.author, root) {
        return Err(MembershipRefusal::Refused(
            "a removed device is taken back only by a device that keeps its person's root".into(),
        ));
    }
    let held = crate::workspace::policy_of(at, workspace);
    let actor = op.person();
    let foreign = op.author.capabilities.foreign && !held.is_owner(&actor);
    let authority = held.person_on_device(&actor, foreign);
    let cx = Context::at(op.at_ns / 1_000_000);
    held.check_action(&authority, governance::ADD_MEMBER, &cx, false)
        .map_err(|refusal| {
            MembershipRefusal::Refused(format!("a readmission takes add-member: {refusal}"))
        })?;
    let key = hex::encode(device);
    let decoded = |bytes: &[u8]| {
        GovernedIdentity::decode(bytes)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))
    };
    let encoded = |row: &GovernedIdentity| {
        row.encode()
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))
    };
    let Some(is) = written.identities.get(&key) else {
        return Err(MembershipRefusal::Refused(format!(
            "device {key} is taken back onto a row of its own"
        )));
    };
    let mut is = decoded(is)?;
    if is.removed || is.device_cert.device_public_key != *device {
        return Err(MembershipRefusal::Refused(format!(
            "device {key} is taken back under its own certificate, unmarked"
        )));
    }
    if let Some(was) = before.identities.get(&key) {
        let was = decoded(was)?;
        // A copy that holds the device's row as the readmission writes it, unmarked, takes
        // nothing back: the change gives the device nothing there.
        if !was.removed && encoded(&is)? == encoded(&was)? {
            return Ok(());
        }
        is.removed = true;
        if !was.removed || encoded(&is)? != encoded(&was)? {
            return Err(MembershipRefusal::Refused(format!(
                "device {key} is given its place back and nothing else"
            )));
        }
    } else {
        tilo_user_identity::verify_device_cert(&is.device_cert)
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))?;
        let person = hex::encode(is.device_cert.user_public_key);
        let bare = is.revocations.is_empty()
            && is.admission.is_none()
            && is.admissions.is_empty()
            && is.delegate.is_none()
            && is.workspace_delegations.is_empty();
        let wanted = crate::workspace::policy_of(after, workspace);
        if !bare || (!wanted.is_owner(&person) && wanted.role_of(&person).is_none()) {
            return Err(MembershipRefusal::Refused(format!(
                "device {key} is given its place back and nothing else"
            )));
        }
    }
    Ok(())
}

/// Whether `written`, another device's row in place of `held`, only takes the
/// device out: it marks the row removed, records revocations or withdraws an
/// agent's delegation, and gives the device nothing `held` did not, whether a
/// certificate, an admission or a delegation.
fn takes_out(
    held: &GovernedIdentity,
    mut written: GovernedIdentity,
) -> Result<bool, MembershipRefusal> {
    if held.removed && !written.removed {
        return Ok(false);
    }
    if !held
        .revocations
        .iter()
        .all(|entry| written.revocations.contains(entry))
    {
        return Ok(false);
    }
    match (&held.delegate, &mut written.delegate) {
        (Some(was), Some(is)) => {
            if was.withdrawn_at_ns.is_some() && is.withdrawn_at_ns != was.withdrawn_at_ns {
                return Ok(false);
            }
            is.withdrawn_at_ns = was.withdrawn_at_ns;
        }
        (None, None) => {}
        _ => return Ok(false),
    }
    written.removed = held.removed;
    written.revocations.clone_from(&held.revocations);
    let encoded = |row: &GovernedIdentity| {
        row.encode()
            .map_err(|error| MembershipRefusal::Malformed(error.to_string()))
    };
    Ok(encoded(&written)? == encoded(held)?)
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
        if !capability.is_durable() {
            return Err(MembershipRefusal::Refused(format!(
                "sandbox capability {id} ends with a session or a single use"
            )));
        }
        if capability
            .granted_by
            .as_ref()
            .and_then(tilo_action_policy::MemberId::person_id)
            != Some(actor)
        {
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
