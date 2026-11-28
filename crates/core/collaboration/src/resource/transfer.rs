//! Branching: a whole workspace forked into a new one, each of its resources forked with it.
//! A record moving between workspaces is the store's own `move`, which founds it afresh and
//! leaves its history behind. A branch is the one place a record crosses with its history.

use automerge::{AutoCommit, ObjType};
use tilo_automerge::ApplyIncoming as _;

use super::{DocumentCheckout, ResourceOpenError, identity, validate_identity};

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

/// A branch's copy of `source`, the content-only record `checkout` names of `from_workspace`, in
/// `to_workspace`: its whole history under a writer of its own, its founding derived again for the
/// workspace it goes to, so the copy and a record an app of the branch makes of the same kind and
/// id before it arrives meet as one document. Its id stays: a resource id names a document only
/// inside its workspace ([`super::workspace_scope`]), so every reference one of its resources
/// makes to another still names the same one.
///
/// # Errors
/// `to_workspace` is empty, `source` is not the record `checkout` names in `from_workspace`, its
/// document names an identity of its own, or its history cannot be re-founded.
pub fn branch(
    source: &AutoCommit,
    checkout: &DocumentCheckout,
    from_workspace: &str,
    to_workspace: &str,
) -> Result<AutoCommit, TransferError> {
    if to_workspace.is_empty() {
        return Err(TransferError::NotTransferable(
            "a branch is made into a workspace",
        ));
    }
    if identity(source).is_some() {
        return Err(TransferError::NotTransferable(
            "a record that names its identity in its own document is founded afresh in a branch",
        ));
    }
    validate_identity(source, checkout, from_workspace)?;
    refounded(source, checkout, from_workspace, to_workspace)
}

/// `source` re-founded in `to_workspace` on the chain its kind, id and workspace derive there
/// ([`tilo_resource::founding::chain`]), with every change after that chain replayed on it.
fn refounded(
    source: &AutoCommit,
    checkout: &DocumentCheckout,
    from_workspace: &str,
    to_workspace: &str,
) -> Result<AutoCommit, TransferError> {
    use automerge::legacy::{ElementId, Key, ObjectId, OpId, OpType};

    let kind = checkout.kind.as_str();
    let id = checkout.id.route();
    let changes = source.clone().get_changes(&[]);
    let Some(founding) = changes.first() else {
        return Err(TransferError::NotTransferable(
            "a record with no history is no branch's",
        ));
    };
    // The containers its founding made, read from it: the branch derives theirs again.
    let mut containers: Vec<(String, ObjType)> = founding
        .decode()
        .operations
        .iter()
        .filter_map(|op| match (&op.obj, &op.key, &op.action) {
            (ObjectId::Root, Key::Map(field), OpType::Make(ty)) => Some((field.to_string(), *ty)),
            _ => None,
        })
        .collect();
    containers.sort_by(|a, b| a.0.cmp(&b.0));
    containers.dedup_by(|a, b| a.0 == b.0);
    // The version steps the source stands on, told by the actor each derives under.
    let step_actor = |version: u32| {
        tilo_resource::genesis::actor(id, kind, &format!("version {version} in {from_workspace}"))
    };
    let mut version: u32 = 0;
    while changes
        .iter()
        .any(|change| change.actor_id() == &step_actor(version.saturating_add(1)))
    {
        version = version.saturating_add(1);
    }
    let chain = tilo_resource::founding::chain(
        tilo_resource::founding::Scope {
            workspace: to_workspace,
            kind,
            id,
        },
        version,
        &containers,
    )
    .map_err(|error| TransferError::Automerge(error.to_string()))?;
    let from_founding = tilo_resource::founding::Scope {
        workspace: from_workspace,
        kind,
        id,
    }
    .founding_actor();
    let mut hashes = std::collections::HashMap::with_capacity(changes.len());
    let mut actors = std::collections::HashMap::with_capacity(chain.len());
    for change in &changes {
        let actor = change.actor_id();
        let target = if actor == &from_founding {
            chain.first()
        } else {
            (1..=version).find_map(|version| {
                (actor == &step_actor(version))
                    .then(|| {
                        usize::try_from(version)
                            .ok()
                            .and_then(|index| chain.get(index))
                    })
                    .flatten()
            })
        };
        let Some(target) = target else {
            continue;
        };
        hashes.insert(change.hash(), target.hash());
        actors.insert(actor.clone(), target.actor_id().clone());
    }
    let rename = |id: &mut OpId| {
        if let Some(to) = actors.get(&id.1) {
            id.1 = to.clone();
        }
    };
    let mut replayed = Vec::with_capacity(changes.len());
    for change in &changes {
        if actors.contains_key(change.actor_id()) {
            continue;
        }
        let was = change.hash();
        let mut expanded = change.decode();
        expanded.hash = None;
        expanded.deps = expanded
            .deps
            .iter()
            .map(|dep| hashes.get(dep).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                TransferError::Automerge(format!("change {was} stands before its base"))
            })?;
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
        }
        let change = automerge::Change::from(expanded);
        hashes.insert(was, change.hash());
        replayed.push(change);
    }
    let mut history = chain;
    history.extend(replayed);
    let mut doc = AutoCommit::new().with_actor(source.get_actor().clone());
    doc.apply_incoming(history)
        .map_err(|error| TransferError::Automerge(error.to_string()))?;
    validate_identity(&doc, checkout, to_workspace)?;
    Ok(doc)
}
