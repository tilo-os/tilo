//! The governor's own reads and writes of documents its members write sealed.
//!
//! In a workspace whose channels this server takes part in, members seal every
//! write of a document, and the governor's copy keeps those writes as the
//! document's ciphertext history
//! ([`tilo_peer_state::PeerPublication::is_sealed_document`]). As a member of
//! the channels the server holds their content keys: it reads such a document by
//! opening that history over the document's cleartext base, writes to it
//! sealed like any member does, and compacts the history before it reaches
//! its bound, so its copy goes on following the document however long the
//! workspace keeps writing. Every other device taking part in a channel does the
//! same with its own copy and publishes the checkpoint
//! ([`tilo_peer_state::PeerPublication::draft_sealed_compaction`]): the
//! governor's copy follows that checkpoint like any other member write, and
//! no device's history waits for someone else to fold it.

use collab_types::{ContentRetention, SignedOp};
use ed25519_dalek::SigningKey;
use key_group::ContentKeys;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::StatePeer;
use tilo_user_identity::DeviceCert;

use tilo_peer_state::UnmergedReason;

use super::{CollaborationError, GovernedWorkspace, SystemTime, Workspaces};

/// Member writes followed before a failed compaction is tried again. A
/// record whose history does not merge yet does not compact; trying on every
/// write opened and merged its whole history each time.
#[cfg_attr(
    not(feature = "device-keys"),
    allow(
        dead_code,
        reason = "sealed histories are followed only where they are read"
    )
)]
const COMPACTION_RETRY: usize = 256;

use crate::documents::device_keys::{DeviceKeys, GroupKeysAnswer};
pub(crate) use crate::documents::sealed::{
    DocumentKeys, read_document_keys, read_document_keys_for,
};

impl DocumentKeys {
    /// Seal content of the record under the channel's current epoch.
    pub(super) fn seal(
        &self,
        retention: ContentRetention,
    ) -> impl FnMut(&[u8]) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> + '_ {
        move |plaintext| {
            self.keys
                .seal_content(self.artifact, retention, plaintext)
                .map_err(|error| error.to_string())
        }
    }

    /// Compact `record`'s sealed history in `candidate` as the host's `key`, into `rebuilt` when
    /// the operator rebuilt the record, else into its merged history; `device` is the host's own,
    /// which checks the checkpoint it drafts.
    fn compact(
        &self,
        candidate: &mut StatePeer,
        (app, record): (&str, &str),
        rebuilt: Option<Vec<u8>>,
        (device, key): (&DeviceCert, &SigningKey),
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut open = self.open();
        // A snapshot larger than one op goes in segments: the first a
        // checkpoint, the rest following it.
        let mut seal = |frame: &[u8]| {
            self.keys
                .seal_content(self.artifact, ContentRetention::of_frame(frame), frame)
                .map_err(|error| error.to_string())
        };
        let author = key.verifying_key().to_bytes();
        let operator_rebuilt = rebuilt.is_some();
        let drafted = candidate.publication().draft_sealed_compaction(
            (app, record),
            self.readers.as_deref(),
            rebuilt,
            &mut open,
            &mut seal,
            author,
            now,
        );
        let mut ops = match drafted {
            Ok(ops) => ops,
            Err(strict) if operator_rebuilt => return Err(strict),
            Err(strict) => match self.without_unsent(candidate, (app, record))? {
                Some(read) => candidate.publication().draft_sealed_compaction(
                    (app, record),
                    self.readers.as_deref(),
                    Some(read),
                    &mut open,
                    &mut seal,
                    author,
                    now,
                )?,
                None => return Err(strict),
            },
        };
        for op in &mut ops {
            collab_types::sign_op(op, key).map_err(|error| error.to_string())?;
        }
        let admitted = candidate.publication_mut().admit_sealed_compaction(
            app,
            record,
            self.readers.as_deref(),
            ops,
            now,
        )?;
        // The host drafts a checkpoint as a device with the record's keys: it checks it as every
        // such device does, and keeps that it did, so the writes it stands in for are dropped
        // once every one of them has. A check the workspace does not take leaves the checkpoint
        // standing: this host's silence then keeps the writes back, and fails nothing.
        if let Some((checkpoint, true)) = candidate
            .publication()
            .check_leading_checkpoint((app, record), &mut self.open())
            && let Err(error) = candidate.workspace_mut().record_checkpoint_check(
                device,
                (app, record),
                &checkpoint,
                key,
                now,
            )
        {
            tracing::warn!(
                %app,
                %record,
                %error,
                "this host's check of the checkpoint it drafted was not kept"
            );
        }
        Ok(admitted)
    }
}

impl DocumentKeys {
    /// `record` of `app` as it reads without the writes of its history stuck behind a change
    /// their own writer never sent, when those are all its history leaves out besides a writer's
    /// second change of one sequence: what the governor, the record's store, compacts it into, so
    /// its history folds again rather than holding them until it fills. `None` when its history
    /// holds no such write, and merges or waits as any other.
    fn without_unsent(
        &self,
        candidate: &StatePeer,
        (app, record): (&str, &str),
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(opened) =
            candidate
                .publication()
                .opened_document(app, record, &mut self.open())?
        else {
            return Ok(None);
        };
        let unsent = opened
            .unmerged
            .iter()
            .filter(|write| matches!(write.reason, UnmergedReason::BehindUnsent { .. }))
            .count();
        // A write the workspace's rules refuse, and one built on it, is read without by every
        // reader, and folds away with the rest.
        let forks = opened
            .unmerged
            .iter()
            .filter(|write| {
                matches!(
                    write.reason,
                    UnmergedReason::AlreadyHeld { .. }
                        | UnmergedReason::Refused { .. }
                        | UnmergedReason::BuiltOnRefused { .. }
                )
            })
            .count();
        if unsent == 0 || unsent.saturating_add(forks) != opened.unmerged.len() {
            return Ok(None);
        }
        for write in &opened.unmerged {
            if let UnmergedReason::BehindUnsent { missing } = &write.reason {
                tracing::warn!(
                    %app,
                    %record,
                    writer = %hex::encode(write.author),
                    seq = write.seq,
                    ?missing,
                    "the governor compacts a sealed record without a write built on a change its writer never sent"
                );
            }
        }
        Ok(Some(opened.document))
    }
}

impl GovernedWorkspace {
    /// The workspace's content keys as the device this host runs on holds
    /// them, without opening a group it has none of: `None` when this host
    /// runs on a device that takes part in no group of the workspace.
    pub(super) async fn route_keys(&self) -> Option<Result<ContentKeys, String>> {
        self.reader_keys(collab_types::EVERY_MEMBER).await
    }

    /// The keys of the group of `readers` in this workspace as this device
    /// holds them, without opening a group it has none of: `None` when it takes part in none.
    pub(super) async fn reader_keys(&self, readers: &str) -> Option<Result<ContentKeys, String>> {
        let device = self.device.as_ref()?;
        device.group_keys(self.workspace(), readers, false).await
    }

    /// Whether this host holds the workspace's keys: it can then read sealed
    /// writes.
    #[cfg_attr(
        not(feature = "device-keys"),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    pub(super) async fn holds_keys(&self) -> bool {
        matches!(self.route_keys().await, Some(Ok(_)))
    }

    /// The keys of `document` of `app`'s channel when the document is sealed:
    /// `None` for a document written in the clear.
    ///
    /// # Errors
    ///
    /// The document is sealed and this server's device does not take part in its
    /// channel, or the channel's keys cannot be read.
    pub(super) async fn document_keys(
        &self,
        app: &str,
        document: &str,
    ) -> Result<Option<DocumentKeys>, String> {
        if !self.peer().publication().is_sealed_document(app, document) {
            return Ok(None);
        }
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| String::from("this device holds no group keys"))?;
        let scoped = self
            .peer()
            .requires_document_keys(app, document, tilo_clock::wall())?;
        let named = self.resources.known_readers(document);
        read_document_keys_for(
            device,
            self.workspace(),
            (app, document),
            scoped,
            scoped,
            named,
        )
        .await
        .map(Some)
    }
}

/// `resource` of `app` as `peer` holds it, its sealed history opened with `keys` where it has
/// them; a write of the history that does not merge is left out and named in the log.
pub(super) fn opened_readable(
    peer: &StatePeer,
    keys: Option<&DocumentKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
) -> Result<Option<Vec<u8>>, String> {
    let Some(keys) = keys else {
        return Ok(peer.document(app, resource));
    };
    let Some(opened) = peer
        .publication()
        .opened_document(app, resource, &mut keys.open())?
    else {
        return Ok(None);
    };
    Ok(Some(merged_or_named(opened, workspace, (app, resource))))
}

/// What `opened` reads as, its writes that do not merge named in the log.
fn merged_or_named(
    opened: tilo_peer_state::OpenedDocument,
    workspace: &str,
    (app, resource): (&str, &str),
) -> Vec<u8> {
    if !opened.unmerged.is_empty() {
        // A write left out for any reason is a loss: nothing reads it, and
        // it is kept for an operator rather than dropped.
        let writes: Vec<String> = opened.unmerged.iter().map(ToString::to_string).collect();
        tracing::warn!(
            %workspace,
            %app,
            %resource,
            left_out = writes.len(),
            writes = ?writes,
            "a sealed resource is read without writes of its history that do not merge; they are kept"
        );
    }
    opened.document
}

/// `record` of `app` of `workspace` with every write of its history, saved,
/// read from `peer`, a copy of the governor's state, and opened with
/// `device`'s keys where it is sealed: what a reader that must see all of a
/// record reads without holding the workspace while it does.
///
/// # Errors
///
/// Why it does not read whole: a sealed write of it does not open or merge,
/// or its channel's keys are not here.
pub(super) async fn whole_document(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, document): (&str, &str),
) -> Result<Vec<u8>, String> {
    if !peer.publication().is_sealed_document(app, document) {
        return peer
            .document(app, document)
            .ok_or_else(|| "the host holds no copy of it".to_owned());
    }
    let device = device.ok_or("it is sealed and this host has no channel keys")?;
    let scoped = peer.publication().has_document_keys(app, document);
    let keys = read_document_keys(device, workspace, (app, document), scoped, scoped).await?;
    let opened = peer
        .publication()
        .opened_document(app, document, &mut keys.open())
        .map_err(|error| format!("its sealed history does not open: {error}"))?
        .ok_or("its sealed history holds no write")?;
    // A write stuck behind a change its writer never sent, one the workspace's rules refuse, and
    // one built on that, are no part of the document anyone will ever read: the document is whole
    // without them.
    let unmerged: Vec<String> = opened
        .unmerged
        .iter()
        .filter(|write| {
            !matches!(
                write.reason,
                UnmergedReason::BehindUnsent { .. }
                    | UnmergedReason::Refused { .. }
                    | UnmergedReason::BuiltOnRefused { .. }
            )
        })
        .map(ToString::to_string)
        .collect();
    if unmerged.is_empty() {
        Ok(opened.document)
    } else {
        Err(format!(
            "{} sealed writes of its history do not merge: {}",
            unmerged.len(),
            unmerged.join("; ")
        ))
    }
}

/// Why a host writes no record its device holds no keys of: it writes a record
/// only sealed, as every device does, and never in the clear.
pub(super) const SEALED_ONLY: &str =
    "this host writes a record only sealed, and holds no keys to seal this one with";

/// `keys`, the keys this host writes a record with, when it holds any.
///
/// # Errors
///
/// It holds none: [`SEALED_ONLY`].
pub(super) fn sealed_with(keys: Option<&DocumentKeys>) -> Result<&DocumentKeys, String> {
    keys.ok_or_else(|| SEALED_ONLY.to_owned())
}

/// Write `changes` to `record` of `app` into `candidate` as `key`'s own
/// write, sealed with `keys`: answers the frame this host's device carries,
/// as any device's write reaches the workspace's readers.
///
/// The first sealed write of a record is the whole record, what it held with
/// `changes`: a reader holding only the record's ciphertext history — a
/// page that joins later — reads the record from it, where a change alone
/// would wait forever for a base that never comes.
pub(super) fn seal_changes(
    candidate: &mut StatePeer,
    keys: &DocumentKeys,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    if candidate.publication().is_sealed_document(app, record) {
        return candidate.publication_mut().publish_sealed_incremental(
            app,
            record,
            changes,
            &mut keys.seal(ContentRetention::Delta),
            key,
            now,
        );
    }
    let mut document = match candidate.document(app, record) {
        Some(base) => automerge::AutoCommit::load(&base).map_err(|error| error.to_string())?,
        None => automerge::AutoCommit::new(),
    };
    tilo_automerge::ApplyIncoming::load_incoming(&mut document, &changes)
        .map_err(|error| error.to_string())?;
    refound(candidate, keys, (app, record), document.save(), key, now)
}

/// Write `record` of `app` as this host's own sealed checkpoint of it, from
/// `document`, the record as its key holder reads it, sealed with `keys`. This
/// is what founds a route again after its retained history lost the base its
/// deltas were written over: the checkpoint
/// is the whole record, so a device that joins afterwards reads it and the
/// writes after it.
///
/// `document` is admitted as this state's own write and joined to its sealed
/// history, exactly as [`super::applications`]' authoring path does, so the
/// replay that follows carries it.
///
/// # Errors
///
/// The write could not be sealed, signed or admitted.
pub(super) fn refound(
    candidate: &mut StatePeer,
    keys: &DocumentKeys,
    (app, record): (&str, &str),
    document: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    candidate.publication_mut().publish_sealed_checkpoint(
        app,
        record,
        document,
        &mut keys.seal(collab_types::ContentRetention::Checkpoint),
        key,
        now,
    )
}

/// Start `record` of `app` in `candidate` as the workspace's host, signed with
/// `key` and sealed with `keys`: the checkpoint every record is born from,
/// which holds nothing, then `changes`, what the record starts with. A record
/// that needs keys of its own is written in the clear not even empty: its
/// first sealed write is the whole record and founds it alone. Answers the
/// frames it wrote, in order.
pub(super) fn start_sealed(
    candidate: &mut StatePeer,
    keys: &DocumentKeys,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<Vec<NeighbourMsg>, String> {
    let mut written = Vec::with_capacity(2);
    if !candidate.holds_document(app, record)
        && !candidate.requires_document_keys(app, record, now)?
    {
        written.push(candidate.publication_mut().publish_document(
            app,
            record,
            automerge::AutoCommit::new().save(),
            key,
            now,
        )?);
    }
    written.push(seal_changes(
        candidate,
        keys,
        (app, record),
        changes,
        key,
        now,
    )?);
    Ok(written)
}

impl Workspaces {
    /// The content keys of `route`'s workspace, for the workspace service's endpoints
    /// (workspace actions for members' pages), as this device holds them.
    /// `create` opens the key group for the documents the
    /// writes are about to start, and otherwise only an existing key group is
    /// read. A document that needs keys of its own has one opened for it when
    /// it has none yet, as a read of that document opens it: a rule can make a
    /// document need its own keys after it was written, and nothing has sealed
    /// there since. `None` when this device takes part in no key group of the
    /// workspace.
    pub async fn own_route_keys(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        create: bool,
    ) -> GroupKeysAnswer {
        self.own_route_keys_read_by(route, None, create).await
    }

    pub(super) async fn own_route_keys_read_by(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        readers: Option<&str>,
        create: bool,
    ) -> GroupKeysAnswer {
        let device = self.device_keys()?;
        let scope = match route.key_scope_for(readers) {
            Ok(scope) => scope,
            Err(reason) => return Some(Err(reason.to_string())),
        };
        let readers = readers.unwrap_or(collab_types::EVERY_MEMBER);
        // Read off the workspace's replica as it stands, not a copy of it taken under its lock.
        let isolated = match self
            .reading_of(&route.cluster_id)
            .await?
            .replica
            .read(|peer| {
                peer.requires_document_keys(&route.app_id, &route.document, tilo_clock::wall())
            }) {
            Ok(isolated) => isolated,
            Err(error) => return Some(Err(error)),
        };
        if !isolated {
            return device.group_keys(&route.cluster_id, readers, create).await;
        }
        let scope = scope.to_string();
        let keys = match device.group_keys(&scope, readers, create).await? {
            Ok(keys) => keys,
            // A document that came to need keys of its own after it was written
            // has no group yet, and a document's group is founded by whichever
            // of its devices first needs its keys. This host reads the
            // document, so it opens the group as its own reads of it do
            // (`GovernedWorkspace::document_keys`): the policy names the devices
            // the group admits, and this device is among them.
            Err(_) if !create => match device.group_keys(&scope, readers, true).await? {
                Ok(keys) => keys,
                Err(reason) => return Some(Err(reason)),
            },
            Err(reason) => return Some(Err(reason)),
        };
        let keys = match device.group_keys(&route.cluster_id, readers, false).await {
            Some(Ok(history)) => keys.with_workspace_history(history),
            _ => keys,
        };
        Some(Ok(keys))
    }

    /// Take part in `workspace`'s group: this host's device asks its key holders
    /// for access and holds its keys once one admits it. `false` when it takes
    /// part in no group of the workspace. Asked before the workspace is locked,
    /// since the ask waits on the key service for the group to admit this device.
    #[cfg_attr(
        not(feature = "device-keys"),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    pub async fn join_keys(&self, workspace: &str) -> bool {
        let Some(device) = self.device_keys() else {
            return false;
        };
        matches!(
            device
                .group_keys(workspace, collab_types::EVERY_MEMBER, true)
                .await,
            Some(Ok(_))
        )
    }
}

/// How long the service waits for the device's key service to open a group: far more than the
/// wait before the device founds the group itself (`tilo_workspace_keys::founding_wait`) and the work a first write does beside it, so a
/// fresh workspace's first write is not refused for keys that are about to arrive.
pub const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(feature = "device-keys")]
const _: () = assert!(OPEN_WAIT.as_secs() > tilo_workspace_keys::founding_wait().as_secs());

/// Ask the key holders of `workspace`'s own group to admit this host's device, and wait until it
/// holds the group's keys: `true` once it does, `false` when the device takes part in no group of
/// the workspace or nothing will ever say its keys changed. Reading a sealed record never asks
/// (it waits for keys the device is due, [`crate::documents::Sealing::Due`]), so a device that
/// only reads what was written before it arrived must ask, however long it waits.
#[cfg_attr(
    not(feature = "device-keys"),
    allow(
        dead_code,
        reason = "sealed histories are followed only where they are read"
    )
)]
pub(super) async fn wait_for_group_keys(device: &DeviceKeys, workspace: &str) -> bool {
    // The first ask starts the device's part in the group; the key service keeps asking on its
    // own after the wait, so a wait that ends first leaves nothing undone.
    if let Some(Err(error)) = device
        .group_keys(workspace, collab_types::EVERY_MEMBER, true)
        .await
    {
        tracing::debug!(%workspace, %error, "a workspace's group has not admitted this device yet");
    }
    // Where the keys stood when last looked at: a change after that look ends the wait at once,
    // so keys that arrive between a look and the wait are not missed.
    let mut seen = 0;
    loop {
        match device
            .group_keys(workspace, collab_types::EVERY_MEMBER, false)
            .await
        {
            Some(Ok(_)) => return true,
            None => return false,
            Some(Err(_)) => {}
        }
        let Some(next) = device.keys_changed(workspace, seen).await else {
            return false;
        };
        seen = next;
    }
}

impl Workspaces {
    /// Everything the governor's copy holds of `record` of `app` in
    /// `workspace`: its cleartext base and, for a sealed record, every write
    /// of its history opened with the channel's keys this server holds, none
    /// merged. What an operator repairing a record that does not read whole
    /// reads it from; `None` for a record not held here.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or the record is sealed and its
    /// channel's keys are not here.
    pub async fn document_history(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<tilo_peer_state::OpenedHistory>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        let keys = governed
            .document_keys(app, record)
            .await
            .map_err(CollaborationError::new)?;
        let mut sealed_shut = |_: &SignedOp| Err("this host holds no keys of it".to_owned());
        let history = match &keys {
            Some(keys) => {
                governed
                    .peer()
                    .publication()
                    .opened_history(app, record, &mut keys.open())
            }
            None => governed
                .peer()
                .publication()
                .opened_history(app, record, &mut sealed_shut),
        };
        drop(governed);
        Ok(history)
    }

    /// Replace the sealed history of `record` of `app` in `workspace` with
    /// `rebuilt`, the record as an operator rebuilt it from that history
    /// ([`Self::document_history`]) where it does not merge whole: one sealed
    /// checkpoint, compacted and signed by this host's device and carried like
    /// any compaction, which every member takes in place of exactly the writes
    /// it covers.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the record is held in the clear or
    /// its channel's keys are not here, or the compaction could not be made,
    /// committed or published.
    pub async fn compact_document_into(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        rebuilt: Vec<u8>,
    ) -> Result<(), CollaborationError> {
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        let keys = governed
            .document_keys(app, record)
            .await
            .map_err(CollaborationError::new)?
            .ok_or_else(|| CollaborationError::new("the record is held in the clear"))?;
        let now = tilo_clock::wall();
        let mut candidate = governed.peer().clone();
        let written = keys
            .compact(
                &mut candidate,
                (app, record),
                Some(rebuilt),
                (&self.inner.host.cert, &self.inner.host.key),
                now,
            )
            .map_err(CollaborationError::new)?;
        let collaboration = governed.collaboration().clone();
        governed.keep(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        self.carry_written(governed.workspace_id(), &written)?;
        drop(governed);
        Ok(())
    }

    /// The keys the governor writes `record` of `app` with: in a workspace whose
    /// channels this server takes part in every new record is sealed from its
    /// first write, so its channel is opened first when it has none (its
    /// genesis, published before anything is written there). A record it
    /// already holds in the clear stays so. `None` in a workspace it does
    /// not take part in, where records are written in the clear.
    ///
    /// # Errors
    ///
    /// As [`GovernedWorkspace::document_keys`], or the channel cannot be opened.
    pub(super) async fn sealing_keys(
        &self,
        workspace: &GovernedWorkspace,
        app: &str,
        document: &str,
    ) -> Result<Option<DocumentKeys>, String> {
        let scoped = workspace
            .peer()
            .requires_document_keys(app, document, tilo_clock::wall())?;
        if scoped {
            let device = workspace
                .device
                .as_ref()
                .ok_or("no keys are available for this document")?;
            return read_document_keys_for(
                device,
                workspace.workspace(),
                (app, document),
                true,
                true,
                workspace.resources.known_readers(document),
            )
            .await
            .map(Some);
        }
        if !workspace.seals() || neighbour_wire::KeyScope::has_named_readers(document) {
            return workspace.document_keys(app, document).await;
        }
        let held_in_the_clear = workspace.peer().holds_document(app, document)
            && !workspace
                .peer()
                .publication()
                .is_sealed_document(app, document);
        if held_in_the_clear {
            return workspace.document_keys(app, document).await;
        }
        let name = workspace.workspace();
        // The device's channel service opens the document's channel when it has
        // none yet, as the workspace's writes ask it to.
        let Some(device) = &workspace.device else {
            return workspace.document_keys(app, document).await;
        };
        let keys = device
            .group_keys(name, collab_types::EVERY_MEMBER, true)
            .await
            .ok_or("the document is sealed and this host takes part in no channel")??;
        Ok(Some(DocumentKeys::app_document(
            keys,
            name,
            (app, document),
        )))
    }

    /// Keep that this host, a device with `record`'s keys, checked the checkpoint leading it
    /// against the writes it stands in for: the workspace's stores drop those writes once every
    /// device with the record's keys, this one among them, has. A record whose keys this host
    /// holds none of is one it is no device with the keys of, and it checks nothing; a checkpoint
    /// it checked already is checked once.
    ///
    /// # Errors
    /// The workspace cannot be committed, or the check cannot be kept.
    #[cfg(feature = "device-keys")]
    pub(super) async fn check_leading_checkpoint(
        &self,
        workspace: &mut GovernedWorkspace,
        (app, record): (&str, &str),
    ) -> Result<(), String> {
        let Some(checkpoint) = workspace
            .peer()
            .publication()
            .leading_checkpoint((app, record))
        else {
            return Ok(());
        };
        let route = (app.to_owned(), record.to_owned());
        if workspace.checked_checkpoints.get(&route) == Some(&checkpoint.signature.0) {
            return Ok(());
        }
        let Ok(Some(keys)) = workspace.document_keys(app, record).await else {
            return Ok(());
        };
        let Some((checkpoint, holds)) = workspace
            .peer()
            .publication()
            .check_leading_checkpoint((app, record), &mut keys.open())
        else {
            return Ok(());
        };
        if !holds {
            tracing::warn!(
                workspace = %workspace.workspace(),
                %app,
                %record,
                "a checkpoint does not hold what the writes it stands in for merge to: its \
                 readers read the writes instead"
            );
            return Ok(());
        }
        self.keep_checkpoint_check(workspace, (app, record), &checkpoint)
    }

    /// Keep that this host checked `checkpoint`, the checkpoint leading `record`, as the
    /// workspace's record of the checks every device with the record's keys makes. One this host
    /// keeps already is kept once.
    ///
    /// # Errors
    /// The workspace cannot be committed, or the check cannot be kept.
    #[cfg(feature = "device-keys")]
    pub(super) fn keep_checkpoint_check(
        &self,
        workspace: &mut GovernedWorkspace,
        (app, record): (&str, &str),
        checkpoint: &collab_types::SignedOp,
    ) -> Result<(), String> {
        let route = (app.to_owned(), record.to_owned());
        if workspace.checked_checkpoints.get(&route) == Some(&checkpoint.signature.0) {
            return Ok(());
        }
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        workspace.change(
            &host.key,
            now,
            |_, candidate| {
                let filed = candidate.workspace_mut().record_checkpoint_check(
                    &host.cert,
                    (app, record),
                    checkpoint,
                    &host.key,
                    now,
                )?;
                Ok(((), filed))
            },
            |entry, _| Ok(entry.collaboration.clone()),
        )?;
        workspace
            .checked_checkpoints
            .insert(route, checkpoint.signature.0);
        Ok(())
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, and commit and publish the
    /// compactions. A record whose history does not merge yet (a write it
    /// depends on has not arrived) waits for a later call.
    #[cfg_attr(
        not(feature = "device-keys"),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    pub(super) async fn compact_sealed_histories(
        &self,
        workspace: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let due = workspace.peer().publication().sealed_to_compact();
        if due.is_empty() || self.holds_as_store(workspace) {
            return Ok(());
        }
        if workspace.compaction_wait > 0 {
            workspace.compaction_wait = workspace.compaction_wait.saturating_sub(1);
            return Ok(());
        }
        let now = tilo_clock::wall();
        let mut candidate = workspace.peer().clone();
        let mut written = Vec::new();
        for (app, record) in &due {
            let compaction = match workspace.document_keys(app, record).await {
                Ok(Some(keys)) => keys.compact(
                    &mut candidate,
                    (app, record),
                    None,
                    (&self.inner.host.cert, &self.inner.host.key),
                    now,
                ),
                Ok(None) => continue,
                Err(error) => Err(error),
            };
            match compaction {
                Ok(frames) => written.extend(frames),
                Err(error) => {
                    workspace.compaction_wait = COMPACTION_RETRY;
                    tracing::warn!(workspace = %workspace.workspace(), %app, %record, %error,
                        "a sealed record history was not compacted");
                }
            }
        }
        if written.is_empty() {
            return Ok(());
        }
        let collaboration = workspace.collaboration().clone();
        workspace.keep(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(workspace).await?;
        // The compaction reaches the members as a member's own does: this host is the device
        // that wrote it.
        self.carry_written(workspace.workspace_id(), &written)
            .map_err(|error| error.to_string())
    }
}
