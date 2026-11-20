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

use collab_types::{ArtifactId, ContentRetention, SignedOp};
use ed25519_dalek::SigningKey;
use key_group::ContentKeys;
use neighbour_wire::NeighbourMsg;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::StatePeer;

use tilo_peer_state::UnmergedReason;

use super::{GovernedWorkspace, SystemTime, Workspaces};

/// Member writes followed before a failed compaction is tried again. A
/// record whose history does not merge yet does not compact; trying on every
/// write opened and merged its whole history each time.
#[cfg_attr(
    not(any(feature = "agents", feature = "web")),
    allow(
        dead_code,
        reason = "sealed histories are followed only where they are read"
    )
)]
const COMPACTION_RETRY: usize = 256;

/// The content keys of one document, as the node holds them: its readers'
/// group's keys, scoped to the document.
pub(super) struct DocumentKeys {
    keys: ContentKeys,
    artifact: ArtifactId,
    /// The workspace's own keys, for a document only some of its members read: what its writes
    /// sealed before reader sets were sealed under, as every document's were then
    /// ([`collab_types::artifact_sealed_before_reader_sets`]).
    before_reader_sets: Option<ContentKeys>,
}

impl DocumentKeys {
    /// `keys`, of the group of `readers` in `workspace`, for document `doc`
    /// ([`collab_types::content_artifact_id`]).
    pub(super) fn of(keys: ContentKeys, workspace: &str, readers: &str, doc: &str) -> Self {
        Self {
            keys,
            artifact: collab_types::content_artifact_id(workspace, readers, doc),
            before_reader_sets: None,
        }
    }

    /// `keys`, of the group of the readers of `document` of `app` in `workspace`, for the
    /// document itself, under the scope its writes name ([`collab_types::document_artifact_id`]);
    /// with `workspace_keys` for a document only some members read, which opens what it holds
    /// sealed before reader sets.
    fn of_document(
        keys: ContentKeys,
        workspace_keys: Option<ContentKeys>,
        workspace: &str,
        (app, document): (&str, &str),
    ) -> Self {
        Self {
            keys,
            artifact: collab_types::document_artifact_id(app, workspace, document),
            before_reader_sets: workspace_keys,
        }
    }

    /// `keys`, the workspace's own, for a document of `app` every member
    /// reads.
    fn app_document(keys: ContentKeys, workspace: &str, (app, document): (&str, &str)) -> Self {
        Self::of(
            keys,
            workspace,
            collab_types::EVERY_MEMBER,
            &collab_types::app_document_id(app, document),
        )
    }

    /// The epoch of the keys: a rotation moves it.
    pub(super) fn epoch(&self) -> u64 {
        self.keys.epoch()
    }

    /// Open any write of the record's history, under any epoch the keys
    /// hold, a compaction included.
    pub(super) fn open(&self) -> impl FnMut(&SignedOp) -> Result<Vec<u8>, String> + '_ {
        |op| {
            let metadata = op.encryption.ok_or("the write is not sealed")?;
            let ciphertext =
                collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
            let opened = self.keys.open_content(op.artifact, &metadata, &ciphertext);
            // A write sealed before reader sets was sealed under the workspace's own keys.
            let opened = match (opened, &self.before_reader_sets) {
                (Err(_), Some(workspace_keys)) if op.artifact != self.artifact => {
                    workspace_keys.open_content(op.artifact, &metadata, &ciphertext)
                }
                (opened, _) => opened,
            };
            opened
                .map(|plaintext| plaintext.to_vec())
                .map_err(|error| error.to_string())
        }
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
        candidate
            .publication_mut()
            .admit_sealed_compaction(app, record, ops, now)
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

    /// [`Self::reader_keys`] for a write sealed to `readers`: the group is opened when this
    /// device takes part in none of it yet, as the first write in a fresh workspace finds it.
    #[cfg(any(feature = "apps", feature = "agents"))]
    pub(super) async fn writer_keys(&self, readers: &str) -> Option<Result<ContentKeys, String>> {
        let device = self.device.as_ref()?;
        device.group_keys(self.workspace(), readers, true).await
    }

    /// Whether this host holds the workspace's keys: it can then read sealed
    /// writes.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
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
        let waiting = |fallback: &str| {
            self.waiting_for_update(app, document)
                .unwrap_or_else(|| fallback.to_owned())
        };
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| waiting("this device holds no group keys"))?;
        // A resource store's document is read by the set its id names, every member's for any
        // other document.
        let readers = match collab_types::store_document_readers(document) {
            None | Some(collab_types::EVERY_MEMBER) => collab_types::EVERY_MEMBER.to_owned(),
            Some(_) => self
                .resources
                .known_readers(document)
                .ok_or("the reader set of the document is not known here")?,
        };
        let keys = device
            .group_keys(self.workspace(), &readers, false)
            .await
            .ok_or_else(|| waiting("this device takes no part in the readers' group"))??;
        let workspace_keys = if readers == collab_types::EVERY_MEMBER {
            None
        } else {
            self.route_keys().await.and_then(Result::ok)
        };
        Ok(Some(DocumentKeys::of_document(
            keys,
            workspace_keys,
            self.workspace(),
            (app, document),
        )))
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

    /// The plaintext of `op`, a member's sealed write of `record` of `app`,
    /// opened with its channel's keys: `None` when this host takes no part in
    /// the workspace's channels or holds no keys for this one yet.
    ///
    /// # Errors
    ///
    /// The keys cannot be read, or the write does not open with them.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    pub(super) async fn open_write(
        &self,
        app: &str,
        record: &str,
        op: &SignedOp,
    ) -> Result<Option<Vec<u8>>, String> {
        let workspace = self.workspace();
        let Some(Ok(keys)) = self.route_keys().await else {
            return Ok(None);
        };
        let keys = DocumentKeys::app_document(keys, workspace, (app, record));
        let mut open = keys.open();
        open(op).map(Some)
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

/// The keys of document `doc` that `readers` of `workspace` read, as `device` holds its readers'
/// group's keys.
async fn keys_of(
    device: Option<&super::DeviceKeys>,
    workspace: &str,
    readers: &str,
    doc: &str,
) -> Result<DocumentKeys, String> {
    let device = device.ok_or("this device holds no group keys")?;
    let keys = device
        .group_keys(workspace, readers, false)
        .await
        .ok_or("this device takes no part in the readers' group")??;
    Ok(DocumentKeys::of(keys, workspace, readers, doc))
}

/// [`GovernedWorkspace::readable_document`] as the workspace `reading` reads
/// holds it now, without the workspace's lock: the device's key service can
/// take long to answer (it settles a newcomer's access first), and every other
/// read and every member's write waited for it while a read held the workspace.
///
/// # Errors
///
/// As [`GovernedWorkspace::readable_document`].
#[cfg_attr(
    not(any(feature = "agents", feature = "web")),
    allow(
        dead_code,
        reason = "sealed documents are read only where a page or an agent reads them"
    )
)]
pub(super) async fn readable_now(
    reading: &super::readings::Reading,
    (app, resource): (&str, &str),
) -> Result<Option<Vec<u8>>, String> {
    let sealed = reading
        .replica
        .read(|peer| peer.publication().is_sealed_document(app, resource));
    let keys = if sealed {
        let keys = keys_of(
            reading.device.as_ref(),
            &reading.workspace,
            collab_types::EVERY_MEMBER,
            &collab_types::app_document_id(app, resource),
        )
        .await;
        // A document holding writes sealed before reader sets that no key holder has sealed again
        // is shown as waiting for the device that will, not as missing.
        let keys = keys.map_err(|missing| {
            reading
                .replica
                .read(|peer| peer.publication().waiting_for_update(app, resource))
                .map_or(missing, |waiting| waiting.to_string())
        })?;
        Some(keys)
    } else {
        None
    };
    let Some(keys) = keys else {
        return Ok(reading.replica.read(|peer| peer.document(app, resource)));
    };
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
    let prior = reading.opened.take(&route);
    let (opened, next) = held.opened_over(&mut keys.open(), prior)?;
    if let Some(next) = next {
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

#[cfg_attr(
    not(any(feature = "agents", feature = "web")),
    allow(
        dead_code,
        reason = "sealed documents are read only where a page or an agent reads them"
    )
)]
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
    device: Option<&super::DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
) -> Result<Vec<u8>, String> {
    if !peer.publication().is_sealed_document(app, resource) {
        return peer
            .document(app, resource)
            .ok_or_else(|| "the host holds no copy of it".to_owned());
    }
    let device = device.ok_or("it is sealed and this host has no channel keys")?;
    let keys = match device
        .group_keys(workspace, collab_types::EVERY_MEMBER, false)
        .await
    {
        None => return Err("it is sealed for a channel this host holds no keys of".to_owned()),
        Some(Err(error)) => return Err(format!("its channel's keys did not open: {error}")),
        Some(Ok(keys)) => keys,
    };
    let keys = DocumentKeys::app_document(keys, workspace, (app, resource));
    let opened = peer
        .publication()
        .opened_document(app, resource, &mut keys.open())
        .map_err(|error| format!("its sealed history does not open: {error}"))?
        .ok_or("its sealed history holds no write")?;
    // A write stuck behind a change its writer never sent is no part of the record anyone will
    // ever read: the record is whole without it.
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

/// The writes of `record` of `app` of `workspace`, as `peer` (a copy of the governor's state)
/// holds it, stuck behind a change their own writer never sent, opened with `device`'s keys: no
/// device will ever send what they wait for (see [`UnmergedReason::BehindUnsent`]). The governor
/// is the record's store, which every write of it reaches, so what it names so is what the
/// server's fold leaves out of the record for good. Empty for a record held in the clear, or one
/// this host holds no keys of.
pub(crate) async fn unsent_writes(
    peer: &StatePeer,
    device: Option<&super::DeviceKeys>,
    workspace: &str,
    (app, record): (&str, &str),
) -> Vec<SignedOp> {
    if !peer.publication().is_sealed_document(app, record) {
        return Vec::new();
    }
    let Some(device) = device else {
        return Vec::new();
    };
    let Some(Ok(keys)) = device
        .group_keys(workspace, collab_types::EVERY_MEMBER, false)
        .await
    else {
        return Vec::new();
    };
    let keys = DocumentKeys::app_document(keys, workspace, (app, record));
    let opened = match peer
        .publication()
        .opened_document(app, record, &mut keys.open())
    {
        Ok(Some(opened)) => opened,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(%workspace, %app, %record, %error, "a sealed record does not open");
            return Vec::new();
        }
    };
    let history = peer.publication().sealed_history(app, record);
    let mut unsent = Vec::new();
    for write in &opened.unmerged {
        if !matches!(write.reason, UnmergedReason::BehindUnsent { .. }) {
            continue;
        }
        unsent.extend(
            history
                .iter()
                .filter(|op| *op.author.as_bytes() == write.author && op.seq == write.seq)
                .cloned(),
        );
    }
    unsent
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
/// which holds nothing, then `changes`, what the record starts with.
pub(super) fn start_sealed(
    candidate: &mut StatePeer,
    keys: &DocumentKeys,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    if !candidate.holds_document(app, record) {
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

/// The server's compactor folds the sealed history of the channels this
/// server takes part in with their keys, so a newcomer replays one snapshot and
/// the writes since rather than every write the workspace's members ever made.
impl crate::compaction::GroupKeys for Workspaces {
    fn keys<'a>(
        &'a self,
        route: &'a crate::compaction::Route,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ContentKeys>> + Send + 'a>> {
        Box::pin(async move {
            let device = self.device_keys()?;
            device
                .group_keys(&route.cluster_id, collab_types::EVERY_MEMBER, false)
                .await?
                .ok()
        })
    }

    fn unsent<'a>(
        &'a self,
        route: &'a crate::compaction::Route,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<SignedOp>> + Send + 'a>> {
        Box::pin(async move {
            let Ok(state) = self.workspace(&route.cluster_id).await else {
                return Vec::new();
            };
            let (peer, device) = {
                let governed = state.lock().await;
                (governed.peer().clone(), governed.device.clone())
            };
            unsent_writes(
                &peer,
                device.as_ref(),
                &route.cluster_id,
                (&route.app_id, &route.document_id),
            )
            .await
        })
    }
}

impl Workspaces {
    /// Take part in `workspace`'s group: this host's device asks its key holders
    /// for access and holds its keys once one admits it. `false` when it takes
    /// part in no group of the workspace. Asked before the workspace is locked,
    /// since the ask waits on the key service for the group to admit this device.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
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
    ) -> Result<Option<tilo_peer_state::OpenedHistory>, super::CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        let keys = governed
            .document_keys(app, record)
            .await
            .map_err(super::CollaborationError::new)?;
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
    ) -> Result<(), super::CollaborationError> {
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        let keys = governed
            .document_keys(app, record)
            .await
            .map_err(super::CollaborationError::new)?
            .ok_or_else(|| super::CollaborationError::new("the record is held in the clear"))?;
        let now = tilo_clock::wall();
        let mut candidate = governed.peer().clone();
        keys.compact(
            &mut candidate,
            (app, record),
            Some(rebuilt),
            &self.inner.host.key,
            now,
        )
        .map_err(super::CollaborationError::new)?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        drop(governed);
        Ok(())
    }

    /// Seal the records the workspaces this server takes part in hold in the clear, from
    /// now on: the host's operator decided it (the server's `reseal`).
    #[cfg(feature = "agents")]
    pub fn reseal_existing(&self) {
        self.inner
            .resealing
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn resealing(&self) -> bool {
        self.inner
            .resealing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Seal every record of the workspaces this server takes part in: open the channel
    /// of each that has none yet, and seal what each channel retains in the
    /// clear, as `compactor` does ([`crate::compaction::Compactor::reseal`]):
    /// records written before their channels were opened.
    #[cfg(feature = "agents")]
    pub async fn reseal_own_channels(&self, compactor: &crate::compaction::Compactor) {
        let workspaces: Vec<_> = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for state in workspaces {
            let (name, workspace_id, routes) = {
                let mut workspace = state.lock().await;
                if !workspace.seals() {
                    continue;
                }
                let plaintext = workspace.peer().plaintext_routes();
                let mut candidate = workspace.peer().clone();
                for (app, record) in &plaintext {
                    match self.sealing_keys(&workspace, app, record).await {
                        Ok(_) => {
                            candidate.seal_document(app, record);
                        }
                        Err(error) => {
                            tracing::warn!(workspace = %workspace.workspace(), %app, %record,
                            %error, "a record's channel was not opened");
                        }
                    }
                }
                if candidate.sealed_routes().len() != workspace.peer().sealed_routes().len() {
                    let collaboration = workspace.collaboration().clone();
                    let settled = match workspace.commit(
                        candidate,
                        &self.inner.host.key,
                        tilo_clock::wall(),
                        |_, _| Ok(collaboration),
                    ) {
                        Ok(_) => self.settle(&mut workspace).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = settled {
                        tracing::warn!(workspace = %workspace.workspace(), %error,
                            "the workspace's records were not sealed");
                        continue;
                    }
                }
                let peer = workspace.peer();
                (
                    workspace.workspace().to_owned(),
                    workspace.workspace_id(),
                    peer.sealed_routes()
                        .into_iter()
                        .map(|(app, record)| {
                            let own = peer.own_resource(&app, &record);
                            (app, record, own)
                        })
                        .collect::<Vec<_>>(),
                )
            };
            let Some(workspace) = self
                .inner
                .host
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned()
            else {
                continue;
            };
            for (app, record, own) in routes {
                let channel = neighbour_wire::app_document_channel(&app, &record);
                let route = crate::compaction::Route {
                    app_id: app,
                    cluster_id: name.clone(),
                    document_id: record,
                    own,
                    readers: None,
                };
                if let Err(error) = compactor
                    .reseal(&workspace, workspace_id, channel, &route)
                    .await
                {
                    tracing::warn!(workspace = %name, app = %route.app_id, record = %route.document_id,
                        %error, "a record's writes made in the clear were not sealed");
                }
            }
            drop(workspace);
        }
    }

    /// Repair every sealed record of the workspaces this host governs whose store holds writes
    /// stuck behind a change their own writer never sent: written before a page's writes waited
    /// for their record, they kept every reader from the record, and no fold since left them
    /// out. Each such record's channel is folded without them now, as its next fold would
    /// ([`crate::compaction::Compactor::repair`]). Runs once as the host starts.
    #[cfg(any(feature = "agents", feature = "web"))]
    pub(crate) async fn fold_away_unsent(&self, compactor: &crate::compaction::Compactor) {
        let workspaces: Vec<_> = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for state in workspaces {
            let (peer, device, name, workspace_id) = {
                let governed = state.lock().await;
                (
                    governed.peer().clone(),
                    governed.device.clone(),
                    governed.workspace().to_owned(),
                    governed.workspace_id(),
                )
            };
            let mut holed = Vec::new();
            for (app, record) in peer.sealed_routes() {
                if !unsent_writes(&peer, device.as_ref(), &name, (&app, &record))
                    .await
                    .is_empty()
                {
                    let own = peer.own_resource(&app, &record);
                    holed.push((app, record, own));
                }
            }
            if holed.is_empty() {
                continue;
            }
            let Some(workspace) = self
                .inner
                .host
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned()
            else {
                continue;
            };
            for (app, record, own) in holed {
                let channel = neighbour_wire::app_document_channel(&app, &record);
                let route = crate::compaction::Route {
                    app_id: app,
                    cluster_id: name.clone(),
                    document_id: record,
                    own,
                    readers: None,
                };
                if let Err(error) = compactor
                    .repair(&workspace, workspace_id, channel, &route)
                    .await
                {
                    tracing::warn!(workspace = %name, app = %route.app_id, record = %route.document_id,
                        %error, "a record's writes stuck behind a change their writer never sent were not folded away");
                }
            }
        }
    }

    /// Seal again what the workspaces governed here hold sealed before reader sets
    /// ([`collab_types::artifact_sealed_before_reader_sets`]), as the first device holding their
    /// keys to open them since: each such document's history is compacted by this server, its
    /// content sealed under the document's own scope and signed as this server alone, and the
    /// writes it folds kept beside the document as their authors signed them
    /// ([`tilo_peer_state::PeerPublication::resealed_originals`]). A workspace whose keys this
    /// device does not hold yet is tried again while they arrive; one it holds none of waits for
    /// a device that does, and its readers are told so
    /// ([`tilo_peer_state::WaitingForUpdate`]). Called as the server starts.
    #[cfg(any(feature = "agents", feature = "web"))]
    pub(crate) async fn reseal_before_reader_sets(&self) {
        const TRIES: usize = 60;
        const BETWEEN: std::time::Duration = std::time::Duration::from_secs(5);
        for attempt in 0..TRIES {
            let workspaces: Vec<_> = self
                .inner
                .workspaces
                .lock()
                .await
                .values()
                .cloned()
                .collect();
            let mut waiting = 0usize;
            for state in workspaces {
                let mut workspace = state.lock().await;
                let awaiting = workspace.peer().publication().awaiting_reseals();
                if awaiting.is_empty() {
                    continue;
                }
                if !workspace.seals() || self.holds_as_store(&workspace) {
                    tracing::info!(workspace = %workspace.workspace(), documents = awaiting.len(),
                        "documents sealed before reader sets wait for a device holding the workspace's keys");
                    continue;
                }
                workspace.compaction_wait = 0;
                if let Err(error) = self.compact_sealed_histories(&mut workspace).await {
                    tracing::warn!(workspace = %workspace.workspace(), %error,
                        "documents sealed before reader sets were not sealed again");
                }
                let left = workspace.peer().publication().awaiting_reseals();
                tracing::info!(workspace = %workspace.workspace(),
                    sealed_again = awaiting.len().saturating_sub(left.len()), left = left.len(),
                    "documents sealed before reader sets sealed again under their own scope");
                waiting = waiting.saturating_add(left.len());
            }
            if waiting == 0 || attempt.saturating_add(1) == TRIES {
                return;
            }
            tokio::time::sleep(BETWEEN).await;
        }
    }

    /// The keys the governor writes `record` of `app` with: in a workspace whose
    /// channels this server takes part in every new record is sealed from its
    /// first write, so its channel is opened first when it has none (its
    /// genesis, published before anything is written there). A record it
    /// already holds in the clear stays so until the host's operator decides
    /// to seal them ([`Workspaces::reseal_existing`]). `None` in a workspace it does
    /// not take part in, where records are written in the clear.
    ///
    /// # Errors
    ///
    /// As [`GovernedWorkspace::document_keys`], or the channel cannot be opened.
    pub(super) async fn sealing_keys(
        &self,
        workspace: &GovernedWorkspace,
        app: &str,
        record: &str,
    ) -> Result<Option<DocumentKeys>, String> {
        if !workspace.seals() {
            return workspace.document_keys(app, record).await;
        }
        let held_in_the_clear = workspace.peer().holds_document(app, record)
            && !workspace
                .peer()
                .publication()
                .is_sealed_document(app, record);
        if held_in_the_clear && !self.resealing() {
            return workspace.document_keys(app, record).await;
        }
        let name = workspace.workspace();
        // The device's channel service opens the record's channel when it has
        // none yet, as the workspace's writes ask it to.
        let Some(device) = &workspace.device else {
            return workspace.document_keys(app, record).await;
        };
        let keys = device
            .group_keys(name, collab_types::EVERY_MEMBER, true)
            .await
            .ok_or("the record is sealed and this host takes part in no channel")??;
        Ok(Some(DocumentKeys::app_document(keys, name, (app, record))))
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, and commit and publish the
    /// compactions. A record whose history does not merge yet (a write it
    /// depends on has not arrived) waits for a later call.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
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
#[cfg(all(test, feature = "agents"))]
mod chain_head_tests;
#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub mod fixtures;
#[cfg(all(test, feature = "agents"))]
mod folded_tests;
#[cfg(all(test, feature = "agents"))]
pub(crate) mod tests;
