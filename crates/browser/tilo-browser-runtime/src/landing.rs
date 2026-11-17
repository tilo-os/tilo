//! A record the workspace's host sent a page that no window of it holds open.
//!
//! The page keeps it in its storage for whoever opens it, and the windows that
//! waited for it look again. The host can hold a record with no change in it:
//! a window of an app that keeps nothing in its record (Settings) founds the
//! record empty, and the host keeps that empty document. Such a landing is all
//! the host holds of it, so it is the host's answer: the windows waiting for the
//! record open it as a new one, as they do when the host holds none of it.
//! Keeping nothing and telling nobody left those windows on their arrival view.
//!
//! The page's replication does this for every mount; it is here, apart from the
//! page's thread-locals, so it is tested on the host.

use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;
use ui::automerge::ChangeHash;
use ui::resource::{ResourceStore, WorkspaceAnswer};

/// What a page did with a record its host sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Landed {
    /// Kept for whoever opens it, at these heads.
    Kept(Vec<ChangeHash>),
    /// The record holds no change: nothing was kept, and the host has
    /// answered all it holds of it ([`answered`]).
    Empty,
}

/// Merge the host's document `bytes` of a record, which stands at `heads`,
/// into the copy `store` keeps under `key`. Into an empty slot it is kept as it
/// came, unloaded: opening the record loads and checks it once.
///
/// # Errors
/// The store refused the read or the write, or a document did not load.
pub(crate) fn keep(
    store: &dyn DurableStore,
    key: &StorageKey,
    bytes: Vec<u8>,
    heads: &[[u8; 32]],
) -> Result<Landed, String> {
    let stored = store.read(key).map_err(|error| error.to_string())?;
    let writer = stored.as_ref().and_then(|stored| stored.writer.clone());
    let stored = stored
        .map(|stored| stored.bytes)
        .filter(|bytes| !bytes.is_empty());
    let Some(stored) = stored else {
        if heads.is_empty() {
            return Ok(Landed::Empty);
        }
        let heads = heads.iter().copied().map(ChangeHash).collect();
        return store
            .write(key, WriteOp::Replace(bytes), writer.as_deref())
            .map(|_| Landed::Kept(heads))
            .map_err(|error| error.to_string());
    };
    // Either may be bytes a reader on this thread holds as a document (the workspace's replica
    // captured `bytes`): copies of those, not loads.
    let mut doc = tilo_automerge::shared::load(&stored).map_err(|error| error.to_string())?;
    let mut incoming = tilo_automerge::shared::load(&bytes).map_err(|error| error.to_string())?;
    let before = doc.get_heads();
    let shared: Vec<_> = before
        .iter()
        .copied()
        .filter(|head| incoming.get_change_by_hash(head).is_some())
        .collect();
    doc.load_incoming(&incoming.save_after(&shared))
        .map_err(|error| error.to_string())?;
    let heads = doc.get_heads();
    if heads.is_empty() {
        return Ok(Landed::Empty);
    }
    if heads == before {
        return Ok(Landed::Kept(heads));
    }
    let saved = doc.save();
    tilo_automerge::shared::hold(&saved, &doc);
    store
        .write(key, WriteOp::Replace(saved), writer.as_deref())
        .map(|_| Landed::Kept(heads))
        .map_err(|error| error.to_string())
}

/// The host answered `app`'s record `record` (its wire name) with `answer`:
/// each of `stores` that holds or wants it is told so. Says whether one was, so
/// the windows waiting for it are built again.
pub(crate) fn answered(
    stores: &[ResourceStore],
    app: &str,
    record: &str,
    answer: WorkspaceAnswer,
) -> bool {
    let mut answered = false;
    for store in stores {
        if store.app_and_home().0 != app {
            continue;
        }
        let found = store
            .open_resources()
            .into_iter()
            .map(|checkout| checkout.id)
            .chain(store.wanted_resources())
            .find(|id| tilo_desktop_surface::host::records::wire_document(id) == record);
        if let Some(id) = found {
            store.note_workspace_answer(id, answer);
            answered = true;
        }
    }
    answered
}

#[cfg(test)]
mod tests;
