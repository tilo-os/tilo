//! The governor's own reads and writes of records its members write sealed.
//!
//! In a workspace whose channels this relay takes part in, members seal every
//! write of a record, and the governor's copy keeps those writes as the
//! record's ciphertext history
//! ([`tilo_peer_state::PeerPublication::is_sealed_record`]). As a member of
//! the channels the relay holds their content keys: it reads such a record by
//! opening that history over the record's cleartext base, writes to it
//! sealed like any member does, and compacts the history before it reaches
//! its bound, so its copy goes on following the record however long the
//! workspace keeps writing. Every other device taking part in a channel does the
//! same with its own copy and publishes the checkpoint
//! ([`tilo_peer_state::PeerPublicationMut::compact_sealed_history`]): the
//! governor's copy follows that checkpoint like any other member write, and
//! no device's history waits for someone else to fold it.

use channel_crypto::ChannelContentKeys;
use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, SignedOp};
#[cfg(feature = "agents")]
use ed25519_dalek::SigningKey;
#[cfg(feature = "agents")]
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::OpenedRecord;
#[cfg(feature = "agents")]
use tilo_peer_state::StatePeer;

use super::{GovernedWorkspace, SystemTime, Workspaces};

/// Member writes followed before a failed compaction is tried again. A
/// record whose merged snapshot outgrew one mutation never compacts; trying
/// on every write opened and merged its whole history each time.
const COMPACTION_RETRY: usize = 256;

/// The content keys of one record's channel, as the node holds them.
pub(super) struct RecordKeys {
    keys: ChannelContentKeys,
    artifact: ArtifactId,
}

impl RecordKeys {
    /// Open any write of the record's history, under any epoch the keys
    /// hold, a compaction included.
    pub(super) fn open(&self) -> impl FnMut(&SignedOp) -> Result<Vec<u8>, String> + '_ {
        |op| {
            let metadata = op.encryption.ok_or("the write is not sealed")?;
            let ciphertext =
                collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
            self.keys
                .open_content(op.artifact, &metadata, &ciphertext)
                .map(|plaintext| plaintext.to_vec())
                .map_err(|error| error.to_string())
        }
    }

    /// Seal content of the record under the channel's current epoch.
    fn seal(
        &self,
        retention: ContentRetention,
    ) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + '_ {
        move |plaintext| {
            self.keys
                .seal_content(self.artifact, retention, plaintext)
                .map_err(|error| error.to_string())
        }
    }
}

impl GovernedWorkspace {
    /// The content keys of `route`'s channel as the device this host runs on
    /// holds them, without opening a channel it has none of: `None` when
    /// this host runs on a device that takes part in no channel of the workspace.
    pub(super) async fn route_keys(
        &self,
        route: &collab_types::ChannelRoute,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let device = self.device.as_ref()?;
        device.channel_keys(self.workspace(), route, false).await
    }

    /// Whether this host takes part in the channel of `record` of `app` and
    /// holds its keys: it can then read the record's sealed writes.
    pub(super) async fn takes_part_in_channel(&self, app: &str, record: &str) -> bool {
        let route = collab_types::ChannelRoute::new(app, record);
        matches!(self.route_keys(&route).await, Some(Ok(_)))
    }

    /// The keys of `record` of `app`'s channel when the record is sealed:
    /// `None` for a record written in the clear.
    ///
    /// # Errors
    ///
    /// The record is sealed and this host's device does not take part in its
    /// channel, or the channel's keys cannot be read.
    pub(super) async fn record_keys(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<RecordKeys>, String> {
        if !self.peer.publication().is_sealed_record(app, record) {
            return Ok(None);
        }
        let workspace = self.workspace();
        let route = collab_types::ChannelRoute::new(app, record);
        let keys = self
            .route_keys(&route)
            .await
            .ok_or("the record is sealed and this host takes part in no channel")??;
        Ok(Some(RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, workspace, record),
        }))
    }

    /// The plaintext of `op`, a member's sealed write of `record` of `app`,
    /// opened with its channel's keys: `None` when this host takes no part in
    /// the workspace's channels or holds no keys for this one yet.
    ///
    /// # Errors
    ///
    /// The keys cannot be read, or the write does not open with them.
    pub(super) async fn open_write(
        &self,
        app: &str,
        record: &str,
        op: &SignedOp,
    ) -> Result<Option<Vec<u8>>, String> {
        let workspace = self.workspace();
        let route = collab_types::ChannelRoute::new(app, record);
        let Some(Ok(keys)) = self.route_keys(&route).await else {
            return Ok(None);
        };
        let keys = RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, workspace, record),
        };
        let mut open = keys.open();
        open(op).map(Some)
    }

    /// `record` of `app` as the governor reads it: the record itself, or for
    /// a sealed one its history opened with the channel's keys, and the
    /// writes the read leaves out ([`OpenedRecord`]). A sealed write that
    /// does not open, or waits for a change the record lacks, is left out
    /// and named in the log: the workspace reads the rest, and the write
    /// stays in the history for when what it waits for comes.
    ///
    /// # Errors
    ///
    /// As [`Self::record_keys`], or the history's changes are malformed.
    pub(super) async fn opened_record(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<OpenedRecord>, String> {
        let Some(keys) = self.record_keys(app, record).await? else {
            return Ok(self
                .peer
                .document(app, record)
                .map(|document| OpenedRecord {
                    document,
                    unmerged: Vec::new(),
                }));
        };
        let Some(opened) =
            self.peer
                .publication()
                .opened_document(app, record, &mut keys.open())?
        else {
            return Ok(None);
        };
        if !opened.unmerged.is_empty() {
            // One writer wrote a sequence twice: the record keeps the change
            // that reached it first, nothing is lost, and no operator has to
            // act. A write left out for any other reason is a loss.
            let writes: Vec<String> = opened.unmerged.iter().map(ToString::to_string).collect();
            if opened.only_repeated_sequences() {
                tracing::info!(
                    workspace = %self.workspace(),
                    %app,
                    %record,
                    writes = ?writes,
                    "a sealed record keeps the change that reached it first of a writer's sequence"
                );
            } else {
                tracing::warn!(
                    workspace = %self.workspace(),
                    %app,
                    %record,
                    left_out = writes.len(),
                    writes = ?writes,
                    "a sealed record is read without writes of its history that do not merge; they are kept"
                );
            }
        }
        Ok(Some(opened))
    }

    /// [`Self::opened_record`]'s document alone, saved: the record's history
    /// opened as it reads.
    ///
    /// # Errors
    ///
    /// As [`Self::opened_record`].
    pub(super) async fn readable_document(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(self
            .opened_record(app, record)
            .await?
            .map(|opened| opened.document))
    }
}

/// Publish `changes` to `record` of `app` into `candidate` as `key`'s write:
/// sealed with `keys` for a sealed record, in the clear otherwise.
#[cfg(feature = "agents")]
pub(super) fn publish_changes(
    candidate: &mut StatePeer,
    keys: Option<&RecordKeys>,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    match keys {
        None => candidate
            .publication_mut()
            .publish_incremental(app, record, changes, key, now),
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

/// Start `record` of `app` in `candidate` as the workspace's host, signed with
/// `key` and sealed with `keys`: the checkpoint every record is born from,
/// which holds nothing, then `changes`, what the record starts with.
#[cfg(feature = "agents")]
pub(super) fn start_sealed(
    candidate: &mut StatePeer,
    keys: &RecordKeys,
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
    keys: &RecordKeys,
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

/// The relay's compactor folds the sealed history of the channels this
/// relay takes part in with their keys, so a newcomer replays one snapshot and
/// the writes since rather than every write the workspace's members ever made.
impl crate::compaction::ParticipantKeys for Workspaces {
    fn keys<'a>(
        &'a self,
        route: &'a crate::compaction::Route,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ChannelContentKeys>> + Send + 'a>>
    {
        Box::pin(async move {
            let device = self.device_keys()?;
            let named = collab_types::ChannelRoute::new(&route.app_id, &route.record_id);
            device
                .channel_keys(&route.cluster_id, &named, false)
                .await?
                .ok()
        })
    }
}

impl Workspaces {
    /// Take part in the channel of `record` of `app` in `workspace`: this host's
    /// device asks the channel's participants for access and holds its keys once
    /// one admits it. `false` when it takes part in no channel of the workspace,
    /// or is no reader of this one. Asked before the workspace is locked, since
    /// the ask waits on the channel's service for the group to admit this device.
    pub(crate) async fn join_channel(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Some(device) = self.device_keys() else {
            return false;
        };
        let route = collab_types::ChannelRoute::new(app, record);
        matches!(
            device.channel_keys(workspace, &route, true).await,
            Some(Ok(_))
        )
    }

    /// Seal the records the workspaces this relay takes part in hold in the clear, from
    /// now on: the host's operator decided it (the relay's `reseal`).
    #[cfg(feature = "agents")]
    pub fn reseal_existing(&self) {
        self.inner
            .resealing
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(feature = "agents")]
    fn resealing(&self) -> bool {
        self.inner
            .resealing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Seal every record of the workspaces this relay takes part in: open the channel
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
                            candidate.seal_record(app, record);
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
                        SystemTime::now(),
                        |_, _| Ok(collaboration),
                    ) {
                        Ok(_) => self.settle(&mut workspace).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = settled {
                        tracing::warn!(workspace = %workspace.workspace(), %error,
                            "the room's records were not sealed");
                        continue;
                    }
                }
                (
                    workspace.workspace().to_owned(),
                    workspace.workspace_id(),
                    workspace.peer().sealed_routes(),
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
            for (app, record) in routes {
                let channel = neighbour_wire::app_document_channel(&app, &record);
                let route = crate::compaction::Route {
                    app_id: app,
                    cluster_id: name.clone(),
                    record_id: record,
                };
                if let Err(error) = compactor
                    .reseal(&workspace, workspace_id, channel, &route)
                    .await
                {
                    tracing::warn!(workspace = %name, app = %route.app_id, record = %route.record_id,
                        %error, "a record's writes made in the clear were not sealed");
                }
            }
            drop(workspace);
        }
    }

    /// The keys the governor writes `record` of `app` with: in a workspace whose
    /// channels this relay takes part in every new record is sealed from its
    /// first write, so its channel is opened first when it has none (its
    /// genesis, published before anything is written there). A record it
    /// already holds in the clear stays so until the host's operator decides
    /// to seal them ([`Workspaces::reseal_existing`]). `None` in a workspace it does
    /// not take part in, where records are written in the clear.
    ///
    /// # Errors
    ///
    /// As [`GovernedRoom::record_keys`], or the channel cannot be opened.
    #[cfg(feature = "agents")]
    pub(super) async fn sealing_keys(
        &self,
        workspace: &GovernedWorkspace,
        app: &str,
        record: &str,
    ) -> Result<Option<RecordKeys>, String> {
        if !workspace.seals() {
            return workspace.record_keys(app, record).await;
        }
        let held_in_the_clear = workspace.peer().holds_document(app, record)
            && !workspace.peer().publication().is_sealed_record(app, record);
        if held_in_the_clear && !self.resealing() {
            return workspace.record_keys(app, record).await;
        }
        let name = workspace.workspace();
        let route = collab_types::ChannelRoute::new(app, record);
        // The device's channel service opens the record's channel when it has
        // none yet, as the workspace's writes ask it to.
        let Some(device) = &workspace.device else {
            return workspace.record_keys(app, record).await;
        };
        let keys = device
            .channel_keys(name, &route, true)
            .await
            .ok_or("the record is sealed and this host takes part in no channel")??;
        Ok(Some(RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, name, record),
        }))
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, and commit and publish the
    /// compactions. A record whose history does not merge yet (a write it
    /// depends on has not arrived) waits for a later call.
    pub(super) async fn compact_sealed_histories(
        &self,
        workspace: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let due = workspace.peer().publication().sealed_records_to_compact();
        if due.is_empty() {
            return Ok(());
        }
        if workspace.compaction_wait > 0 {
            workspace.compaction_wait = workspace.compaction_wait.saturating_sub(1);
            return Ok(());
        }
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        let mut compacted = 0usize;
        for (app, record) in &due {
            let compaction = match workspace.record_keys(app, record).await {
                Ok(Some(keys)) => candidate.publication_mut().compact_sealed_history(
                    app,
                    record,
                    &mut keys.open(),
                    &mut keys.seal(ContentRetention::Checkpoint),
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

    /// Repair the sealed history of `record` of `app` in `workspace` once:
    /// one writer wrote a sequence twice there, so every read of the history
    /// keeps the change that reached the record first and leaves out the
    /// other one. Compacting the history into one sealed checkpoint
    /// supersedes the write it leaves out and publishes a record every
    /// device following it reads whole, through the same path as any other
    /// write — the checkpoint is the repair, not a special message.
    ///
    /// Paced by [`GovernedWorkspace::compaction_wait`] as
    /// [`Self::compact_sealed_histories`] is, so a history that cannot be
    /// compacted is not opened and merged on every read.
    pub(super) async fn repair_repeated_sequence(&self, workspace: &str, app: &str, record: &str) {
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let mut governed = state.lock().await;
        self.repair_in(&mut governed, app, record).await;
    }

    /// [`Self::repair_repeated_sequence`] of a workspace the caller has
    /// locked.
    pub(super) async fn repair_in(
        &self,
        governed: &mut GovernedWorkspace,
        app: &str,
        record: &str,
    ) {
        if governed.compaction_wait > 0 {
            governed.compaction_wait = governed.compaction_wait.saturating_sub(1);
            return;
        }
        let now = SystemTime::now();
        let mut candidate = governed.peer().clone();
        let compaction = match governed.record_keys(app, record).await {
            Ok(Some(keys)) => candidate.publication_mut().compact_sealed_history(
                app,
                record,
                &mut keys.open(),
                &mut keys.seal(ContentRetention::Checkpoint),
                &self.inner.host.key,
                now,
            ),
            Ok(None) => return,
            Err(error) => Err(error),
        };
        if let Err(error) = compaction {
            governed.compaction_wait = COMPACTION_RETRY;
            tracing::warn!(workspace = %governed.workspace(), %app, %record, %error,
                "a sealed record repeating a writer's sequence was not repaired");
            return;
        }
        let collaboration = governed.collaboration().clone();
        if let Err(error) = governed.commit(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        }) {
            governed.compaction_wait = COMPACTION_RETRY;
            tracing::warn!(workspace = %governed.workspace(), %app, %record, %error,
                "a sealed record repeating a writer's sequence was not repaired");
            return;
        }
        if let Err(error) = self.settle(governed).await {
            tracing::warn!(workspace = %governed.workspace(), %app, %record, %error,
                "a repaired sealed record was not published");
        }
    }
}

#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub mod fixtures;
#[cfg(all(test, feature = "agents"))]
pub(crate) mod tests;
