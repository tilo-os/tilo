//! Branching: a whole workspace forked into a new one, each of its resources forked with it.
//!
//! A resource id names a document only inside the workspace whose store holds it
//! ([`super::workspace_scope`]). A branch is the one place a document crosses into another
//! workspace with its history: a record moving between workspaces is the store's own `move`,
//! which founds it afresh in the workspace it goes to.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ScalarValue};
use tilo_automerge::ApplyIncoming as _;

use super::{ResourceOpenError, identity, meta_obj, validate_identity};

/// Why a document could not be branched.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    Open(#[from] ResourceOpenError),
    #[error("{0}")]
    NotTransferable(&'static str),
    #[error("automerge: {0}")]
    Automerge(String),
}

/// A branch's copy of `source`, a document of a workspace branched into `to_workspace`: its whole
/// history, under a writer of its own, with one more change naming `to_workspace` as the workspace
/// it is in when it names one at all. Its id stays: a resource id names a document only inside its
/// workspace ([`super::workspace_scope`]), and a branch is a whole workspace forked, so every
/// reference one of its resources makes to another still names the same one. Where it was minted
/// (`home`) stays too, as its provenance, but for a resource any app makes from its kind alone,
/// which is minted again in the branch ([`minted_again`]). `source` is not changed.
///
/// # Errors
/// `to_workspace` is empty, or the document's metadata cannot name it.
pub fn branch(source: &AutoCommit, to_workspace: &str) -> Result<AutoCommit, TransferError> {
    if to_workspace.is_empty() {
        return Err(TransferError::NotTransferable(
            "a branch is made into a workspace",
        ));
    }
    // A fork continues the same change graph under a writer of its own, so a change made in the
    // branch never takes a number a change in the source already has.
    let mut doc = match minted_again(source, to_workspace)? {
        Some(mut minted) => minted.fork(),
        None => source.clone().fork(),
    };
    let Some(scoped) = super::workspace_scope(&doc) else {
        return Ok(doc);
    };
    if scoped == to_workspace {
        return Ok(doc);
    }
    let automerge = |error: automerge::AutomergeError| TransferError::Automerge(error.to_string());
    let meta = meta_obj(&mut doc).ok_or(ResourceOpenError::IdentityMismatch)?;
    doc.put(&meta, super::WORKSPACE, to_workspace)
        .map_err(automerge)?;
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Transfer.change_message()),
    );
    Ok(doc)
}

/// `source` minted again in `to_workspace`, when it is a resource any app makes from its kind alone
/// where it holds none ([`collab_types::workspace_document_id`]: the workspace's emoji, its agents)
/// and its genesis made it: its whole history, but with the genesis an app of the branch makes
/// ([`super::genesis`]) as its first change, and every change after it built on that one. An app of
/// the branch that opens the resource before the branch's copy reached it makes the same genesis, so
/// the two meet as one resource. `None` for any other document, which keeps where it was minted.
fn minted_again(
    source: &AutoCommit,
    to_workspace: &str,
) -> Result<Option<AutoCommit>, TransferError> {
    use automerge::legacy::{ElementId, Key, ObjectId, OpId, OpType};

    let Some(checkout) = identity(source) else {
        return Ok(None);
    };
    if checkout.id.as_str() != collab_types::workspace_document_id(checkout.kind.as_str()) {
        return Ok(None);
    }
    let Some(home) = super::home(source).filter(|home| home != to_workspace) else {
        return Ok(None);
    };
    let from = super::genesis_actor_of(&checkout.id, &checkout.kind, &home);
    let to = super::genesis_actor_of(&checkout.id, &checkout.kind, to_workspace);
    let changes = source.clone().get_changes(&[]);
    // Its genesis made it: the genesis actor's one change comes first and depends on nothing.
    let made_by_genesis = changes
        .first()
        .is_some_and(|first| first.actor_id() == &from && first.deps().is_empty())
        && changes
            .iter()
            .filter(|change| change.actor_id() == &from)
            .count()
            == 1;
    if !made_by_genesis {
        return Ok(None);
    }
    let rename = |id: &mut OpId| {
        if id.1 == from {
            id.1 = to.clone();
        }
    };
    let mut hashes = std::collections::HashMap::new();
    let mut minted = Vec::with_capacity(changes.len());
    for change in changes {
        let was = change.hash();
        let mut expanded = change.decode();
        expanded.hash = None;
        expanded.deps = expanded
            .deps
            .iter()
            .map(|dep| hashes.get(dep).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| TransferError::Automerge(format!("change {was} before its base")))?;
        let genesis = expanded.actor_id == from;
        if genesis {
            expanded.actor_id = to.clone();
        }
        for op in &mut expanded.operations {
            if let ObjectId::Id(id) = &mut op.obj {
                rename(id);
            }
            if let Key::Seq(ElementId::Id(id)) = &mut op.key {
                rename(id);
            }
            op.pred = op
                .pred
                .iter()
                .cloned()
                .map(|mut pred| {
                    rename(&mut pred);
                    pred
                })
                .collect();
            // The genesis names the workspace that minted it, and the one it is in.
            let names_home = match (&op.key, &op.action) {
                (Key::Map(key), OpType::Put(ScalarValue::Str(named))) => {
                    genesis
                        && ["home", super::WORKSPACE].contains(&key.as_str())
                        && named.as_str() == home
                }
                _ => false,
            };
            if names_home {
                op.action = OpType::Put(ScalarValue::from(to_workspace));
            }
        }
        let change = automerge::Change::from(expanded);
        hashes.insert(was, change.hash());
        minted.push(change);
    }
    let mut doc = AutoCommit::new();
    doc.apply_incoming(minted)
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    if super::genesis_actor(&doc).as_ref() != Some(&to) {
        return Err(ResourceOpenError::IdentityMismatch.into());
    }
    validate_identity(&doc, &checkout, to_workspace)?;
    Ok(Some(doc))
}
