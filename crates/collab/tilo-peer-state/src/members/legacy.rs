//! The genesis import: a workspace whose members live in the legacy rows
//! starts its operation log from them.
//!
//! Before the log, a workspace's membership was the last-writer-wins state of
//! the document's own rows (`owner`, `members`, `grants`, `members_joined_v1`,
//! and the bots and delegations the identity rows carry). A workspace that has
//! those rows and no [`super::MEMBERS_KEY`] imports them as the first
//! operation: its effect is the row projection at the heads the operation
//! names, and every receiver recomputes it from the rows it holds.

use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use tilo_action_policy::WorkspacePolicy;

use super::op::{MembershipOpCommand, OpId, SignedMembershipOp};

/// The workspace document as `legacy_heads` left it, or `None` when this
/// device holds no such history.
#[must_use]
pub fn at_heads(doc: &AutoCommit, legacy_heads: &[Vec<u8>]) -> Option<AutoCommit> {
    let heads = legacy_heads
        .iter()
        .map(|bytes| ChangeHash::try_from(bytes.as_slice()).ok())
        .collect::<Option<Vec<_>>>()?;
    let mut forked = doc.clone();
    forked.fork_at(&heads).ok()
}

/// Whether `doc` still has to import its legacy rows: it names members and
/// holds no operation at all.
#[must_use]
pub fn needs_genesis(doc: &AutoCommit) -> bool {
    if !super::replay::raw(doc).is_empty() {
        return false;
    }
    doc.get(ROOT, tilo_desktop_model::projection::WORKSPACE_MEMBERS_KEY)
        .ok()
        .flatten()
        .is_some()
}

/// The genesis operation for `membership`, written by `device` at the heads
/// `legacy_heads` of the history the workspace began at (empty for a workspace
/// created with it).
pub fn genesis_op(
    device: &tilo_user_identity::DeviceCert,
    key: &ed25519_dalek::SigningKey,
    legacy_heads: &[automerge::ChangeHash],
    membership: &WorkspacePolicy,
    now_ns: u64,
) -> Result<SignedMembershipOp, String> {
    SignedMembershipOp::signed(
        device,
        key,
        Vec::new(),
        now_ns,
        MembershipOpCommand::Genesis {
            legacy_heads: legacy_heads
                .iter()
                .map(|head| head.as_ref().to_vec())
                .collect(),
            membership: Box::new(super::op::Membership::of(membership)),
        },
    )
}

/// Write `op` into `doc` under the key of its own id.
pub fn put_op(doc: &mut AutoCommit, op: &SignedMembershipOp) -> Result<OpId, String> {
    let id = op.id()?;
    doc.put(
        ROOT,
        super::op::op_key(id).as_str(),
        ScalarValue::Bytes(op.to_bytes()?),
    )
    .map_err(|error| error.to_string())?;
    Ok(id)
}
