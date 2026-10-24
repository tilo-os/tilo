//! The governor's own reads and writes of records its members write sealed.
//!
//! In a room whose channels this relay stewards, members seal every write
//! of a record, and the governor's copy keeps those writes as the record's
//! ciphertext history ([`tilo_peer_state::PeerPublication::is_sealed_record`]). As the channels'
//! steward the relay holds their content keys: it reads such a record by
//! opening that history over the record's cleartext base, writes to it
//! sealed like any member does, and compacts the history before it reaches
//! its bound, so its copy goes on following the record however long the
//! room keeps writing.

use channel_crypto::ChannelContentKeys;
use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, SignedOp};
#[cfg(feature = "agents")]
use ed25519_dalek::SigningKey;
#[cfg(feature = "agents")]
use neighbour_wire::NeighbourMsg;
#[cfg(feature = "agents")]
use tilo_peer_state::StatePeer;

use super::{GovernedRoom, Rooms, SystemTime};

/// Member writes followed before a failed compaction is tried again. A
/// record whose merged snapshot outgrew one mutation never compacts; trying
/// on every write opened and merged its whole history each time.
const COMPACTION_RETRY: usize = 256;

/// The content keys of one record's channel, as its steward holds them.
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

impl GovernedRoom {
    /// Whether this relay stewards the channel of `record` of `app` and
    /// holds its keys: it can then read the record's sealed writes.
    pub(super) async fn stewards_channel(&self, app: &str, record: &str) -> bool {
        let Some(steward) = self.stewardship.as_ref() else {
            return false;
        };
        let channel = collab_types::document_channel_id(app, self.room(), record);
        steward.channels.lock().await.existing_keys(channel).is_ok()
    }

    /// The keys of `record` of `app`'s channel when the record is sealed:
    /// `None` for a record written in the clear.
    ///
    /// # Errors
    ///
    /// The record is sealed and this relay does not steward its channel,
    /// or the channel's keys cannot be read.
    pub(super) async fn record_keys(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<RecordKeys>, String> {
        if !self.peer.publication().is_sealed_record(app, record) {
            return Ok(None);
        }
        let steward = self
            .stewardship
            .as_ref()
            .ok_or("the record is sealed and this relay does not steward its channel")?;
        let room = self.room();
        let channel = collab_types::document_channel_id(app, room, record);
        let keys = steward.channels.lock().await.existing_keys(channel)?;
        Ok(Some(RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, room, record),
        }))
    }

    /// The plaintext of `op`, a member's sealed write of `record` of `app`,
    /// opened with its channel's keys: `None` when this relay does not
    /// steward the room's channels or holds no keys for this one yet.
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
        let Some(steward) = self.stewardship.as_ref() else {
            return Ok(None);
        };
        let room = self.room();
        let channel = collab_types::document_channel_id(app, room, record);
        let Ok(keys) = steward.channels.lock().await.existing_keys(channel) else {
            return Ok(None);
        };
        let keys = RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, room, record),
        };
        let mut open = keys.open();
        open(op).map(Some)
    }

    /// `record` of `app` as the governor reads it, saved: the record itself,
    /// or for a sealed one its history opened with the channel's keys.
    /// A sealed write that does not open, or waits for a change the record
    /// lacks, is left out and named in the log: the room reads the rest,
    /// and the write stays in the history for when what it waits for comes.
    ///
    /// # Errors
    ///
    /// As [`Self::record_keys`], or the history's changes are malformed.
    pub(super) async fn readable_document(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(keys) = self.record_keys(app, record).await? else {
            return Ok(self.peer.document(app, record));
        };
        let Some(opened) =
            self.peer
                .publication()
                .opened_document(app, record, &mut keys.open())?
        else {
            return Ok(None);
        };
        if !opened.unmerged.is_empty() {
            let writes: Vec<String> = opened.unmerged.iter().map(ToString::to_string).collect();
            tracing::warn!(
                room = %self.room(),
                %app,
                %record,
                left_out = writes.len(),
                writes = ?writes,
                "a sealed record is read without writes of its history that do not merge; they are kept"
            );
        }
        Ok(Some(opened.document))
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

/// Start `record` of `app` in `candidate` as the room's host, signed with
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
/// and published under the governor's `key`, which the room's members take
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
/// relay stewards with their keys, so a newcomer replays one snapshot and
/// the writes since rather than every write the room's members ever made.
impl crate::compaction::StewardedKeys for Rooms {
    fn keys<'a>(
        &'a self,
        route: &'a crate::compaction::Route,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ChannelContentKeys>> + Send + 'a>>
    {
        Box::pin(async move {
            let state = self.room(&route.cluster_id).await.ok()?;
            let steward = state.lock().await.stewardship.clone()?;
            let channel = collab_types::document_channel_id(
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            );
            steward.channels.lock().await.existing_keys(channel).ok()
        })
    }
}

impl Rooms {
    /// Seal the records the rooms this relay stewards hold in the clear, from
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

    /// Seal every record of the rooms this relay stewards: open the channel
    /// of each that has none yet, and seal what each channel retains in the
    /// clear, as `compactor` does ([`crate::compaction::Compactor::reseal`]):
    /// records written before their channels were opened.
    #[cfg(feature = "agents")]
    pub async fn reseal_stewarded(&self, compactor: &crate::compaction::Compactor) {
        let rooms: Vec<_> = self.inner.rooms.lock().await.values().cloned().collect();
        for state in rooms {
            let (name, workspace_id, routes) = {
                let mut room = state.lock().await;
                if room.stewardship.is_none() {
                    continue;
                }
                let plaintext = room.peer().plaintext_routes();
                let mut candidate = room.peer().clone();
                for (app, record) in &plaintext {
                    match self.sealing_keys(&room, app, record).await {
                        Ok(_) => {
                            candidate.seal_record(app, record);
                        }
                        Err(error) => tracing::warn!(room = %room.room(), %app, %record,
                            %error, "a record's channel was not opened"),
                    }
                }
                if candidate.sealed_routes().len() != room.peer().sealed_routes().len() {
                    let collaboration = room.collaboration().clone();
                    let settled = match room.commit(
                        candidate,
                        &self.inner.host.key,
                        SystemTime::now(),
                        |_, _| Ok(collaboration),
                    ) {
                        Ok(_) => self.settle(&mut room).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = settled {
                        tracing::warn!(room = %room.room(), %error,
                            "the room's records were not sealed");
                        continue;
                    }
                }
                (
                    room.room().to_owned(),
                    room.workspace_id(),
                    room.peer().sealed_routes(),
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
                    tracing::warn!(room = %name, app = %route.app_id, record = %route.record_id,
                        %error, "a record's writes made in the clear were not sealed");
                }
            }
            drop(workspace);
        }
    }

    /// What each record channel of the rooms this relay stewards retains,
    /// read with the channel's keys where it has a channel: per record, its
    /// workspace, store channel and `(app, record)`.
    #[cfg(feature = "agents")]
    pub async fn stewarded_channels(
        &self,
    ) -> Vec<(u64, [u8; 32], (String, String), crate::compaction::Retained)> {
        let payloads = self.inner.host.access.shell_payloads().clone();
        let rooms: Vec<_> = self.inner.rooms.lock().await.values().cloned().collect();
        let mut out = Vec::new();
        for state in rooms {
            let (steward, name, workspace_id, routes) = {
                let room = state.lock().await;
                let Some(steward) = room.stewardship.clone() else {
                    continue;
                };
                let mut routes = room.peer().plaintext_routes();
                routes.extend(room.peer().sealed_routes());
                (steward, room.room().to_owned(), room.workspace_id(), routes)
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
            let store = workspace.lock().await.store.clone();
            drop(workspace);
            for (app, record) in routes {
                let keys = steward
                    .channels
                    .lock()
                    .await
                    .existing_keys(collab_types::document_channel_id(&app, &name, &record))
                    .ok();
                let open = |op: &SignedOp| {
                    let keys = keys.as_ref()?;
                    let metadata = op.encryption?;
                    let ciphertext = collab_types::sealed_ciphertext(op).ok()?;
                    keys.open_content(op.artifact, &metadata, &ciphertext)
                        .ok()
                        .map(|plaintext| plaintext.to_vec())
                };
                let channel = neighbour_wire::app_document_channel(&app, &record);
                let entries = store.channel_entries(&channel).unwrap_or_default();
                let retained = crate::compaction::retained(payloads.clone(), &entries, Some(&open));
                out.push((workspace_id, channel, (app, record), retained));
            }
        }
        out
    }

    /// The keys the governor writes `record` of `app` with: in a room whose
    /// channels this relay stewards every new record is sealed from its
    /// first write, so its channel is opened first when it has none (its
    /// genesis, published before anything is written there). A record it
    /// already holds in the clear stays so until the host's operator decides
    /// to seal them ([`Rooms::reseal_existing`]). `None` in a room it does
    /// not steward, where records are written in the clear.
    ///
    /// # Errors
    ///
    /// As [`GovernedRoom::record_keys`], or the channel cannot be opened.
    #[cfg(feature = "agents")]
    pub(super) async fn sealing_keys(
        &self,
        room: &GovernedRoom,
        app: &str,
        record: &str,
    ) -> Result<Option<RecordKeys>, String> {
        let Some(steward) = room.stewardship.clone() else {
            return room.record_keys(app, record).await;
        };
        let held_in_the_clear = room.peer().holds_document(app, record)
            && !room.peer().publication().is_sealed_record(app, record);
        if held_in_the_clear && !self.resealing() {
            return room.record_keys(app, record).await;
        }
        let name = room.room();
        let channel = collab_types::document_channel_id(app, name, record);
        let mut stewardship = steward.channels.lock().await;
        let keys = if let Ok(keys) = stewardship.existing_keys(channel) {
            keys
        } else {
            steward.learn_route(channel, &collab_types::ChannelRoute::new(app, record));
            self.open_record_channel(room, &steward, &mut stewardship, channel)
                .await?;
            stewardship.existing_keys(channel)?
        };
        Ok(Some(RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, name, record),
        }))
    }

    /// Compact every sealed record history of `room` that has used most of
    /// its bound into one sealed checkpoint, and commit and publish the
    /// compactions. A record whose history does not merge yet (a write it
    /// depends on has not arrived) waits for a later call.
    pub(super) async fn compact_sealed_histories(
        &self,
        room: &mut GovernedRoom,
    ) -> Result<(), String> {
        let due = room.peer().publication().sealed_records_to_compact();
        if due.is_empty() {
            return Ok(());
        }
        if room.compaction_wait > 0 {
            room.compaction_wait = room.compaction_wait.saturating_sub(1);
            return Ok(());
        }
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        let mut compacted = 0usize;
        for (app, record) in &due {
            let compaction = match room.record_keys(app, record).await {
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
                    room.compaction_wait = COMPACTION_RETRY;
                    tracing::warn!(room = %room.room(), %app, %record, %error,
                        "a sealed record history was not compacted");
                }
            }
        }
        if compacted == 0 {
            return Ok(());
        }
        let collaboration = room.collaboration().clone();
        room.commit(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(room).await
    }
}

#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub mod fixtures;
#[cfg(all(test, feature = "agents"))]
pub(crate) mod tests;
