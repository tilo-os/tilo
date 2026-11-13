//! The workspace's membership, derived from its operations, the same way on
//! every device.
//!
//! Every receiver keeps every operation it is sent (admission refuses a
//! changed or unproven blob, never a member's own change), so the set of
//! operations a device holds is the workspace's record of who changed what.
//! Replay is a function of that set and the workspace's owner alone:
//!
//! - the workspace starts as its owner's ([`WorkspacePolicy::new`]): the owner
//!   and every role's preset grants, no one else;
//! - the operations are applied in one deterministic order: no operation
//!   before one it was written on top of, and operations written concurrently
//!   ordered by their author's standing in the workspace as it stands when
//!   they are reached — the owner's first, then an administrator's, then
//!   everyone else's — and, among equals, by id;
//! - each is decided against the membership at the point it is reached
//!   ([`tilo_action_policy::WorkspacePolicy::apply`]), and an admission by
//!   the invitation it redeems as the membership then stands, so an operation
//!   a writer was not allowed to make changes nothing, and every device
//!   holding the same operations applies exactly the same ones.
//!
//! Two devices that hold the same operations therefore hold the same
//! membership. A device that holds fewer catches up as operations arrive; one
//! that holds an operation it cannot apply keeps it and is unaffected by it.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use tilo_action_policy::{Change, Context, Grantee, Role, WorkspacePolicy, roles};

use super::op::{MembershipOpCommand, OpId, SignedMembershipOp};

/// The operations a workspace document holds, by id.
pub type HeldOps = BTreeMap<OpId, std::sync::Arc<SignedMembershipOp>>;

/// Every operation `doc` holds, by id, that proves its author. A blob that
/// does not decode, or whose signature is not its author device's, is kept in
/// the document and ignored here: admission refuses such a blob, and a device
/// that was sent one another way is still never governed by it.
#[must_use]
pub fn ops(doc: &AutoCommit) -> HeldOps {
    raw(doc)
        .into_values()
        .filter_map(|bytes| {
            tilo_failure::ignored!(
                "an operation that does not prove its author, or cannot be keyed by its own id, \
                 governs nothing",
                SignedMembershipOp::verified_stored(&bytes).ok()
            )
        })
        .collect()
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

/// The workspace's membership as `doc`'s operations make it.
#[must_use]
pub fn replay(doc: &AutoCommit, workspace: &str) -> WorkspacePolicy {
    // A change is replayed where it is written and again where it is taken
    // in, from the same document state: the membership that state gives is
    // remembered by its heads. A document with a change still open is not
    // the state its heads name, and is replayed as it is.
    if doc.pending_ops() > 0 {
        return replayed_from(doc, workspace).policy;
    }
    let heads = doc.base_heads();
    if let Some(replayed) = recall(workspace, &heads) {
        return replayed.policy;
    }
    let replayed = replayed_from(doc, workspace);
    let policy = replayed.policy.clone();
    keep(workspace, heads, replayed);
    policy
}

/// The membership a document state gives, as [`replay`] keeps it.
#[derive(Clone)]
struct Replayed {
    policy: WorkspacePolicy,
    /// The document keys of the state's operations.
    op_keys: BTreeSet<String>,
    /// The state's membership heads ([`heads`]).
    heads: BTreeSet<OpId>,
    /// Whether an admission the state holds admitted no one: an invitation
    /// part arriving later could let it.
    unadmitted: bool,
}

/// The document states whose membership was read, by workspace and heads.
type States = std::sync::Mutex<crate::kept_states::KeptStates<Replayed>>;

fn states() -> &'static States {
    static STATES: std::sync::OnceLock<States> = std::sync::OnceLock::new();
    STATES.get_or_init(|| {
        std::sync::Mutex::new(crate::kept_states::KeptStates::new(REMEMBERED, WORKSPACES))
    })
}

/// The states of one workspace a write and the admissions after it read
/// again.
const REMEMBERED: usize = 8;
/// The workspaces whose states are kept at once: a host's busy ones.
const WORKSPACES: usize = 256;

fn recall(workspace: &str, heads: &[automerge::ChangeHash]) -> Option<Replayed> {
    states()
        .lock()
        .ok()
        .and_then(|mut states| states.recall(workspace, heads))
}

fn keep(workspace: &str, heads: Vec<automerge::ChangeHash>, replayed: Replayed) {
    if let Ok(mut states) = states().lock() {
        states.keep(workspace, heads, replayed);
    }
}

/// Remember the membership of `after`, the state at `before` with `changes`
/// on top, when the changes add operations written on top of every head the
/// state held and change nothing else a replay reads.
///
/// Such an operation is the last the order reaches, whoever wrote it: every
/// other operation is one it was written on top of. Its effect on the
/// membership remembered for `before` is therefore its effect on the whole
/// replay, and an admission or a name costs no replay of every operation the
/// workspace holds.
pub(crate) fn carry(
    before: &[automerge::ChangeHash],
    after: &AutoCommit,
    changes: &[automerge::Change],
    workspace: &str,
) {
    use crate::state::invitation_ledger::{PARTS_KEY, RECEIPTS_KEY};
    if after.pending_ops() > 0 {
        return;
    }
    let Some(mut replayed) = recall(workspace, before) else {
        return;
    };
    let Some(touched) = crate::workspace::rows::touched(after, changes) else {
        return;
    };
    let mut added: HeldOps = BTreeMap::new();
    for key in touched.keys() {
        match key.as_str() {
            // The owner starts the replay, and a saved set's rules decide
            // the grants assigned from it.
            tilo_desktop_model::projection::WORKSPACE_OWNER_KEY
            | tilo_desktop_model::projection::WORKSPACE_PRESETS_KEY => return,
            // A receipt or part admits no one by itself; one that completes an
            // admission the state turned away makes it replay whole.
            RECEIPTS_KEY | PARTS_KEY if replayed.unadmitted => return,
            _ => {}
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
        if let Ok((id, op)) = tilo_failure::ignored!(
            "an operation that does not prove its writer is never applied, so it moves nothing",
            SignedMembershipOp::verified_stored(bytes)
        ) {
            added.insert(id, op);
        }
    }
    let mut ledger = Ledger::new(after, workspace);
    while !added.is_empty() {
        let Some(id) = added
            .iter()
            .find(|(_, op)| replayed.heads.iter().all(|head| op.parents.contains(head)))
            .map(|(id, _)| *id)
        else {
            return;
        };
        let Some(op) = added.remove(&id) else {
            return;
        };
        replayed.unadmitted |= !op_effect(&mut replayed.policy, &op, &mut ledger);
        replayed.heads = BTreeSet::from([id]);
    }
    // Every carry a test makes is checked against replaying the state.
    #[cfg(test)]
    assert!(
        read_replay(after, workspace).policy == replayed.policy,
        "a carried membership is the one its state replays to"
    );
    keep(workspace, after.base_heads(), replayed);
}

/// [`replay`], read from the document.
fn replayed_from(doc: &AutoCommit, workspace: &str) -> Replayed {
    crate::governance_work::replayed();
    read_replay(doc, workspace)
}

/// [`replayed_from`], uncounted.
fn read_replay(doc: &AutoCommit, workspace: &str) -> Replayed {
    let raw = raw(doc);
    let op_keys = raw
        .keys()
        .map(|key| format!("{}/{key}", super::op::MEMBERS_KEY))
        .collect();
    let held: HeldOps = raw
        .into_values()
        .filter_map(|bytes| {
            tilo_failure::ignored!(
                "an operation that does not prove its author, or cannot be keyed by its own id, \
                 governs nothing",
                SignedMembershipOp::verified_stored(&bytes).ok()
            )
        })
        .collect();
    let (policy, unadmitted) = replay_held(doc, workspace, &held);
    Replayed {
        policy,
        op_keys,
        heads: heads(&held).into_iter().collect(),
        unadmitted,
    }
}

/// [`heads`] of `doc`'s operations: those of a state whose membership was
/// read ([`replay`]) as it was remembered with it, so a write on a workspace
/// whose log holds an operation for every admission does not decode the log
/// to name its parents.
#[must_use]
pub fn heads_of(doc: &AutoCommit, workspace: &str) -> Vec<OpId> {
    if doc.pending_ops() == 0
        && let Some(found) = recall(workspace, &doc.base_heads())
    {
        #[cfg(test)]
        assert_eq!(
            found.heads,
            heads(&ops(doc)).into_iter().collect(),
            "remembered membership heads are the state's"
        );
        return found.heads.into_iter().collect();
    }
    heads(&ops(doc))
}

/// The membership a workspace starts from: its owner's, as the `owner` row
/// names them, with every role's preset grants; with no owner yet, the presets
/// alone.
fn start(doc: &AutoCommit, workspace: &str) -> WorkspacePolicy {
    let owner = doc
        .get(ROOT, tilo_desktop_model::projection::WORKSPACE_OWNER_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok());
    match owner {
        Some(owner) => WorkspacePolicy::new(workspace, owner),
        None => WorkspacePolicy::unowned(workspace, &Grantee::role(Role::Owner)),
    }
    .with_current_roles()
}

/// The membership `held`, `doc`'s operations, give, and whether an admission
/// admitted no one.
fn replay_held(doc: &AutoCommit, workspace: &str, held: &HeldOps) -> (WorkspacePolicy, bool) {
    let mut policy = start(doc, workspace);
    order(&mut policy);
    let mut ledger = Ledger::new(doc, workspace);
    let mut unadmitted = false;
    // Each operation waits for the parents it was written on top of; one
    // whose parents are all applied is ready. Of the ready ones the next is
    // the one the order reaches first. An operation written on top of one
    // this device does not hold waits for good.
    let mut waiting: BTreeMap<OpId, usize> = BTreeMap::new();
    let mut children: BTreeMap<OpId, Vec<OpId>> = BTreeMap::new();
    let mut ready: BTreeSet<OpId> = BTreeSet::new();
    for (id, op) in held {
        let parents: BTreeSet<OpId> = op.parents.iter().copied().collect();
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
            unadmitted |= !op_effect(&mut policy, op, &mut ledger);
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
    (policy, unadmitted)
}

/// The grants of a membership in the one order every device reads them in: by
/// id. Which grant comes first is no part of what a workspace allows.
fn order(policy: &mut WorkspacePolicy) {
    policy
        .grants
        .sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
}

/// The invitation ledger of the document a replay reads, its parts read once
/// and only when an admission needs them.
struct Ledger<'a> {
    doc: &'a AutoCommit,
    workspace: &'a str,
    parts: Option<BTreeMap<String, Vec<Vec<u8>>>>,
}

impl<'a> Ledger<'a> {
    const fn new(doc: &'a AutoCommit, workspace: &'a str) -> Self {
        Self {
            doc,
            workspace,
            parts: None,
        }
    }

    /// Admit `person` into `policy` by the invitation `invite`, as recorded at
    /// `at_ns`: the ledger holds their receipt for it, the invitation it names
    /// admits them as `policy` stands ([`crate::workspace::membership::invitation_admits`]),
    /// and they are neither its owner nor banned. Someone already a member
    /// keeps the role they hold: a redemption is how someone entered, not the
    /// role they hold now. False when it admits no one.
    fn admit(
        &mut self,
        policy: &mut WorkspacePolicy,
        invite: [u8; 32],
        person: [u8; 32],
        at_ns: u64,
    ) -> bool {
        let Ok(values) = tilo_failure::ignored!(
            "a ledger this device cannot read admits no one",
            crate::workspace::invitation_receipts_of(self.doc, &hex::encode(invite))
        ) else {
            return false;
        };
        let parts = self.parts.get_or_insert_with(|| {
            tilo_failure::ignored!(
                "parts this device cannot read complete no receipt",
                crate::state::invitation_ledger::parts(self.doc).unwrap_or_default()
            )
        });
        let Some(signed) = values.iter().find_map(|value| {
            let ledgered = crate::state::LedgerReceipt::decode(value).ok()?;
            (ledgered.receipt.user_public_key == person && ledgered.receipt.invite_id == invite)
                .then(|| {
                    ledgered.invitation(|digest| {
                        crate::state::invitation_ledger::part_in(parts, digest)
                    })
                })
                .flatten()
        }) else {
            return false;
        };
        let Ok(grant) = crate::workspace::membership::invitation_admits(
            policy,
            self.workspace,
            &signed,
            person,
            at_ns,
        ) else {
            return false;
        };
        let person = hex::encode(person);
        if policy.is_owner(&person) || policy.is_banned(&person) {
            return false;
        }
        policy
            .joined
            .entry(person.clone())
            .or_insert(at_ns / 1_000_000);
        policy
            .members
            .entry(person)
            .or_insert_with(|| Role::from(grant.role));
        true
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
    let administrator = roles::administrator_grant_id(person);
    if policy.grants.iter().any(|grant| grant.id == administrator) {
        return 1;
    }
    2
}

/// Fold one operation into the membership: the changes it carries, in order,
/// each decided against the membership as the operations before it left it. An
/// operation with a change its author's grants do not allow changes nothing:
/// one refused step takes the whole operation with it, so a writer cannot slip
/// a change past the membership by putting an allowed one in front of it.
/// False when it was an admission that admitted no one.
fn op_effect(policy: &mut WorkspacePolicy, op: &SignedMembershipOp, ledger: &mut Ledger) -> bool {
    match &op.command {
        MembershipOpCommand::Governance { .. } => {}
        MembershipOpCommand::Admit { invite, person } => {
            if !ledger.admit(policy, *invite, *person, op.at_ns) {
                return false;
            }
            order(policy);
        }
        MembershipOpCommand::Project { .. } => {
            if let Ok(next) = tilo_failure::ignored!(
                "a change this author's grants do not allow takes the whole operation with it",
                effect(ledger.doc, policy, op)
            ) {
                *policy = next;
                order(policy);
            }
        }
    }
    true
}

/// [`replay`] of the workspace `doc` declares itself (its `room` row).
#[must_use]
pub fn replay_declared(doc: &AutoCommit) -> WorkspacePolicy {
    let workspace = doc
        .get(ROOT, "room")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
        .unwrap_or_default();
    replay(doc, &workspace)
}

/// The people the workspace `doc` governs lists as its owner and members, by
/// person id: who still holds what they made on its desktop
/// ([`tilo_desktop_model::ownership::released`]). `None` when it lists nobody
/// (not heard yet), so nothing is taken as released before the list arrives.
#[must_use]
pub fn people(doc: &AutoCommit) -> Option<BTreeSet<String>> {
    let policy = replay_declared(doc);
    let people: BTreeSet<String> = policy.members.into_keys().chain(policy.owner).collect();
    (!people.is_empty()).then_some(people)
}

/// The membership a [`MembershipOpCommand::Project`] operation `op` leaves
/// `policy` at ([`op_effect`]), or the first of its changes its author's grants
/// do not allow, with why.
fn effect(
    doc: &AutoCommit,
    policy: &WorkspacePolicy,
    op: &SignedMembershipOp,
) -> Result<WorkspacePolicy, Box<(Change, String)>> {
    let MembershipOpCommand::Project { changes } = &op.command else {
        return Ok(policy.clone());
    };
    let person = op.person();
    let cx = Context::at(op.at_ns / 1_000_000);
    let mut next = policy.clone();
    let foreign = op.author.capabilities.foreign && !next.is_owner(&person);
    let device = hex::encode(op.author.device_public_key);
    for change in changes {
        // The change is decided against the membership as the changes before
        // it left it, and applied to the operation's own copy. One the
        // workspace's grants do not allow may still be the member's by their
        // own rule: a share, its end, or a saved set they assign.
        let mut after = next.clone();
        let authority = next.person_on_device(&person, foreign);
        next = match after.apply(&person, &authority, change.clone(), &cx, false) {
            Ok(()) => after,
            Err(refusal) => crate::workspace::share::members_own(
                &next,
                || tilo_desktop_model::projection::workspace_presets_of(doc),
                (&person, &device, &authority, &cx),
                change,
            )
            .map_err(|own| Box::new((change.clone(), format!("{refusal:?}; {own}"))))?,
        };
    }
    Ok(next)
}

/// Why `op`, written on top of the membership `policy`, changes nothing: the
/// first of its changes its author's grants do not allow, and the answer it
/// got. `None` when it applies.
#[must_use]
pub fn refused(
    doc: &AutoCommit,
    policy: &WorkspacePolicy,
    op: &SignedMembershipOp,
) -> Option<String> {
    effect(doc, policy, op)
        .err()
        .map(|refused| format!("{:?} was refused: {}", refused.0, refused.1))
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
        // An operation that does not prove its writer is refused, and one stored
        // under a key that is not its own id with it.
        let (id, _) = SignedMembershipOp::verified_stored(&bytes)?;
        if id.to_key() != key {
            return Err("a membership operation is stored under another operation's id".into());
        }
        added = added.saturating_add(1);
    }
    Ok(added)
}

/// The workspace document's membership operations as stored, by operation key:
/// each under its own root key ([`super::op::op_key`]).
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
    held
}
