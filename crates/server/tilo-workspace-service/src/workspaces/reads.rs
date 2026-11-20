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
    KeptDocument, PeerDocumentReadError, StatePeer, StoredOpening, changes_since,
    committed_document_revision, saved_whole,
};
#[cfg(feature = "apps")]
use tilo_user_identity::DeviceCert;

#[cfg(feature = "apps")]
use super::SystemTime;
use super::{DeviceKeys, GovernedWorkspace, Workspaces};
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
            .await
            .ok_or("this service does not hold the workspace")?;
        if !peer.holds_document(app, resource) {
            return Ok(DocumentRead::Absent);
        }
        if peer.checkpoint_pending(app, resource) {
            return Ok(DocumentRead::Pending);
        }
        let reader = self
            .own_reader(workspace)
            .await
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
            .as_ref()
            .map(super::sealed_documents::DocumentKeys::open);
        // What this device opened of the resource before is read on, so a read opens only the
        // writes sealed since rather than the whole history again; a second read of it waits
        // for the first and reads on from what that one opened.
        let used = (
            reader,
            keys.as_ref()
                .map(super::sealed_documents::DocumentKeys::epoch),
        );
        let opening = self.inner.own_opened.of(workspace, app, resource);
        let mut held = opening.lock_recover();
        // Nothing opened here since the service started: what it stored of the resource before
        // is read on, so a start opens the writes sealed since and not the whole history.
        let mut kept = match held.take() {
            Some((opened_with, kept)) if opened_with == used => kept,
            _ => keys
                .as_ref()
                .and_then(|keys| {
                    self.inner
                        .own_opened
                        .stored(&peer, (workspace, app, resource), keys)
                })
                .unwrap_or_default(),
        };
        let diag_t0 = std::time::Instant::now();
        let diag_fresh = !kept.is_open();
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
        let diag_open = diag_t0.elapsed();
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
        if diag_t0.elapsed().as_millis() >= 50 {
            eprintln!("DIAG own_read {app}/{resource} fresh={diag_fresh} open={}ms total={}ms thread={:?}", diag_open.as_millis(), diag_t0.elapsed().as_millis(), std::thread::current().name());
        }
        if kept.is_open() {
            if let Some(keys) = &keys {
                self.inner
                    .own_opened
                    .store((workspace, app, resource), keys, &mut kept);
            }
            *held = Some((used, kept));
        } else {
            eprintln!("DIAG kept not open after read {app}/{resource}: {:?}", read.as_ref().map(|read| match read { DocumentRead::Whole{..} => "whole", DocumentRead::Changes{..} => "changes", DocumentRead::Unchanged(_) => "unchanged", DocumentRead::Absent => "absent", DocumentRead::Pending => "pending" }));
        }
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
        let Some((peer, device)) = self.replica(workspace).await else {
            return false;
        };
        if !peer.holds_document(app, resource) || peer.checkpoint_pending(app, resource) {
            return false;
        }
        let Some(reader) = self.own_reader(workspace).await else {
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
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor's guard lives only while its routes are copied out"
    )]
    pub(crate) async fn own_listing(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let held = self
            .held_workspace(workspace)
            .await
            .ok_or("this service does not hold the workspace")?;
        let listing = {
            let held = held.lock().await;
            let peer = held.peer();
            peer.plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .collect()
        };
        Ok(listing)
    }

    /// The workspaces this service holds, by their ids.
    pub(crate) async fn held_workspaces(&self) -> Vec<String> {
        self.inner.workspaces.lock().await.keys().cloned().collect()
    }

    /// The device this service reads `workspace` as: its own where it governs the workspace, the
    /// device its member copy acts as where it joins one.
    async fn own_reader(&self, workspace: &str) -> Option<[u8; 32]> {
        let held = self.held_workspace(workspace).await?;
        let member = held.lock().await.member.is_some();
        if member {
            return self.member_identity().ok().map(|device| device.device());
        }
        Some(self.governor().device_public_key)
    }

    /// Whether `workspace` seals every write of its resources: its device takes part in the
    /// workspace's key group and holds its keys.
    #[cfg(feature = "apps")]
    pub(crate) async fn seals_resources(&self, workspace: &str) -> bool {
        match self.held_workspace(workspace).await {
            Some(held) => held.lock().await.seals(),
            None => false,
        }
    }

    /// The member's own resource `app`'s `resource` in `workspace` is, when it is one.
    #[cfg(feature = "apps")]
    pub(crate) async fn own_resource(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> Option<OwnResource> {
        let held = self.held_workspace(workspace).await?;
        let held = held.lock().await;
        held.peer().own_resource(app, resource)
    }

    /// Whether this service holds anything of `app`'s `resource` in `workspace`, committed or
    /// sealed alike.
    pub(crate) async fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        match self.held_workspace(workspace).await {
            Some(held) => held.lock().await.peer().holds_document(app, resource),
            None => false,
        }
    }

    /// Whether this service holds `workspace` itself, governed or as a member copy.
    #[cfg(feature = "apps")]
    pub(crate) async fn holds_workspace(&self, workspace: &str) -> bool {
        self.inner.workspaces.lock().await.contains_key(workspace)
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
            .await
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
    pub(crate) async fn resource_arriving(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> bool {
        match self.held_workspace(workspace).await {
            Some(held) => held.lock().await.peer().checkpoint_pending(app, resource),
            None => false,
        }
    }

    /// Whether `certificate`'s device may read `app`'s `resource` of `workspace` now: a device the
    /// workspace certifies, a current member, and the resource's Read permission as its governance
    /// grants it.
    #[cfg(feature = "apps")]
    pub(crate) async fn authorizes_resource_read(
        &self,
        workspace: &str,
        certificate: &DeviceCert,
        app: &str,
        resource: &str,
    ) -> Result<(), String> {
        let held = self
            .held_workspace(workspace)
            .await
            .ok_or("this service does not hold the workspace")?;
        let guard = held.lock().await;
        let peer = guard.peer();
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
        drop(guard);
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
            .await
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
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the replica's guard is held only while its routes are copied out"
    )]
    #[cfg(feature = "apps")]
    pub(crate) async fn chain_head_has_part(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> bool {
        let Some(held) = self.held_workspace(workspace).await else {
            return false;
        };
        let routes: Vec<_> = {
            let held = held.lock().await;
            let peer = held.peer();
            peer.plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .collect()
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
    async fn replica(&self, workspace: &str) -> Option<(StatePeer, Option<DeviceKeys>)> {
        let held = self.held_workspace(workspace).await?;
        let held = held.lock().await;
        Some((held.peer().clone(), held.device.clone()))
    }

    /// The bit of `workspace`'s governor the reads above ask: its replica, or `None` for a
    /// workspace not governed here.
    async fn held_workspace(
        &self,
        workspace: &str,
    ) -> Option<std::sync::Arc<tokio::sync::Mutex<GovernedWorkspace>>> {
        self.workspace(workspace).await.ok()
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

/// The extension of the folder beside a workspace's record that holds what this device opened
/// of its sealed resources ([`OwnOpened`]).
const OPENED: &str = "opened";

/// How many resources [`OwnOpened`] keeps opened at most.
const OWN_OPENED: usize = 64;

/// Sealed resources as this service's own device last opened them, by workspace, app and
/// resource, so the next read opens only what was sealed since: a resource its session follows
/// is opened once per write, not whole per read. What it holds is plaintext this device's keys
/// opened, kept in its memory.
///
/// What was opened is stored too, sealed under the resource's own content keys
/// ([`StoredOpening`]), and a read after the service starts again, or after the resource was let
/// go of here, takes it up and opens only the writes sealed since. Opening a sealed history
/// merges each of its writes in an isolated worker and again here: seconds for each part of a
/// tracker of thousands of issues, and a restarted service opened every part a listing reads
/// so before the listing answered.
#[derive(Default)]
pub(super) struct OwnOpened {
    kept: std::sync::Mutex<std::collections::BTreeMap<(String, String, String), OwnKept>>,
    /// The folder of the workspaces' records, beside which what was opened is stored; `None`
    /// keeps it in memory alone.
    stored: Option<std::path::PathBuf>,
}

/// A resource of a workspace, by its app: `(workspace, app, resource)`.
type OpenedResource<'a> = (&'a str, &'a str, &'a str);

impl OwnOpened {
    /// What was opened, stored beside the workspaces' records in `folder` as well as kept in
    /// memory.
    pub(super) fn stored_in(folder: std::path::PathBuf) -> Self {
        Self {
            kept: std::sync::Mutex::default(),
            stored: Some(folder),
        }
    }

    /// Where what was opened of `resource` is stored.
    fn path(&self, (workspace, app, resource): OpenedResource<'_>) -> Option<std::path::PathBuf> {
        let mut named = blake3::Hasher::new_derive_key("tilo own opened resource v1");
        named.update(app.as_bytes());
        named.update(&[0]);
        named.update(resource.as_bytes());
        // Beside the workspace's record, so it goes when the workspace's files do.
        Some(
            self.stored
                .as_ref()?
                .join(format!("{workspace}.{OPENED}"))
                .join(named.finalize().to_hex().as_str()),
        )
    }

    /// What was stored of `resource`, kept again when `peer`'s history of it still continues
    /// it ([`StatePeer::kept_again`]); `None` when nothing was, or it no longer opens.
    fn stored(
        &self,
        peer: &StatePeer,
        resource: OpenedResource<'_>,
        keys: &super::sealed_documents::DocumentKeys,
    ) -> Option<KeptDocument> {
        let path = self.path(resource)?;
        let sealed = std::fs::read(&path).ok()?;
        let opened = keys
            .open_stored(&sealed)
            .and_then(|plaintext| {
                postcard::from_bytes::<StoredOpening>(&plaintext).map_err(|error| error.to_string())
            });
        match opened {
            Ok(stored) => peer.kept_again((resource.1, resource.2), stored),
            Err(error) => {
                tracing::debug!(%error, path = %path.display(), "a resource's stored opening does not open");
                None
            }
        }
    }

    /// Store what `kept` opened of `resource` that was not stored yet, sealed under `keys`.
    fn store(
        &self,
        resource: OpenedResource<'_>,
        keys: &super::sealed_documents::DocumentKeys,
        kept: &mut KeptDocument,
    ) {
        let Some(path) = self.path(resource) else {
            return;
        };
        let Some(unstored) = kept.unstored() else {
            return;
        };
        let written = postcard::to_allocvec(&unstored)
            .map_err(|error| error.to_string())
            .and_then(|plaintext| keys.seal_stored(&plaintext))
            .and_then(|sealed| write_whole(&path, &sealed).map_err(|error| error.to_string()));
        if let Err(error) = written {
            tracing::debug!(%error, path = %path.display(), "a resource's opening was not stored");
        }
    }

    /// What is kept of `app`'s `resource` of `workspace`, held while it is read.
    fn of(&self, workspace: &str, app: &str, resource: &str) -> OwnKept {
        let key = (workspace.to_owned(), app.to_owned(), resource.to_owned());
        let mut kept = self.kept.lock_recover();
        if !kept.contains_key(&key) && kept.len() >= OWN_OPENED {
            eprintln!("DIAG own opened evicts");
            kept.pop_first();
        }
        std::sync::Arc::clone(kept.entry(key).or_default())
    }
}

/// How `app`'s `resource` of a workspace is opened on this device.
enum Sealing {
    /// It is held in the clear.
    Clear,
    /// It is sealed, and opens with these keys.
    Sealed(super::sealed_documents::DocumentKeys),
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
        Some(Ok(keys)) => Ok(Sealing::Sealed(super::sealed_documents::DocumentKeys::of(
            keys,
            workspace,
            collab_types::EVERY_MEMBER,
            &collab_types::app_document_id(app, resource),
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
        .as_ref()
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
        .as_ref()
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

/// Write `bytes` to `path` whole: to a file beside it first, then moved over it, so a reader
/// never finds half of them.
fn write_whole(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| std::io::Error::other("a stored opening needs a folder"))?;
    std::fs::create_dir_all(folder)?;
    let mut writing = path.as_os_str().to_owned();
    writing.push(".writing");
    std::fs::write(&writing, bytes)?;
    std::fs::rename(&writing, path)
}
