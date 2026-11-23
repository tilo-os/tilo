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
use tilo_lock::MutexRecover as _;
use tilo_peer_state::StatePeer;

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
    pub(super) fn same_write_key(&self, keys: &ContentKeys) -> bool {
        self.keys.same_write_key(keys)
    }

    /// Seal content of the record under the channel's current epoch.
    fn seal(
        &self,
        retention: ContentRetention,
    ) -> impl FnMut(&[u8]) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> + '_ {
        move |plaintext| {
            self.keys
                .seal_content(self.artifact, retention, plaintext)
                .map_err(|error| error.to_string())
        }
    }

    /// Compact `record`'s sealed history in `candidate` as the host's `key`,
    /// into `rebuilt` when the operator rebuilt the record, else into its
    /// merged history.
    fn compact(
        &self,
        candidate: &mut StatePeer,
        (app, record): (&str, &str),
        rebuilt: Option<Vec<u8>>,
        key: &SigningKey,
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
        candidate.publication_mut().admit_sealed_compaction(
            app,
            record,
            self.readers.as_deref(),
            ops,
            now,
        )
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
        let forks = opened
            .unmerged
            .iter()
            .filter(|write| matches!(write.reason, UnmergedReason::AlreadyHeld { .. }))
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
        let waiting = |fallback: String| self.waiting_for_update(app, document).unwrap_or(fallback);
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| waiting("this device holds no group keys".into()))?;
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
        .map_err(waiting)
    }

    /// What a reader of `document` of `app` is told on this device while it holds no keys to
    /// read it and the document waits for a key holder to seal what was written before reader
    /// sets again ([`tilo_peer_state::WaitingForUpdate`]).
    fn waiting_for_update(&self, app: &str, document: &str) -> Option<String> {
        self.peer()
            .publication()
            .waiting_for_update(app, document)
            .map(|waiting| waiting.to_string())
    }

    /// `record` of `app` as the governor reads it, saved: the record itself,
    /// or for a sealed one its history opened with the channel's keys.
    /// A sealed write that does not open, or waits for a change the record
    /// lacks, is left out and named in the log: the workspace reads the rest,
    /// and the write stays in the history for when what it waits for comes.
    ///
    /// # Errors
    ///
    /// As [`Self::document_keys`], or the history's changes are malformed.
    pub(super) async fn readable_document(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let keys = self.document_keys(app, record).await?;
        opened_readable(&self.peer(), keys.as_ref(), self.workspace(), (app, record))
    }
}

/// [`GovernedWorkspace::readable_document`] as the workspace `reading` reads
/// holds it now, without the workspace's lock: the device's key service can
/// take long to answer (it settles a newcomer's access first), and every other
/// read and every member's write waited for it while a read held the workspace.
///
/// # Errors
///
/// As [`GovernedWorkspace::readable_document`].
pub(super) async fn readable_now(
    reading: &super::readings::Reading,
    (app, resource): (&str, &str),
) -> Result<Option<Vec<u8>>, String> {
    let (sealed, scoped) = reading.replica.read(|peer| {
        let publication = peer.publication();
        (
            publication.is_sealed_document(app, resource),
            publication.has_document_keys(app, resource),
        )
    });
    if !sealed {
        return Ok(reading.replica.read(|peer| peer.document(app, resource)));
    }
    let waiting = |missing: String| {
        reading
            .replica
            .read(|peer| peer.publication().waiting_for_update(app, resource))
            .map_or(missing, |waiting| waiting.to_string())
    };
    let device = reading
        .device
        .as_ref()
        .ok_or_else(|| waiting("this device holds no group keys".into()))?;
    let keys = read_document_keys(device, &reading.workspace, (app, resource), scoped, false)
        .await
        .map_err(waiting)?;
    // The document is taken as it stands and opened once the replica is let go: opening it
    // decrypts and merges every write of its history, which grows with what it holds, and every
    // policy read and commit of the workspace waited on it while a read held the replica.
    let Some(held) = reading
        .replica
        .read(|peer| peer.publication().held_document(app, resource))
    else {
        return Ok(None);
    };
    // Opened over what its last read merged: a document nobody wrote since reads as that read
    // made it, and one being written merges only the writes it took since.
    let route = (app.to_owned(), resource.to_owned());
    let prior = if scoped {
        None
    } else {
        reading.opened.take(&route)
    };
    let (opened, next) = held.opened_over(&mut keys.open(), prior)?;
    if !scoped && let Some(next) = next {
        reading.opened.keep(route, next);
    }
    Ok(Some(merged_or_named(
        opened,
        &reading.workspace,
        (app, resource),
    )))
}

/// The sealed documents of a workspace as their last reads opened them, so the next read of each
/// opens only the writes it took since ([`tilo_peer_state::HeldDocument::opened_over`]).
///
/// What each read is kept; the merged document a later write is merged into is kept only for the
/// documents read most lately, as it is far larger: a document being written is read again and
/// again, one at rest is read as it was saved.
#[derive(Default)]
pub(super) struct MergedDocuments(std::sync::Mutex<OpenedState>);

#[derive(Default)]
struct OpenedState {
    documents: std::collections::HashMap<(String, String), (u64, tilo_peer_state::MergedDocument)>,
    reads: u64,
}

impl MergedDocuments {
    /// Documents whose merged state is kept.
    const MERGED: usize = 8;

    /// Documents whose read is kept at all: past it, the read longest ago goes.
    const READS: usize = 4096;

    /// What the last read of `route` left, taken out: a read beside it opens the document whole.
    fn take(&self, route: &(String, String)) -> Option<tilo_peer_state::MergedDocument> {
        let mut state = self.0.lock_recover();
        state.documents.remove(route).map(|(_, opened)| opened)
    }

    /// Keep what a read of `route` left, for the next one.
    fn keep(&self, route: (String, String), opened: tilo_peer_state::MergedDocument) {
        let mut state = self.0.lock_recover();
        state.reads = state.reads.saturating_add(1);
        let at = state.reads;
        state.documents.insert(route, (at, opened));
        let mut merged: Vec<(u64, (String, String))> = state
            .documents
            .iter()
            .filter(|(_, (_, opened))| opened.holds_merged())
            .map(|(route, (at, _))| (*at, route.clone()))
            .collect();
        if merged.len() > Self::MERGED {
            merged.sort_unstable();
            for (_, route) in merged
                .iter()
                .take(merged.len().saturating_sub(Self::MERGED))
            {
                if let Some((_, opened)) = state.documents.get_mut(route) {
                    opened.lighten();
                }
            }
        }
        if state.documents.len() > Self::READS {
            let mut by_age: Vec<(u64, (String, String))> = state
                .documents
                .iter()
                .map(|(route, (at, _))| (*at, route.clone()))
                .collect();
            by_age.sort_unstable();
            let over = state.documents.len().saturating_sub(Self::READS);
            for (_, route) in by_age.into_iter().take(over) {
                state.documents.remove(&route);
            }
        }
    }
}

/// `resource` of `app` as `peer` holds it, its sealed history opened with `keys` where it has
/// them; a write of the history that does not merge is left out and named in the log.
fn opened_readable(
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
    // A write stuck behind a change its writer never sent is no part of the document anyone will
    // ever read: the document is whole without it.
    let unmerged: Vec<String> = opened
        .unmerged
        .iter()
        .filter(|write| !matches!(write.reason, UnmergedReason::BehindUnsent { .. }))
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

/// Publish `changes` to `record` of `app` into `candidate` as `key`'s write:
/// sealed with `keys` for a sealed record, in the clear otherwise.
///
/// The first sealed write of a record is the whole record, its cleartext base
/// with `changes`: a reader holding only the record's ciphertext history — a
/// page that joins later, whose host replays nothing the workspace's
/// governance already covers — reads the record from it, where a change
/// alone would wait forever for a base that never comes.
pub(super) fn publish_changes(
    candidate: &mut StatePeer,
    keys: Option<&DocumentKeys>,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    match keys {
        None => candidate
            .publication_mut()
            .publish_incremental(app, record, changes, key, now),
        Some(keys) if !candidate.publication().is_sealed_document(app, record) => {
            let mut document = match candidate.document(app, record) {
                Some(base) => {
                    automerge::AutoCommit::load(&base).map_err(|error| error.to_string())?
                }
                None => automerge::AutoCommit::new(),
            };
            tilo_automerge::ApplyIncoming::load_incoming(&mut document, &changes)
                .map_err(|error| error.to_string())?;
            refound(
                candidate,
                Some(keys),
                (app, record),
                document.save(),
                key,
                now,
            )
        }
        Some(keys) => candidate.publication_mut().publish_sealed_incremental(
            app,
            record,
            changes,
            &mut keys.seal(ContentRetention::Delta),
            key,
            now,
        ),
    }
}

/// Publish `record` of `app` as the workspace's own checkpoint of it, from
/// `document`, the record as its key holder reads it: sealed with `keys` for a
/// sealed record, in the clear otherwise. This is what founds a route again
/// after its retained history lost the base its deltas were written over
/// ([`Workspaces::refound_document`]): the checkpoint is the whole record, so a
/// device that joins afterwards reads it and the writes after it.
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
    keys: Option<&DocumentKeys>,
    (app, record): (&str, &str),
    document: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    match keys {
        None => {
            let snapshot = collab_types::DocumentMutation::Snapshot(document);
            let message = candidate.publication_mut().publish_document(
                app,
                record,
                snapshot.data().to_vec(),
                key,
                now,
            )?;
            Ok(message)
        }
        // The record is sealed: the checkpoint is the whole record, sealed as
        // the workspace's own, and the sealed writer's history keeps it.
        Some(keys) => candidate.publication_mut().publish_sealed_checkpoint(
            app,
            record,
            document,
            &mut keys.seal(collab_types::ContentRetention::Checkpoint),
            key,
            now,
        ),
    }
}

/// Start `record` of `app` in `candidate` as the workspace's host, signed with
/// `key` and sealed with `keys`: the checkpoint every record is born from,
/// which holds nothing, then `changes`, what the record starts with. A record
/// that needs keys of its own is written in the clear not even empty: its
/// first sealed write is the whole record and founds it alone.
pub(super) fn start_sealed(
    candidate: &mut StatePeer,
    keys: &DocumentKeys,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    if !candidate.holds_document(app, record)
        && !candidate.requires_document_keys(app, record, now)?
    {
        candidate.publication_mut().publish_document(
            app,
            record,
            automerge::AutoCommit::new().save(),
            key,
            now,
        )?;
    }
    publish_changes(candidate, Some(keys), (app, record), changes, key, now)
}

/// Publish `changes` a bot's device `bot` made to a sealed record into
/// `candidate`: decided as the bot's own sealed write, sealed with `keys`
/// and published under the governor's `key`, which the workspace's members take
/// it from, as they take a bot's changes to a record in the clear from the
/// governor's checkpoint.
#[cfg(feature = "agents")]
pub(super) fn publish_changes_for(
    candidate: &mut StatePeer,
    keys: &DocumentKeys,
    bot: &SigningKey,
    route: (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    candidate.publication_mut().publish_sealed_incremental_for(
        bot,
        route,
        changes,
        &mut keys.seal(ContentRetention::Delta),
        key,
        now,
    )
}

impl Workspaces {
    /// The content keys of `route`'s workspace, for the workspace service's endpoints
    /// (workspace actions for members' pages and bots), as this device holds them.
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

    async fn own_route_keys_read_by(
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
        let peer = self.state(&route.cluster_id).await?;
        let isolated =
            match peer.requires_document_keys(&route.app_id, &route.document, tilo_clock::wall()) {
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
    pub(crate) async fn join_keys(&self, workspace: &str) -> bool {
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
    /// checkpoint, compacted by this host as the workspace's governor and
    /// published like any compaction, which every member takes in place of
    /// exactly the writes it covers.
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
        keys.compact(
            &mut candidate,
            (app, record),
            Some(rebuilt),
            &self.inner.host.key,
            now,
        )
        .map_err(CollaborationError::new)?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        drop(governed);
        Ok(())
    }

    /// The keys a record another writer founds is founded under: a record that needs keys of
    /// its own is founded under them, so even its empty base is not written in the clear. None
    /// for a `whole` write, which is the record itself, and for a record that needs none.
    pub(super) async fn founding_keys(
        &self,
        workspace: &GovernedWorkspace,
        app: &str,
        document: &str,
        whole: bool,
    ) -> Result<Option<DocumentKeys>, String> {
        if whole
            || !workspace
                .peer()
                .requires_document_keys(app, document, tilo_clock::wall())?
        {
            return Ok(None);
        }
        self.sealing_keys(workspace, app, document).await
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
        let mut compacted = 0usize;
        for (app, record) in &due {
            let compaction = match workspace.document_keys(app, record).await {
                Ok(Some(keys)) => keys.compact(
                    &mut candidate,
                    (app, record),
                    None,
                    &self.inner.host.key,
                    now,
                ),
                Ok(None) => continue,
                Err(error) => Err(error),
            };
            match compaction {
                Ok(_) => compacted = compacted.saturating_add(1),
                Err(error) => {
                    workspace.compaction_wait = COMPACTION_RETRY;
                    tracing::warn!(workspace = %workspace.workspace(), %app, %record, %error,
                        "a sealed record history was not compacted");
                }
            }
        }
        if compacted == 0 {
            return Ok(());
        }
        let collaboration = workspace.collaboration().clone();
        workspace.commit(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(workspace).await
    }
}

#[cfg(all(test, feature = "agents"))]
mod behind_tests;
#[cfg(all(test, feature = "agents", feature = "apps"))]
mod chain_head_tests;
#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub mod fixtures;
#[cfg(all(test, feature = "agents"))]
mod folded_tests;
#[cfg(all(test, feature = "agents"))]
pub(crate) mod tests;
