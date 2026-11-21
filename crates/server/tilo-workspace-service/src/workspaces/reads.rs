//! The resources of the workspaces a service holds, read for a device.
//!
//! The service's own copy of a workspace is the one its apps' Opens and the session's
//! own reads answer with: what its governor keeps of a document whole, sealed writes opened with
//! the content keys the device the service is on holds. Every read is decided at the resource's
//! node as a member's read is ([`tilo_peer_state::StatePeer::check_document_read`]), for the
//! device whose certificate asks.

use automerge::ReadDoc as _;
#[cfg(feature = "apps")]
use collab_types::OwnResource;
#[cfg(feature = "apps")]
use tilo_peer_state::PeerDocumentCapture;
use tilo_peer_state::{
    KeptDocument, PeerDocumentReadError, StatePeer, changes_since, committed_document_revision,
    saved_whole,
};
#[cfg(feature = "apps")]
use tilo_user_identity::DeviceCert;

#[cfg(feature = "apps")]
use super::SystemTime;
use super::{DeviceKeys, Workspaces};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::DocumentRead;

impl Workspaces {
    /// `app`'s `resource` of `workspace` as this service's own device reads it: the document
    /// whole, or, for `since` heads this device holds, only what changed after them.
    ///
    /// # Errors
    ///
    /// This service holds no such workspace, this device may not read the resource, it is sealed
    /// and this device holds no keys of it, its history does not open, or it exceeds `maximum`.
    pub(crate) async fn own_read(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        since: &[[u8; 32]],
        maximum: usize,
    ) -> Result<DocumentRead, String> {
        let (peer, device) = self
            .replica(workspace)
            .ok_or("this service does not hold the workspace")?;
        if !peer.holds_document(app, resource) {
            return Ok(DocumentRead::Absent);
        }
        if peer.checkpoint_pending(app, resource) {
            return Ok(DocumentRead::Pending);
        }
        let reader = self
            .own_reader(workspace)
            .ok_or("this service acts as no device here")?;
        let keys = match content_keys(&peer, device.as_ref(), workspace, (app, resource))
            .await
            .map_err(|error| error.to_string())?
        {
            Sealing::Due => return Ok(DocumentRead::Pending),
            Sealing::Clear => None,
            Sealing::Sealed(keys) => Some(keys),
        };
        let mut opened = keys
            .as_deref()
            .map(super::sealed_documents::DocumentKeys::open);
        // What this device opened of the resource before is read on, so a read opens only the
        // writes sealed since rather than the whole history again; a second read of it waits
        // for the first and reads on from what that one opened.
        let used = (
            reader,
            keys.as_deref()
                .map(super::sealed_documents::DocumentKeys::epoch),
        );
        let opening = self.inner.own_opened.of(workspace, app, resource);
        let mut held = opening.lock_recover();
        let mut kept = match held.take() {
            Some((opened_with, kept)) if opened_with == used => kept,
            _ => KeptDocument::default(),
        };
        let read = match peer.open_document(
            reader,
            (app, resource),
            tilo_clock::wall(),
            maximum,
            opened.as_mut().map(|open| {
                open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>
            }),
            &mut kept,
        ) {
            Ok(document) => answer(document, since, maximum),
            // A write this copy knows of has not reached it yet: the next pass reads it.
            Err(PeerDocumentReadError::Incomplete) => Ok(Answer::Read(DocumentRead::Pending)),
            Err(refused) => Err(refused.to_string()),
        };
        let read = match read {
            Ok(Answer::Whole { revision, heads }) => {
                whole(&peer, &mut kept, reader, (app, resource), maximum).map(|bytes| {
                    DocumentRead::Whole {
                        revision,
                        heads,
                        bytes,
                    }
                })
            }
            Ok(Answer::Read(read)) => Ok(read),
            Err(error) => Err(error),
        };
        let size = kept.size();
        if kept.is_open() {
            *held = Some((used, kept));
        }
        drop(held);
        self.inner.own_opened.kept((workspace, app, resource), size);
        read
    }

    /// Whether `app`'s `resource` of `workspace` reads for this service's own device now, decided
    /// as [`Self::own_read`] decides it before it opens anything: held here, its checkpoint and
    /// its keys arrived, and the sealed writes held cover its history. One this device may not
    /// read, or that does not open, waits for nothing: its read says why. Opening a sealed
    /// resource costs its whole history the first time after a start, seconds for a part of a
    /// large tracker, so nothing that only asks whether it could read one opens it.
    pub(crate) async fn own_readable(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
    ) -> bool {
        let Some((peer, device)) = self.replica(workspace) else {
            return false;
        };
        if !peer.holds_document(app, resource) || peer.checkpoint_pending(app, resource) {
            return false;
        }
        let Some(reader) = self.own_reader(workspace) else {
            return true;
        };
        let sealed = match content_keys(&peer, device.as_ref(), workspace, (app, resource)).await {
            Ok(Sealing::Due) => return false,
            Ok(Sealing::Clear) => false,
            Ok(Sealing::Sealed(_)) => true,
            Err(_) => return true,
        };
        !matches!(
            peer.document_opens(reader, (app, resource), tilo_clock::wall(), sealed),
            Err(PeerDocumentReadError::Incomplete)
        )
    }

    /// Every document `workspace` holds here, by its app and route.
    ///
    /// # Errors
    ///
    /// This service holds no such workspace.
    pub(crate) fn own_listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        Ok(held.replica.read(|peer| {
            peer.plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .collect()
        }))
    }

    /// The members' own records `workspace`'s copy here knows, by app and route, each with whose
    /// it is: none for a workspace this service does not hold.
    pub(crate) fn own_records(
        &self,
        workspace: &str,
    ) -> std::collections::BTreeMap<(String, String), [u8; 32]> {
        let Some(held) = self.inner.readings.held(workspace) else {
            return std::collections::BTreeMap::new();
        };
        held.replica.read(StatePeer::own_resources)
    }

    /// The workspaces this service holds, by their ids.
    pub(crate) fn held_workspaces(&self) -> Vec<String> {
        self.inner.readings.workspaces()
    }

    /// The device this service reads `workspace` as: its own where it governs the workspace, the
    /// device its member copy acts as where it joins one.
    fn own_reader(&self, workspace: &str) -> Option<[u8; 32]> {
        if self.inner.readings.held(workspace)?.member {
            return self.member_identity().ok().map(|device| device.device());
        }
        Some(self.governor().device_public_key)
    }

    /// Whether `workspace` seals every write of its resources: its device takes part in the
    /// workspace's key group and holds its keys.
    #[cfg(feature = "apps")]
    pub(crate) fn seals_resources(&self, workspace: &str) -> bool {
        self.inner
            .readings
            .held(workspace)
            .is_some_and(|held| held.device.is_some())
    }

    /// The member's own resource `app`'s `resource` in `workspace` is, when it is one.
    #[cfg(feature = "apps")]
    pub(crate) fn own_resource(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> Option<OwnResource> {
        let held = self.inner.readings.held(workspace)?;
        held.replica.read(|peer| peer.own_resource(app, resource))
    }

    /// Whether this service holds anything of `app`'s `resource` in `workspace`, committed or
    /// sealed alike.
    pub(crate) fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.inner
            .readings
            .held(workspace)
            .is_some_and(|held| held.replica.read(|peer| peer.holds_document(app, resource)))
    }

    /// The revision `app`'s `resource` of `workspace` stands at for the device `reader`: decided
    /// as a read is, handing nothing out. `None` while a checkpoint is arriving, which only a
    /// capture answers.
    ///
    /// # Errors
    ///
    /// The reader may not read it, the service holds no copy, or it is sealed and this device
    /// holds no keys of it.
    #[cfg(feature = "apps")]
    pub(crate) async fn resource_revision(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: [u8; 32],
    ) -> Result<Option<[u8; 32]>, String> {
        let (peer, device) = self
            .replica(workspace)
            .ok_or("this service does not hold the workspace")?;
        resource_revision(
            &peer,
            device.as_ref(),
            workspace,
            (app, resource),
            reader,
            tilo_clock::wall(),
        )
        .await
    }

    /// Whether `app`'s `resource` of `workspace` is still arriving: its governed checkpoint is
    /// cut in flight, so it reads once that body is admitted.
    #[cfg(feature = "apps")]
    pub(crate) fn resource_arriving(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.inner.readings.held(workspace).is_some_and(|held| {
            held.replica
                .read(|peer| peer.checkpoint_pending(app, resource))
        })
    }

    /// Whether `certificate`'s device may read `app`'s `resource` of `workspace` now: a device the
    /// workspace certifies, a current member, and the resource's Read permission as its governance
    /// grants it.
    #[cfg(feature = "apps")]
    pub(crate) fn authorizes_resource_read(
        &self,
        workspace: &str,
        certificate: &DeviceCert,
        app: &str,
        resource: &str,
    ) -> Result<(), String> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        let peer = held.replica.lock();
        let certified = peer.certificates().iter().any(|known| {
            known.device_public_key == certificate.device_public_key
                && known.user_public_key == certificate.user_public_key
        });
        if !certified {
            return Err("the reader is no device the workspace certifies".to_owned());
        }
        let checked = peer.check_document_read(
            certificate.device_public_key,
            app,
            resource,
            tilo_clock::wall(),
        );
        drop(peer);
        checked
    }

    /// `app`'s `resource` of `workspace` as the device `reader` reads it now: at most `maximum`
    /// bytes of it, its sealed writes opened with the service's content keys. `None` while it is
    /// on its way: sealed under keys this device is due and has not taken yet, or missing a write
    /// this device knows of. It reads once those arrive.
    ///
    /// # Errors
    ///
    /// The reader may not read it, the service holds no copy, it is sealed for a group this device
    /// takes no part in, its history does not open, or it exceeds `maximum`.
    #[cfg(feature = "apps")]
    pub(crate) async fn capture_resource(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: [u8; 32],
        maximum: usize,
    ) -> Result<Option<PeerDocumentCapture>, PeerDocumentReadError> {
        let (peer, device) = self
            .replica(workspace)
            .ok_or(PeerDocumentReadError::Missing)?;
        capture_resource(
            &peer,
            device.as_ref(),
            workspace,
            (app, resource),
            reader,
            tilo_clock::wall(),
            maximum,
        )
        .await
    }

    /// Whether a part of the chain `app`'s `resource` names is held here: the answer an Open of a
    /// chain's head is decided by.
    #[cfg(feature = "apps")]
    pub(crate) fn chain_head_has_part(&self, workspace: &str, app: &str, resource: &str) -> bool {
        let Ok(routes) = self.own_listing(workspace) else {
            return false;
        };
        routes.into_iter().any(|(other_app, other)| {
            other_app == app
                && matches!(
                    neighbour_wire::document_sync(&other),
                    neighbour_wire::DocumentSync::Part { base, .. }
                        if part_of(resource) == Some(base)
                )
        })
    }

    /// The replica of `workspace` and the content keys its device holds, taken in a moment: the
    /// governor stays free while a read opens a sealed history.
    fn replica(&self, workspace: &str) -> Option<(StatePeer, Option<DeviceKeys>)> {
        let held = self.inner.readings.held(workspace)?;
        Some((held.replica.read(StatePeer::clone), held.device))
    }
}

/// What [`Workspaces::own_read`] answers from a document it opened.
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
    let revision = committed_document_revision(&document.get_heads());
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
    let since: Vec<automerge::ChangeHash> = since
        .iter()
        .map(|head| automerge::ChangeHash(*head))
        .collect();
    let bytes = changes_since(document, &since, maximum).map_err(|error| error.to_string())?;
    if bytes.is_empty() && heads == since.iter().map(|head| head.0).collect::<Vec<_>>() {
        return Ok(Answer::Read(DocumentRead::Unchanged(revision)));
    }
    Ok(Answer::Read(DocumentRead::Changes {
        revision,
        heads,
        bytes,
    }))
}

/// `app`'s `resource` saved whole for `reader`, at most `maximum` bytes: what its last capture
/// handed out with the changes since appended where it is kept opened, rather than the document
/// saved again; saved from the copy itself where it is held in the clear.
fn whole(
    peer: &StatePeer,
    kept: &mut KeptDocument,
    reader: [u8; 32],
    route: (&str, &str),
    maximum: usize,
) -> Result<Vec<u8>, String> {
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
    saved_whole(document, maximum).map_err(|error| error.to_string())
}

/// The chain `resource`, a chain's head, belongs to; `None` for any other resource.
#[cfg(feature = "apps")]
fn part_of(resource: &str) -> Option<&str> {
    match neighbour_wire::document_sync(resource) {
        neighbour_wire::DocumentSync::ChainHead { base } => Some(base),
        _ => None,
    }
}

/// The device a resource was opened as, and the epoch of the keys that opened it, `None` for one
/// held in the clear: what was opened under other keys (the workspace rotated them, as removing a
/// member does) is let go, and the resource opens whole under the keys held now.
type OpenedWith = ([u8; 32], Option<u64>);

/// One resource as this service's own device last opened it; `None` until a read opened it.
type OwnKept = std::sync::Arc<std::sync::Mutex<Option<(OpenedWith, KeptDocument)>>>;

/// Most bytes [`OwnOpened`] keeps opened ([`KeptDocument::size`]), the least recently read let
/// go first. It is never bounded by how many resources there are: a tracker of thousands of
/// issues is kept in eighty parts or more, read part after part by each reader in turn (the
/// tracker's own copy, a caller's, the search index), and a bound below the number of parts let
/// each reader's pass push out the parts the next was about to read, so every reader opened
/// every part's whole sealed history again.
const OWN_OPENED_BYTES: usize = 256 * 1024 * 1024;

/// Sealed resources as this service's own device last opened them, by workspace, app and
/// resource, so the next read opens only what was sealed since: a resource its session follows
/// is opened once per write, not whole per read. What it holds is plaintext this device's keys
/// opened, kept in its memory alone.
#[derive(Default)]
pub(super) struct OwnOpened(std::sync::Mutex<OwnEntries>);

/// What [`OwnOpened`] keeps, and how many reads it counted.
#[derive(Default)]
struct OwnEntries {
    entries: std::collections::HashMap<OpenedResource, OwnEntry>,
    reads: u64,
}

/// A resource of a workspace, by its app: `(workspace, app, resource)`.
type OpenedResource = (String, String, String);

/// One resource [`OwnOpened`] keeps: what is opened of it, the read that last asked for it,
/// and the bytes it kept after its last read.
struct OwnEntry {
    kept: OwnKept,
    read: u64,
    size: usize,
}

impl OwnOpened {
    /// What is kept of `app`'s `resource` of `workspace`, held while it is read.
    fn of(&self, workspace: &str, app: &str, resource: &str) -> OwnKept {
        let key = (workspace.to_owned(), app.to_owned(), resource.to_owned());
        let mut kept = self.0.lock_recover();
        kept.reads = kept.reads.wrapping_add(1);
        let read = kept.reads;
        let entry = kept.entries.entry(key).or_insert_with(|| OwnEntry {
            kept: OwnKept::default(),
            read,
            size: 0,
        });
        entry.read = read;
        let opened = std::sync::Arc::clone(&entry.kept);
        drop(kept);
        opened
    }

    /// `app`'s `resource` of `workspace` keeps `size` bytes now: what was read least recently
    /// is let go while everything kept comes to more than [`OWN_OPENED_BYTES`].
    fn kept(&self, (workspace, app, resource): (&str, &str, &str), size: usize) {
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

/// How `app`'s `resource` of a workspace is opened on this device.
enum Sealing {
    /// It is held in the clear.
    Clear,
    /// It is sealed, and opens with these keys.
    Sealed(Box<super::sealed_documents::DocumentKeys>),
    /// It is sealed for a group this device takes part in, under keys it does not hold yet: the
    /// welcome or commit that brings them is on its way, and it reads once that arrived.
    Due,
}

/// How `app`'s `resource` of `workspace` opens on this device: in the clear, with the keys of the
/// group it is sealed for, or once the keys this device is due arrive. Nothing waits here: a read
/// answered [`Sealing::Due`] says the resource is on its way, and its reader reads it again when
/// the device's keys change, as an app's held read and the session's next pass do.
async fn content_keys(
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
        let keys = super::sealed_documents::read_document_keys(
            device,
            workspace,
            (app, resource),
            scoped,
            false,
        )
        .await;
        return match keys {
            Ok(keys)
                if peer
                    .publication()
                    .sealed_keys_available(app, resource, |metadata| keys.covers(metadata)) =>
            {
                Ok(Sealing::Sealed(Box::new(keys)))
            }
            Ok(_) => Ok(Sealing::Due),
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
            Ok(Sealing::Due)
        }
        Some(Ok(keys)) => Ok(Sealing::Sealed(Box::new(
            super::sealed_documents::DocumentKeys::app_document(keys, workspace, (app, resource)),
        ))),
    }
}

/// `app`'s `resource` of `workspace` for the device `reader` at `now`, as
/// [`Workspaces::capture_resource`] answers it.
#[cfg(feature = "apps")]
async fn capture_resource(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
    reader: [u8; 32],
    now: SystemTime,
    maximum: usize,
) -> Result<Option<PeerDocumentCapture>, PeerDocumentReadError> {
    let keys = match content_keys(peer, device, workspace, (app, resource)).await? {
        Sealing::Due => return Ok(None),
        Sealing::Clear => None,
        Sealing::Sealed(keys) => Some(keys),
    };
    let mut opened = keys
        .as_deref()
        .map(super::sealed_documents::DocumentKeys::open);
    match peer.capture_document(
        reader,
        (app, resource),
        now,
        maximum,
        opened
            .as_mut()
            .map(|open| open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>),
    ) {
        Ok(capture) => Ok(Some(capture)),
        // A write this copy knows of has not reached it yet: it is on its way, and its landing
        // wakes the reader.
        Err(PeerDocumentReadError::Incomplete) => Ok(None),
        Err(refused) => Err(refused),
    }
}

/// [`Workspaces::resource_revision`] as this service answers it.
#[cfg(feature = "apps")]
async fn resource_revision(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
    reader: [u8; 32],
    now: SystemTime,
) -> Result<Option<[u8; 32]>, String> {
    peer.check_document_read(reader, app, resource, now)?;
    if peer.checkpoint_pending(app, resource) {
        return Ok(None);
    }
    let keys = match content_keys(peer, device, workspace, (app, resource))
        .await
        .map_err(|error| error.to_string())?
    {
        Sealing::Due => return Ok(None),
        Sealing::Clear => None,
        Sealing::Sealed(keys) => Some(keys),
    };
    let mut opened = keys
        .as_deref()
        .map(super::sealed_documents::DocumentKeys::open);
    let mut kept = tilo_peer_state::KeptDocument::default();
    let document = peer.open_document(
        reader,
        (app, resource),
        now,
        usize::MAX,
        opened
            .as_mut()
            .map(|open| open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>),
        &mut kept,
    );
    let document = match document {
        Ok(document) => document,
        // Missing a write it knows of: undecided until that lands, as the capture answers.
        Err(PeerDocumentReadError::Incomplete) => return Ok(None),
        Err(refused) => return Err(refused.to_string()),
    };
    Ok(Some(tilo_peer_state::committed_document_revision(
        &document.get_heads(),
    )))
}

#[cfg(test)]
mod own_opened_tests {
    use super::{OWN_OPENED_BYTES, OwnOpened};
    use std::sync::Arc;

    /// Each reader of a tracker's parts after a restart (its own copy, a caller's, the search
    /// index) reads every part in turn: what the first opened stays opened for the next however
    /// many parts there are. A bound by count below a tracker's parts let each pass push out the
    /// parts the next was about to read.
    #[test]
    fn a_trackers_parts_stay_opened_from_one_reader_to_the_next() {
        let opened = OwnOpened::default();
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
        let opened = OwnOpened::default();
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
