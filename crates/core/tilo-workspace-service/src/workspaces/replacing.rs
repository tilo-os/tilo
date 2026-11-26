//! A record whose type names its readers is moved into a document sealed to them when a
//! workspace opens, where it was held in the document every member reads
//! ([`tilo_resource_store::Store::misplaced`]): a draft written before its type named its readers
//! reaches every member's device from there, and its history is listed to them.
//!
//! The move is derived from what it moves, not from the moment it runs: its actor, moment and
//! invocation come from the record's kind, its id, the heads of the document it is held at and
//! the device's person. A device repairing the same record twice writes the one change, and two
//! devices repairing it at once put one id under one key in one document, where the store holds
//! one copy of it.

use collab_types::store::Resource;
use collab_types::store::wire::Command;
use tilo_resource_store::{Stamp, Store, Written};

use super::Workspaces;

impl Workspaces {
    /// Move each record `workspace` holds where its type's rule does not place it into the
    /// document its rule gives, as this device's signed move of it. What cannot be moved yet
    /// (its app is unknown here, or this device holds no keys of its readers) stays where it is
    /// and is tried again the next time the workspace opens.
    pub(crate) async fn replace_misplaced(&self, workspace: &str) -> Result<(), String> {
        let Ok(state) = self.workspace(workspace).await else {
            return Ok(());
        };
        let device = self.signing_device();
        // Who this device may write for: its own person, as every write it makes names.
        let invoker = hex::encode(device.cert.user_public_key);
        let misplaced = {
            let mut hosted = std::sync::Arc::clone(&state).lock_owned().await;
            let store = self.store_of(&mut hosted);
            store.misplaced_records()
        };
        let mut first: Option<String> = None;
        for (kind, id) in misplaced {
            let Some(app) = app_of_kind(&kind) else {
                tracing::debug!(%workspace, %kind, %id, "a misplaced record's app is unknown here");
                continue;
            };
            // Drafted and kept under one hold, drafted again after a wait for keys let it go.
            let (device, kind, id, invoker, app) = (&device, &kind, &id, &invoker, &app);
            let repaired =
                super::wanted_keys::write_with_keys(&state, move |mut hosted| async move {
                    let writes = repair_writes(&mut self.store_of(&mut hosted), kind, id, invoker)?;
                    if writes.is_empty() {
                        return Ok(());
                    }
                    self.keep_writes(hosted, std::sync::Arc::clone(device), app, writes, None)
                        .await
                        .map(drop)
                })
                .await;
            if let Err(error) = repaired {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// The app that keeps records of the store kind `kind`: the one this process named for it, else
/// the package its `tl.<app>.<Type>` kind spells. `None` for a kind naming no app.
fn app_of_kind(kind: &str) -> Option<String> {
    collab_types::app_surface::app_of_kind(kind)
        .or_else(|| Some(kind.strip_prefix("tl.")?.rsplit_once('.')?.0.to_owned()))
}

/// The writes that move the record `id` of `kind` out of the document every member reads into
/// the one its rule gives, drafted against `store`: its removal there, its content, and its
/// placement, under the actor and moment [`repair_under`] derives. Empty where the store holds
/// no such record, or holds it where its rule places it already.
pub(super) fn repair_writes(
    store: &mut Store,
    kind: &str,
    id: &str,
    invoker: &str,
) -> Result<Vec<Written>, String> {
    let Some(held) = store.index().get(kind, id).cloned() else {
        return Ok(Vec::new());
    };
    if !store.misplaced(kind, id) {
        return Ok(Vec::new());
    }
    let Some(heads) = store.index().heads(&held.doc) else {
        return Ok(Vec::new());
    };
    let (actor, second) = repair_under(kind, id, heads, invoker);
    let stamp = Stamp {
        verb: tilo_resource_store::MOVE_VERB.to_owned(),
        invocation: repair_invocation(kind, id, heads),
        // This device makes the move, as the person it may write for; it is made for the person
        // whose record it is, as its rule names them.
        invoker: invoker.to_owned(),
        acted_for: store.author_person(kind, id),
        moment: u64::try_from(second)
            .unwrap_or_default()
            .saturating_mul(1_000),
        ..Stamp::default()
    };
    // The rule names who reads it: a write of it names no readers of its own, and the store
    // places it in the document of the ones the rule gives.
    let command = Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: String::new(),
            parent: held.parent,
            order: held.order,
            fields: held.fields,
        },
        stamp,
    };
    store.drafting(Some((actor, second)));
    let drafted = tilo_resource_store::wire::draft(store, command);
    store.drafting(None);
    drafted.map_err(|refused| refused.error.to_string())
}

/// The actor and the second the repair of the record `id` of `kind`, held at `heads`, is made
/// by and at: derived from the three, so every device repairing the same record writes the same
/// change, and the workspace that takes both holds one copy.
fn repair_under(
    kind: &str,
    id: &str,
    heads: &[automerge::ChangeHash],
    invoker: &str,
) -> (automerge::ActorId, i64) {
    let hash = repair_hash(kind, id, heads, Some(invoker));
    let bytes = hash.as_bytes();
    let actor = automerge::ActorId::from(bytes.get(..16).unwrap_or_default());
    let within = bytes
        .get(16..20)
        .and_then(|four| <[u8; 4]>::try_from(four).ok())
        .map_or(0, u32::from_le_bytes);
    // A moment in the year this build repairs records in, so a repaired record sorts with the
    // changes around it rather than at the epoch, and the same moment on every device.
    let second = 1_770_000_000_i64.saturating_add(
        i64::from(within)
            .checked_rem(31_536_000)
            .unwrap_or_default(),
    );
    (actor, second)
}

/// The invocation every change of the repair of the record `id` of `kind`, held at `heads`,
/// names: history lists the move as one action of the workspace. Derived like the actor, so two
/// devices' repairs are one step.
fn repair_invocation(kind: &str, id: &str, heads: &[automerge::ChangeHash]) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        repair_hash(kind, id, heads, None).as_bytes(),
    )
    .to_string()
}

/// The hash of the record `id` of `kind`, held at `heads`: what the actor, the moment and the
/// invocation of its repair are derived from, the same on every device.
fn repair_hash(
    kind: &str,
    id: &str,
    heads: &[automerge::ChangeHash],
    invoker: Option<&str>,
) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo.record-repair\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(id.as_bytes());
    for head in heads {
        hasher.update(b"\0");
        hasher.update(head.as_ref());
    }
    // A change's actor is its writer's: the device's own person names its repair's, so two
    // devices' repairs are two changes, which put the record under one key in one document.
    if let Some(invoker) = invoker {
        hasher.update(b"\0");
        hasher.update(invoker.as_bytes());
    }
    hasher.finalize()
}
