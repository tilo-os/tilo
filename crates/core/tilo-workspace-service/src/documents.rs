//! Holding one workspace's documents: what a device reads of them and the
//! writes it authors, for whichever holder keeps them.
//!
//! A workspace service holds a workspace beside its store, and a page holds one
//! over its own storage; both read a record, decide whether it reads now, list
//! what there is, and write a record as their device. The reads are here, once,
//! over the replica ([`tilo_peer_state::StatePeer`]) and the device's channel
//! keys ([`device_keys::DeviceKeys`]); the write is the one all holders share
//! ([`tilo_peer_state::owner::admit_local_write`]); a holder answers only where
//! its replica and keys are.

pub mod device_keys;
pub mod held;
pub(crate) mod sealed;
#[cfg(all(feature = "session-pages", not(target_arch = "wasm32")))]
pub mod session_page;

pub use device_keys::{
    DeviceKeys, GroupKeys, GroupKeysAnswer, GroupKeysAnswerFuture, KeysChangedFuture, KeysFuture,
};
pub use held::HeldDocuments;

use std::sync::Arc;

use automerge::ReadDoc as _;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_peer_state::{KeptDocument, PeerDocumentReadError, StatePeer};

use sealed::DocumentKeys;
use tilo_lock::MutexRecover as _;
use web_time::SystemTime;

/// The device a document was opened as, and the epoch of the keys that opened it, `None` for one
/// held in the clear: what was opened under keys the device holds no more (the workspace rotated
/// them, as removing a member does) is let go, and the resource opens whole under the keys held
/// now. Keys that moved on to a later epoch and still hold that one open the same history, as
/// they do after a member is admitted: what was opened stands, and only what was sealed since is
/// opened.
pub(crate) type OpenedWith = ([u8; 32], Option<u64>);

/// One resource as this device last opened it; `None` until a read opened it.
pub(crate) type Kept = Option<(OpenedWith, KeptDocument)>;

/// One resource as this device last opened it; `None` until a read opened it.
type KeptSlot = Arc<std::sync::Mutex<Kept>>;

/// Most bytes [`Opened`] keeps opened ([`KeptDocument::size`]), the least recently read let go
/// first. It is never bounded by how many resources there are: a tracker of thousands of issues
/// is kept in eighty parts or more, read part after part by each reader in turn (the tracker's
/// own copy, a caller's, the search index), and a bound below the number of parts let each
/// reader's pass push out the parts the next was about to read, so every reader opened every
/// part's whole sealed history again.
const OWN_OPENED_BYTES: usize = 256 * 1024 * 1024;

/// Sealed resources as a device last opened them, by workspace, app and resource, so the next
/// read opens only what was sealed since: a resource a session follows is opened once per write,
/// not whole per read. What it holds is plaintext the device's keys opened, kept in its memory
/// alone.
#[derive(Default)]
pub(crate) struct Opened(std::sync::Mutex<Openings>);

/// What [`Opened`] keeps, and how many reads it counted.
#[derive(Default)]
struct Openings {
    entries: std::collections::HashMap<OpenedResource, OpenedEntry>,
    reads: u64,
}

/// A resource of a workspace, by its app: `(workspace, app, resource)`.
type OpenedResource = (String, String, String);

/// One resource [`Opened`] keeps: what is opened of it, the read that last asked for it, and the
/// bytes it kept after its last read.
struct OpenedEntry {
    kept: KeptSlot,
    read: u64,
    size: usize,
}

impl Opened {
    /// What is kept of `app`'s `resource` of `workspace`, held while it is read.
    pub(crate) fn of(&self, workspace: &str, app: &str, resource: &str) -> KeptSlot {
        let key = (workspace.to_owned(), app.to_owned(), resource.to_owned());
        let mut kept = self.0.lock_recover();
        kept.reads = kept.reads.wrapping_add(1);
        let read = kept.reads;
        let entry = kept.entries.entry(key).or_insert_with(|| OpenedEntry {
            kept: KeptSlot::default(),
            read,
            size: 0,
        });
        entry.read = read;
        let opened = Arc::clone(&entry.kept);
        drop(kept);
        opened
    }

    /// `app`'s `resource` of `workspace` keeps `size` bytes now: what was read least recently
    /// is let go while everything kept comes to more than [`OWN_OPENED_BYTES`].
    pub(crate) fn kept(&self, (workspace, app, resource): (&str, &str, &str), size: usize) {
        let key = (workspace.to_owned(), app.to_owned(), resource.to_owned());
        let mut kept = self.0.lock_recover();
        if let Some(entry) = kept.entries.get_mut(&key) {
            entry.size = size;
        }
        let mut total: usize = kept.entries.values().map(|entry| entry.size).sum();
        while total > OWN_OPENED_BYTES {
            let Some(oldest) = kept
                .entries
                .iter()
                .filter(|(other, _)| **other != key)
                .min_by_key(|(_, entry)| entry.read)
                .map(|(other, _)| other.clone())
            else {
                break;
            };
            if let Some(gone) = kept.entries.remove(&oldest) {
                total = total.saturating_sub(gone.size);
            }
        }
    }
}

/// Wait for `future` with this thread, or on a thread of its own where this one already drives a
/// runtime: what a read that must not jam its caller's executor waits with.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn on_its_own<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return futures::executor::block_on(future);
    }
    std::thread::scope(
        |scope| match scope.spawn(|| futures::executor::block_on(future)).join() {
            Ok(output) => output,
            Err(panic) => std::panic::resume_unwind(panic),
        },
    )
}

/// Wait for `future` with this thread: a page drives no runtime beside it.
#[cfg(target_arch = "wasm32")]
pub(crate) fn on_its_own<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    futures::executor::block_on(future)
}

/// What a read of one document needs before it opens anything: the keys it opens it under, or
/// the answer a holder hands out without opening it.
pub(crate) enum Prepared {
    /// Handed out as it stands: absent from this device's copy, or still on its way.
    Answer(DocumentRead),
    /// The keys it opens under, `None` for one held in the clear, and the device and epoch they
    /// are.
    Keys {
        used: OpenedWith,
        keys: Option<Box<DocumentKeys>>,
    },
}

/// What reading `app`'s `resource` of `workspace` as the device `reader` needs first: whether it
/// is here at all, and the keys it opens under. A holder asks this, takes what it keeps of the
/// resource, and finishes with [`finish`], so nothing holds a lock across the device's key
/// service answering.
///
/// # Errors
///
/// This device may not read the resource, or it is sealed and this device holds no keys of it.
pub(crate) async fn prepare(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
    reader: [u8; 32],
) -> Result<Prepared, String> {
    if !peer.holds_document(app, resource) {
        // A record a workspace's store keeps arrives sealed for its channel, and is the store's
        // to read once this device holds the channel's keys: until then it is on its way, never
        // absent, or a writer founds the record again beside it.
        if peer.holds_sealed_content_of(app, resource) {
            return Ok(Prepared::Answer(DocumentRead::Pending));
        }
        return Ok(Prepared::Answer(DocumentRead::Absent));
    }
    if peer.checkpoint_pending(app, resource) {
        return Ok(Prepared::Answer(DocumentRead::Pending));
    }
    let keys = match content_keys(peer, device, workspace, (app, resource))
        .await
        .map_err(|error| error.to_string())?
    {
        Sealing::Due => return Ok(Prepared::Answer(DocumentRead::Pending)),
        Sealing::Clear => None,
        Sealing::Sealed(keys) => Some(keys),
    };
    let used = (reader, keys.as_deref().map(DocumentKeys::epoch));
    Ok(Prepared::Keys { used, keys })
}

/// `app`'s `resource` of `workspace` as [`prepare`] left it, opened for the device `reader`: the
/// document whole, or, for `since` heads this device holds, only what changed after them.
///
/// `kept` is what the previous read of the same resource opened, so a read that follows one opens
/// only the writes sealed since rather than the whole history again; it is left holding what this
/// read opened, under the device and key epoch that opened it.
///
/// # Errors
///
/// Its history does not open, or it exceeds `maximum`.
pub(crate) fn finish(
    peer: &StatePeer,
    (app, resource): (&str, &str),
    reader: [u8; 32],
    since: &[[u8; 32]],
    maximum: usize,
    prepared: Prepared,
    kept: &mut Kept,
) -> Result<DocumentRead, String> {
    let Prepared::Keys { used, keys } = prepared else {
        let Prepared::Answer(read) = prepared else {
            return Err("a read is prepared or answered".to_owned());
        };
        return Ok(read);
    };
    let mut opened = keys.as_deref().map(DocumentKeys::open);
    // What this device opened of the resource before is read on, so a read opens only the
    // writes sealed since rather than the whole history again.
    let mut held = held(kept.take(), used, keys.as_deref());
    let read = match peer.open_document(
        reader,
        (app, resource),
        tilo_clock::wall(),
        maximum,
        opened
            .as_mut()
            .map(|open| open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>),
        &mut held,
    ) {
        Ok(document) => answer(document, since, maximum),
        // A write this copy knows of has not reached it yet: the next pass reads it.
        Err(PeerDocumentReadError::Incomplete) => Ok(Answer::Read(DocumentRead::Pending)),
        Err(refused) => Err(refused.to_string()),
    };
    let read = match read {
        Ok(Answer::Whole { revision, heads }) => {
            whole(peer, &mut held, reader, (app, resource), maximum).map(|captured| {
                DocumentRead::Whole {
                    revision,
                    heads,
                    bytes: captured.bytes,
                    hint: captured.hint,
                }
            })
        }
        Ok(Answer::Read(read)) => Ok(read),
        Err(error) => Err(error),
    };
    if held.is_open() {
        *kept = Some((used, held));
    }
    read
}

/// What the previous read of `route` opened, where it opened it as the same device under keys
/// `keys` still hold: the epoch it was opened under, or none for both when it is in the clear.
/// Opened under `used`'s epoch or an earlier one the keys hold, it reads on from there.
fn held(kept: Kept, used: OpenedWith, keys: Option<&DocumentKeys>) -> KeptDocument {
    kept.filter(|((device, epoch), _)| {
        *device == used.0
            && match (*epoch, keys) {
                (None, None) => true,
                (Some(epoch), Some(keys)) => keys.keys.holds_epoch(epoch),
                _ => false,
            }
    })
    .map(|(_, kept)| kept)
    .unwrap_or_default()
}

/// Whether `app`'s `resource` of `workspace` reads for this device now: held here, its checkpoint
/// and its keys arrived, and the sealed writes held cover its history. One this device may not
/// read, or that does not open, waits for nothing: its read says why. Opening a sealed resource
/// costs its whole history the first time after a start, seconds for a part of a large tracker, so
/// nothing that only asks whether it could read one opens it.
pub(crate) async fn readable(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
    reader: [u8; 32],
    now: SystemTime,
) -> bool {
    if !peer.holds_document(app, resource) || peer.checkpoint_pending(app, resource) {
        return false;
    }
    let sealed = match content_keys(peer, device, workspace, (app, resource)).await {
        Ok(Sealing::Due) => return false,
        Ok(Sealing::Clear) => false,
        Ok(Sealing::Sealed(_)) => true,
        Err(_) => return true,
    };
    !matches!(
        peer.document_opens(reader, (app, resource), now, sealed),
        Err(PeerDocumentReadError::Incomplete)
    )
}

/// Every document `peer` holds, by its app and route.
pub(crate) fn listing(peer: &StatePeer) -> Vec<(String, String)> {
    peer.plaintext_routes()
        .into_iter()
        .chain(peer.sealed_routes())
        .collect()
}

/// Whether `peer` holds anything of `app`'s `resource`, committed or sealed alike.
pub(crate) fn holds(peer: &StatePeer, app: &str, resource: &str) -> bool {
    peer.holds_document(app, resource)
}

/// What [`read`] answers from a document it opened.
enum Answer {
    /// Answered from its heads alone: unchanged, or the changes after the reader's.
    Read(DocumentRead),
    /// The document whole, at these heads: captured from what is kept of it.
    Whole {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
    },
}

/// What `document` answers a reader holding `since`: only what changed after those heads, or the
/// document whole where the reader holds none, or heads this device does not hold.
fn answer(
    document: &automerge::Automerge,
    since: &[[u8; 32]],
    maximum: usize,
) -> Result<Answer, String> {
    let heads: Vec<[u8; 32]> = document.get_heads().iter().map(|head| head.0).collect();
    let revision = tilo_peer_state::committed_document_revision(&document.get_heads());
    // A reader already at the document's heads has nothing to take: answered from the heads
    // alone, since looking each of them up in the history builds the history's change index,
    // which for a large record costs a tenth of a second or more on every read that follows a
    // landing that changed nothing it reads, as a member's admission is for every record.
    if !since.is_empty()
        && revision == tilo_peer_state::committed_document_revision(&since_hashes(since))
    {
        return Ok(Answer::Read(DocumentRead::Unchanged(revision)));
    }
    // A reader's heads this device does not hold are answered with the whole document: the
    // changes it would send are not the complete ones after them.
    let holds_since = since.iter().all(|head| {
        document
            .get_change_by_hash(&automerge::ChangeHash(*head))
            .is_some()
    });
    if since.is_empty() || !holds_since {
        return Ok(Answer::Whole { revision, heads });
    }
    let since = since_hashes(since);
    let bytes = tilo_peer_state::changes_since(document, &since, maximum)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() && heads == since.iter().map(|head| head.0).collect::<Vec<_>>() {
        return Ok(Answer::Read(DocumentRead::Unchanged(revision)));
    }
    Ok(Answer::Read(DocumentRead::Changes {
        revision,
        heads,
        bytes,
    }))
}

/// `since` as the history's change hashes.
fn since_hashes(since: &[[u8; 32]]) -> Vec<automerge::ChangeHash> {
    since
        .iter()
        .map(|head| automerge::ChangeHash(*head))
        .collect()
}

/// `app`'s `resource` saved whole for `reader`, at most `maximum` bytes: what its last capture
/// handed out with the changes since appended where it is kept opened, rather than the document
/// saved again; saved from the copy itself where it is held in the clear. Either way its bytes
/// come with the load hint this device's store made for them, so the reader skips rebuilding
/// every change's hash.
fn whole(
    peer: &StatePeer,
    kept: &mut KeptDocument,
    reader: [u8; 32],
    route: (&str, &str),
    maximum: usize,
) -> Result<tilo_peer_state::Captured, String> {
    if let Some(captured) = kept.capture(maximum) {
        return captured.map_err(|error| error.to_string());
    }
    let mut unkept = KeptDocument::default();
    let document = peer
        .open_document(
            reader,
            route,
            tilo_clock::wall(),
            maximum,
            None,
            &mut unkept,
        )
        .map_err(|error| error.to_string())?;
    tilo_peer_state::saved_whole(document, maximum).map_err(|error| error.to_string())
}

/// How `app`'s `resource` of a workspace is opened on this device.
pub(crate) enum Sealing {
    /// It is held in the clear.
    Clear,
    /// It is sealed, and opens with these keys.
    Sealed(Box<DocumentKeys>),
    /// It is sealed for a group this device takes part in, under keys it does not hold yet: the
    /// welcome or commit that brings them is on its way, and it reads once that arrived.
    Due,
}

/// How `app`'s `resource` of `workspace` opens on this device: in the clear, with the keys of the
/// group it is sealed for, or once the keys this device is due arrive. Nothing waits here: a read
/// answered [`Sealing::Due`] says the resource is on its way, and its reader reads it again when
/// the device's keys change, as an app's held read and the session's next pass do.
pub(crate) async fn content_keys(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
) -> Result<Sealing, PeerDocumentReadError> {
    if !peer.publication().is_sealed_document(app, resource) {
        return Ok(Sealing::Clear);
    }
    let Some(device) = device else {
        return Err(PeerDocumentReadError::Encrypted);
    };
    let scoped = peer.publication().has_document_keys(app, resource);
    if scoped || neighbour_wire::KeyScope::has_named_readers(resource) {
        let keys =
            sealed::read_document_keys(device, workspace, (app, resource), scoped, false).await;
        return match keys {
            Ok(keys)
                if peer
                    .publication()
                    .sealed_keys_available(app, resource, |metadata| keys.covers(metadata)) =>
            {
                Ok(Sealing::Sealed(Box::new(keys)))
            }
            Ok(keys) => {
                device.behind(keys.keys.workspace(), keys.epoch());
                Ok(Sealing::Due)
            }
            Err(error) => {
                tracing::debug!(%workspace, %app, %resource, %error, "a sealed resource waits for its document keys");
                Ok(Sealing::Due)
            }
        };
    }
    let epochs = peer.publication().sealed_epochs(app, resource);
    match device
        .group_keys(workspace, collab_types::EVERY_MEMBER, false)
        .await
    {
        None => Err(PeerDocumentReadError::Encrypted),
        // Taking part in the group and holding none of its keys yet, or not yet an epoch a write
        // was sealed under, is a member's state between its admission and the welcome or commit
        // its group sends it.
        Some(Err(error)) => {
            tracing::debug!(%workspace, %app, %resource, %error, "a sealed resource waits for its keys");
            Ok(Sealing::Due)
        }
        Some(Ok(keys)) if !epochs.iter().all(|epoch| keys.holds_epoch(*epoch)) => {
            tracing::debug!(%workspace, %app, %resource, held = keys.epoch(), ?epochs,
                "a sealed resource waits for the keys of an epoch it was written under");
            device.behind(keys.workspace(), keys.epoch());
            Ok(Sealing::Due)
        }
        Some(Ok(keys)) => Ok(Sealing::Sealed(Box::new(DocumentKeys::app_document(
            keys,
            workspace,
            (app, resource),
        )))),
    }
}

#[cfg(test)]
mod opened_tests {
    use std::sync::Arc;

    use super::{OWN_OPENED_BYTES, Opened};

    /// Each reader of a tracker's parts after a restart (its own copy, a caller's, the search
    /// index) reads every part in turn: what the first opened stays opened for the next however
    /// many parts there are. A bound by count below a tracker's parts let each pass push out the
    /// parts the next was about to read.
    #[test]
    fn a_trackers_parts_stay_opened_from_one_reader_to_the_next() {
        let opened = Opened::default();
        let parts: Vec<String> = (0..120).map(|at| format!("part.{at}")).collect();
        let first: Vec<_> = parts
            .iter()
            .map(|part| {
                let kept = opened.of("workspace", "tasks", part);
                opened.kept(("workspace", "tasks", part), 1024 * 1024);
                kept
            })
            .collect();
        for (part, kept) in parts.iter().zip(&first) {
            assert!(
                Arc::ptr_eq(&opened.of("workspace", "tasks", part), kept),
                "{part} was let go and is opened again"
            );
        }
    }

    /// Past its bytes, what was read least recently is let go first.
    #[test]
    fn past_its_bytes_the_least_recently_read_is_let_go() {
        let opened = Opened::default();
        let third = OWN_OPENED_BYTES / 3;
        let a = opened.of("workspace", "app", "a");
        opened.kept(("workspace", "app", "a"), third);
        let b = opened.of("workspace", "app", "b");
        opened.kept(("workspace", "app", "b"), third);
        // Read again: now the more recent of the two.
        assert!(Arc::ptr_eq(&opened.of("workspace", "app", "a"), &a));
        opened.of("workspace", "app", "c");
        opened.kept(("workspace", "app", "c"), third);
        opened.of("workspace", "app", "d");
        opened.kept(("workspace", "app", "d"), third);
        assert!(
            Arc::ptr_eq(&opened.of("workspace", "app", "a"), &a),
            "the one read again stays"
        );
        assert!(
            !Arc::ptr_eq(&opened.of("workspace", "app", "b"), &b),
            "the one read least recently went"
        );
    }
}
