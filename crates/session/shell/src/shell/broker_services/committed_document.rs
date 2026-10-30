//! Reads the same native owner; local app storage is never an authority fallback.
//! Whole captures come from a bounded restore of the room's committed state,
//! never from the live peer. The changes since a revision this owner handed
//! a reader come from the committed view's own document
//! ([`tilo_joined_room::NativeCommittedView::read_changes`]), which is what
//! the room durably committed; anything only a restore can decide is read
//! whole.
use super::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads, NativeOwnerCommitService,
};
use std::sync::Arc;
use tilo_action_broker::services::HeldRevision;
impl NativeDocumentReads for NativeOwnerCommitService {
    fn prepare_application(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Result<Option<runtime_collab::committed_document::CreationWorkspace>, String> {
        context.revalidate()?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        let owner = &self.opened_owner("native document owner unavailable")?;
        if owner.with_current(room, |view| {
            Ok(view
                .peer()
                .document(&route.app_id, &route.record_id)
                .is_some())
        })? || self.holds_sealed_writes(route)
        {
            context.revalidate()?;
            return Ok(None);
        }
        // A missing record of a room another device governs is made empty
        // by this device ([`Self::prepare_application_elsewhere`]): a room
        // hosted on a device, this one included, keeps its records in sealed
        // channels each app opens itself.
        Err("this room's records are sealed channels its apps open themselves".into())
    }
    fn prepare_application_elsewhere(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Option<tilo_action_broker::services::PreparingElsewhere> {
        let room = super::super::state::RoomId::parse(&context.route().cluster_id)?;
        if !self.network.as_ref()?.governed_elsewhere(room) {
            return None;
        }
        Some(preparing_here(
            Arc::clone(self.network.as_ref()?),
            self.opened_owner("").ok()?,
            (context.clone(), self.tree.clone()),
            room,
        ))
    }
    /// A record of a room whose records are sealed channels that neither
    /// this device's owner nor its broker holds anything of may have been
    /// written on another device, or before this session: what the room's
    /// host retains of it reaches this device only in the host's replay.
    /// The Open waits for the broker to take that replay before it decides
    /// the record is new.
    fn await_sealed_history(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Option<tilo_action_broker::services::AwaitingHistory> {
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)?;
        self.network.as_ref()?;
        self.sealed_writes.as_ref()?;
        if !self.tree.read().seals_records(room)
            || self.owner_holds(room, &route.app_id, &route.record_id)
            || self.holds_sealed_writes(route)
        {
            return None;
        }
        let landed = self.network.as_ref()?.await_sealed_replay(room);
        Some(Box::pin(async move {
            // A dropped sender is a network gone: the Open decides on what
            // this device holds.
            let _ = landed.await;
        }))
    }
    fn initialize_application(
        &self,
        context: &NativeDocumentReadContext,
        revision: [u8; 32],
        kind: &str,
        document: &[u8],
    ) -> Result<[u8; 32], String> {
        self.authorize(context)?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid application initialization room")?;
        let evidence = context
            .application_creation()
            .filter(|evidence| evidence.valid())
            .ok_or("application initialization requires original signed launch")?;
        if context.record_kind() != Some(kind) {
            return Err("application initialization kind changed".into());
        }
        let declared = tilo_joined_room::ApplicationRecord::declared(
            &route.app_id,
            &route.record_id,
            kind,
            &evidence.declared_kinds,
        )?;
        let committed = self
            .network
            .as_ref()
            .ok_or("native selected custody unavailable")?
            .initialize_native_application(
                &self.opened_owner("native document owner unavailable")?,
                room,
                context.certificate(),
                tilo_joined_room::ApplicationInitialization {
                    route: &declared,
                    revision,
                    document,
                },
            )?;
        context.revalidate()?;
        Ok(committed)
    }
    fn requires_room(&self, room: &str) -> bool {
        super::super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().commits_native_documents(room))
    }
    fn seals_records(&self, room: &str) -> bool {
        super::super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().seals_records(room))
    }
    /// A room this device commits answers every read. A sealed room answers
    /// the records it already holds, which a cold app has no other way to
    /// load: its own channel only carries what is written while it is open.
    /// It holds a record when its owner does, or when this device holds the
    /// record's sealed writes: its own, and those the room's host relayed or
    /// replayed to it, wherever the room is hosted. A record the sealed room
    /// does not hold yet is new, and the app makes it on its own channel.
    fn record_read(
        &self,
        room: &str,
        app: &str,
        record: &str,
    ) -> tilo_action_broker::services::RecordRead {
        use tilo_action_broker::services::RecordRead;
        let Some(room) = super::super::state::RoomId::parse(room) else {
            return RecordRead::Own;
        };
        {
            let tree = self.tree.read();
            if tree.commits_native_documents(room) {
                return RecordRead::Committed;
            }
            if !tree.seals_records(room) {
                return RecordRead::Own;
            }
        }
        let route = tilo_action_broker::route::CrdtRoute::record(app, room.to_string(), record);
        if self.owner_holds(room, app, record) || self.reads_sealed_writes(room, &route) {
            RecordRead::Sealed
        } else {
            RecordRead::Own
        }
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        let owner = &self.opened_owner("native document owner unavailable")?;
        self.network
            .as_ref()
            .ok_or("native selected custody unavailable")?
            .check_native_custody(owner, room)?;
        owner.with_current(room, |view| {
            if !view.peer().certificates().contains(context.certificate()) {
                return Err("committed document caller certificate changed".into());
            }
            view.peer().check_document_read(
                context.certificate().device_public_key,
                &route.app_id,
                &route.record_id,
                std::time::SystemTime::now(),
            )
        })?;
        context.revalidate()
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        use super::super::native_workspace_owner::DocumentRead;
        self.authorize(context)?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        if self.reads_sealed_writes(room, route) {
            let read = self.capture_sealed_writes(room, route, known_revision, maximum)?;
            self.authorize(context)?;
            return Ok(read);
        }
        let owner = &self.opened_owner("native document owner unavailable")?;
        // A record the room's hosts send a device only once it asks (an
        // earlier part of a chain) is asked for the first time it is read,
        // and read once it arrived. Until then the reader waits on its read
        // (a held `Await`), which its arrival, a host connecting or leaving,
        // or a host saying it has none of it wakes.
        if on_demand(&route.record_id)
            && let Some(network) = &self.network
            && !owner.with_current(room, |view| {
                Ok(view
                    .peer()
                    .document(&route.app_id, &route.record_id)
                    .is_some())
            })?
        {
            return Ok(
                match network.read_on_demand(room, &route.app_id, &route.record_id) {
                    crate::shell::network::OnDemand::Arriving => NativeDocumentRead::Pending,
                    crate::shell::network::OnDemand::Absent => NativeDocumentRead::Absent,
                },
            );
        }
        let read = owner.with_current(room, |view| {
            view.read_document_with_history(
                context.certificate().device_public_key,
                (&route.app_id, &route.record_id),
                known_revision,
                maximum,
                std::time::SystemTime::now(),
                &mut |envelope| {
                    self.history_keys
                        .as_ref()
                        .and_then(tilo_action_broker::channel_encryption::ChannelKeyProvider::get)
                        .ok_or("committed history keys unavailable")?
                        .open_history_write(room, envelope)
                },
            )
        });
        let read = read.inspect_err(|error| self.note_unreadable(context, room, error))?;
        self.authorize(context)?;
        Ok(match read {
            DocumentRead::Unchanged(revision) => NativeDocumentRead::Unchanged(revision),
            DocumentRead::Captured(document) => NativeDocumentRead::Captured {
                revision: document.revision,
                heads: document.heads,
                body: Box::new(Captured(document.bytes)),
            },
            DocumentRead::CheckpointPending => NativeDocumentRead::Pending,
        })
    }
    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        use tilo_joined_room::ChangesRead;
        self.authorize(context)?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        if self.reads_sealed_writes(room, route) {
            return self.capture(context, Some(held.revision), maximum);
        }
        let owner = &self.opened_owner("native document owner unavailable")?;
        let read = owner.with_current(room, |view| {
            view.read_changes(
                context.certificate().device_public_key,
                &route.app_id,
                &route.record_id,
                &held.heads,
                maximum,
                std::time::SystemTime::now(),
            )
        })?;
        Ok(match read {
            ChangesRead::Unchanged(revision) => NativeDocumentRead::Unchanged(revision),
            ChangesRead::Changes(changes) => NativeDocumentRead::Changes {
                revision: changes.revision,
                heads: changes.heads,
                base: held.heads.clone(),
                body: Box::new(Captured(changes.bytes)),
            },
            // A pending checkpoint, an encrypted or incomplete document, a
            // base it does not hold: the whole read decides, as before.
            ChangesRead::Whole => return self.capture(context, Some(held.revision), maximum),
        })
    }
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        // After `Unavailable` only a capture can tell whether the
        // checkpoint's body arrived.
        let Some(known) = known_revision else {
            return Ok(None);
        };
        self.authorize(context)?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        if self.reads_sealed_writes(room, route) {
            return Ok(None);
        }
        let owner = &self.opened_owner("native document owner unavailable")?;
        owner.with_current(room, |view| {
            view.holds_revision(
                context.certificate().device_public_key,
                &route.app_id,
                &route.record_id,
                known,
                std::time::SystemTime::now(),
            )
        })
    }
    fn next_change(
        &self,
        context: &NativeDocumentReadContext,
        after: u64,
    ) -> Result<Option<u64>, String> {
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        let owner = &self.opened_owner("native document owner unavailable")?;
        let second = tilo_action_broker::policy_schedule::second_at_or_after;
        // The room's own governance (its grants' and capabilities' expiries)
        // and this host's custody certificate (`check_native_custody`).
        let committed = owner.with_current(room, |view| {
            let grants = tilo_owner_commit::PrivatePolicyBasis::from_peer(view.peer())?
                .next_expiry_after(after);
            let custody = view
                .device()
                .expires_at_ns
                .map(second)
                .filter(|at| *at > after);
            Ok(grants.into_iter().chain(custody).min())
        })?;
        // The projection a participant reads for the same room: its grants and
        // the certificates of the devices it admitted, the reader's among
        // them.
        let projected =
            super::super::channel_policy::next_policy_change(&self.tree.read(), room, after, &[]);
        Ok(committed.into_iter().chain(projected).min())
    }
}

impl NativeOwnerCommitService {
    /// Whether this device's owner holds `app`'s `record` in `room`.
    fn owner_holds(&self, room: super::super::state::RoomId, app: &str, record: &str) -> bool {
        self.opened_owner("").is_ok_and(|owner| {
            owner.with_current(room, |view| Ok(view.peer().holds_document(app, record))) == Ok(true)
        })
    }
    /// Whether this device holds any sealed write of `route`'s record.
    fn holds_sealed_writes(&self, route: &tilo_action_broker::route::CrdtRoute) -> bool {
        let artifact =
            collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        self.sealed_writes.as_ref().is_some_and(|writes| {
            writes
                .lock()
                .is_ok_and(|mut held| held.holds_sealed_writes(artifact))
        })
    }
    /// Whether a read of `route`'s record in `room` opens the sealed writes
    /// this device holds: a sealed room's record its owner does not hold.
    /// The owner keeps no replica of a sealed room's records; the room's
    /// host keeps each write once, and this device what it was given.
    ///
    /// Writes that build on changes this device does not hold make no record
    /// it can read back: an app that kept a record's first changes to itself
    /// (made before it wrote on its channel) left only its later writes on
    /// the channel. A read of those would wait for the missing changes
    /// forever, so the record is the app's own to open, from the copy it
    /// keeps, as a new one is; it then hands its channel what it holds.
    fn reads_sealed_writes(
        &self,
        room: super::super::state::RoomId,
        route: &tilo_action_broker::route::CrdtRoute,
    ) -> bool {
        if !self.tree.read().seals_records(room)
            || self.owner_holds(room, &route.app_id, &route.record_id)
            || !self.holds_sealed_writes(route)
        {
            return false;
        }
        match self.held_sealed_record(room, route, usize::MAX) {
            Ok(HeldSealed::Incomplete { missing }) => {
                tracing::warn!(
                    app = %route.app_id,
                    record = %route.record_id,
                    %room,
                    missing,
                    "the sealed writes this device holds of a record build on changes it does \
                     not hold; the app opens the record from its own copy"
                );
                false
            }
            // Waiting for the channel's keys, readable, or failing: the read
            // answers each, and says a failure to the app.
            Ok(HeldSealed::Waiting | HeldSealed::Readable(_)) | Err(_) => true,
        }
    }
    /// What the sealed writes this device holds of `route`'s record in
    /// `room` make, opened with the keys of the record's channel, within
    /// `maximum` bytes.
    fn held_sealed_record(
        &self,
        room: super::super::state::RoomId,
        route: &tilo_action_broker::route::CrdtRoute,
        maximum: usize,
    ) -> Result<HeldSealed, String> {
        use tilo_action_broker::channel_encryption::{ChannelAccess, ChannelKeyProvider};
        let artifact =
            collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let writes = self
            .sealed_writes
            .as_ref()
            .ok_or("this device keeps no sealed writes")?
            .lock()
            .map_err(|_| "sealed writes unavailable")?
            .sealed_writes(artifact);
        if writes.is_empty() {
            return Ok(HeldSealed::Waiting);
        }
        let channels = self
            .history_keys
            .as_ref()
            .and_then(ChannelKeyProvider::get)
            .ok_or("channel keys unavailable")?;
        let channel_route = collab_types::ChannelRoute::new(&route.app_id, &route.record_id);
        let channel = channel_route.channel(&route.cluster_id);
        let seen = channels.channel_change(channel);
        let keys = match channels.access_record(room, &channel_route).0 {
            ChannelAccess::Ready(keys) => keys,
            ChannelAccess::Waiting => {
                self.recheck_when_keys_change(channel, seen);
                return Ok(HeldSealed::Waiting);
            }
            refused => {
                return Err(format!(
                    "the record's channel refused its keys: {refused:?}"
                ));
            }
        };
        sealed_document(&writes, &keys, maximum)
    }
    /// `route`'s record in sealed `room`, as the sealed writes this device
    /// holds of it make it, opened with the keys of the record's channel.
    /// Asking for them asks the channel's participants, as the app's own channel
    /// does; until they are here, or while a write it builds on has not
    /// arrived yet, the read is pending and the reader asks again.
    fn capture_sealed_writes(
        &self,
        room: super::super::state::RoomId,
        route: &tilo_action_broker::route::CrdtRoute,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        let mut document = match self.held_sealed_record(room, route, maximum)? {
            HeldSealed::Readable(document) => document,
            HeldSealed::Waiting => return Ok(NativeDocumentRead::Pending),
            HeldSealed::Incomplete { missing } => {
                return Err(format!(
                    "the sealed writes this device holds of the record build on {missing} \
                     change(s) it does not hold"
                ));
            }
        };
        let revision = tilo_peer_state::committed_document_revision(&document.get_heads());
        if known_revision == Some(revision) {
            return Ok(NativeDocumentRead::Unchanged(revision));
        }
        let mut heads: Vec<[u8; 32]> = document.get_heads().iter().map(|head| head.0).collect();
        heads.sort_unstable();
        let bytes = document.save();
        if bytes.len() > maximum {
            return Err("the record exceeds the committed read's bound".into());
        }
        Ok(NativeDocumentRead::Captured {
            revision,
            heads,
            body: Box::new(Captured(bytes)),
        })
    }
    /// Have a reader waiting on a pending read of `channel`'s record check
    /// again once the channel's keys or access change from `seen`: its wait
    /// is woken by announced changes, and a participant's answer is one. One
    /// watch per channel at a time.
    fn recheck_when_keys_change(&self, channel: collab_types::ChannelId, seen: u64) {
        let (Some(provider), Some(runtime)) = (
            self.history_keys.clone(),
            self.network
                .as_ref()
                .and_then(|network| network.runtime_handle().ok()),
        ) else {
            return;
        };
        let watched = Arc::clone(&self.key_watches);
        if !watched.lock().is_ok_and(|mut held| held.insert(channel)) {
            return;
        }
        runtime.spawn(async move {
            provider.channel_changed(channel, seen).await;
            if let Ok(mut held) = watched.lock() {
                held.remove(&channel);
            }
            tilo_identity_host::policy_change::announce();
        });
    }
    /// Say why an app's first read of a record its room does not hold
    /// failed: whether its launch carried the signed original's creation
    /// evidence, and who governs the room. Only a failed read logs.
    fn note_unreadable(
        &self,
        context: &NativeDocumentReadContext,
        room: super::super::state::RoomId,
        error: &str,
    ) {
        let route = context.route();
        let Ok(owner) = self.opened_owner("") else {
            return;
        };
        let Ok((missing, host_governs)) = owner.with_current(room, |view| {
            Ok((
                view.peer()
                    .document(&route.app_id, &route.record_id)
                    .is_none(),
                view.governor().capabilities.can_act_as_host,
            ))
        }) else {
            return;
        };
        if !missing {
            return;
        }
        tracing::error!(
            app = %route.app_id,
            record = %route.record_id,
            %room,
            %error,
            creation_evidence = context
                .application_creation()
                .is_some_and(tilo_action_broker::services::ApplicationCreationEvidence::valid),
            record_kind = context.record_kind().is_some(),
            governed_elsewhere = self
                .network
                .as_ref()
                .is_some_and(|network| network.governed_elsewhere(room)),
            host_governs,
            "committed read of a record the room does not hold"
        );
    }
}

/// How long an on-demand record the room's hosts may keep may take to
/// reach this device once asked for, before this device creates it.
const ON_DEMAND_RECORD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The workspace a record created in `room` names to the app initializing
/// it: the room's id and its workspace's name.
fn creation_workspace(
    tree: &super::super::state::WorkspaceTreeHandle,
    room: super::super::state::RoomId,
) -> runtime_collab::committed_document::CreationWorkspace {
    let name = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.name.clone())
        .filter(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        .unwrap_or_else(|| room.to_string());
    runtime_collab::committed_document::CreationWorkspace {
        id: room.to_string(),
        name,
    }
}

/// [`prepare_here`] as the broker awaits it: logged, and answered as the
/// workspace a created record names.
fn preparing_here(
    network: Arc<super::super::network::ShellNetwork>,
    owner: super::super::native_workspace_owner::NativeWorkspaceOwner,
    (context, tree): (
        NativeDocumentReadContext,
        super::super::state::WorkspaceTreeHandle,
    ),
    room: super::super::state::RoomId,
) -> tilo_action_broker::services::PreparingElsewhere {
    Box::pin(async move {
        let prepared = prepare_here(&network, &owner, &context, room).await;
        let route = context.route();
        match &prepared {
            Ok(created) => tracing::info!(
                app = %route.app_id, record = %route.record_id, %room, created,
                "the room holds the app's record"
            ),
            Err(error) => tracing::error!(
                app = %route.app_id, record = %route.record_id, %room, %error,
                "this device did not create the app's record"
            ),
        }
        prepared.map(|created| created.then(|| creation_workspace(&tree, room)))
    })
}

/// In a room another device governs, create the channel's missing
/// record empty, signed by this device, as every receiver admits a member's
/// record: the room lets its app author records and this person change the
/// record. The same original launch evidence and declared kind a local
/// creation requires are checked first. An on-demand record the room's
/// hosts may keep already is asked for first, and taken when it comes.
/// Answers whether this device created the record, which it then
/// initializes as its own change.
async fn prepare_here(
    network: &Arc<super::super::network::ShellNetwork>,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    context: &NativeDocumentReadContext,
    room: super::super::state::RoomId,
) -> Result<bool, String> {
    context.revalidate()?;
    let route = context.route();
    let holds = || {
        owner.with_current(room, |view| {
            Ok(view
                .peer()
                .document(&route.app_id, &route.record_id)
                .is_some())
        })
    };
    if holds()? {
        return Ok(false);
    }
    let evidence = context
        .application_creation()
        .filter(|evidence| evidence.valid())
        .ok_or("application creation requires original signed launch declarations")?;
    let record = tilo_joined_room::ApplicationRecord::declared(
        &route.app_id,
        &route.record_id,
        context
            .record_kind()
            .ok_or("application creation requires SDK resource kind")?,
        &evidence.declared_kinds,
    )?;
    network.check_native_custody(owner, room)?;
    if on_demand(&route.record_id)
        && network.read_on_demand(room, &route.app_id, &route.record_id)
            == crate::shell::network::OnDemand::Arriving
    {
        let arrived = Arc::new(tokio::sync::Notify::new());
        let wake = Arc::clone(&arrived);
        let watch = tilo_identity_host::policy_change::Watch::new(move || wake.notify_one());
        let came = tokio::time::timeout(ON_DEMAND_RECORD_TIMEOUT, async {
            loop {
                watch.arm();
                if holds()? {
                    return Ok::<(), String>(());
                }
                arrived.notified().await;
            }
        })
        .await;
        if let Ok(came) = came {
            came?;
            context.revalidate()?;
            return Ok(false);
        }
    }
    let creator = context.certificate().clone();
    let (network, owner) = (Arc::clone(network), owner.clone());
    let created = tokio::task::spawn_blocking(move || {
        let key = network.native_signing_key_of(&owner, room)?;
        let created = owner.prepare_application_record(
            room,
            &creator,
            &key,
            &record,
            std::time::SystemTime::now(),
        )?;
        network.schedule_native_owner_publication(room);
        Ok::<_, String>(created)
    })
    .await
    .map_err(|_| "creating the app's record stopped")??;
    context.revalidate()?;
    Ok(created)
}

/// The document `writes` make, each opened with `keys`: a whole frame, or
/// a mutation reassembled from its segments. `None` while a write builds on
/// one that has not arrived. At most `maximum` bytes are opened.
/// What the sealed writes of one record this device holds make.
enum HeldSealed {
    /// Nothing to read yet: no write, or the channel's keys are on their way.
    Waiting,
    /// The record, whole.
    Readable(Box<automerge::AutoCommit>),
    /// The writes build on `missing` changes, or segments of one, this
    /// device does not hold.
    Incomplete { missing: usize },
}

fn sealed_document(
    writes: &[collab_types::SignedOp],
    keys: &channel_crypto::ChannelContentKeys,
    maximum: usize,
) -> Result<HeldSealed, String> {
    use collab_types::{DocumentFrame, DocumentMutation};
    let mut remaining = maximum;
    let mut reassembler = collab_types::DocumentReassembler::default();
    let mut document = automerge::AutoCommit::new();
    for write in writes {
        let metadata = write.encryption.as_ref().ok_or("the write is not sealed")?;
        let plaintext = keys
            .open_content(write.artifact, metadata, &write.payload)
            .map_err(|error| format!("a sealed write did not open: {error}"))?;
        remaining = remaining
            .checked_sub(plaintext.len())
            .ok_or("the record exceeds the committed read's bound")?;
        let mutation = match DocumentFrame::decode(&plaintext).map_err(|error| error.to_string())? {
            DocumentFrame::Whole(mutation) => mutation,
            DocumentFrame::Segment(segment) => {
                match reassembler
                    .accept(write, segment, std::time::Instant::now())
                    .map_err(|error| error.to_string())?
                {
                    Some(whole) => DocumentMutation::decode(&whole.payload)
                        .map_err(|error| error.to_string())?,
                    None => continue,
                }
            }
        };
        let bytes = match &mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        // Automerge holds a change whose dependencies are missing until they
        // arrive, whatever order the writes were admitted in.
        tilo_automerge::ApplyIncoming::load_incoming(&mut document, bytes)
            .map_err(|error| error.to_string())?;
    }
    let missing = reassembler
        .pending_transfers()
        .saturating_add(automerge::ReadDoc::get_missing_deps(&document, &[]).len());
    if missing > 0 {
        return Ok(HeldSealed::Incomplete { missing });
    }
    Ok(HeldSealed::Readable(Box::new(document)))
}

/// Whether `record` reaches a device only once it asks for it
/// (`neighbour_wire::record_sync`): an on-demand record, or a part of a
/// chain, whose earlier parts the room's hosts keep for who reads back.
fn on_demand(record: &str) -> bool {
    neighbour_wire::record_sync(record) != neighbour_wire::RecordSync::Eager
}

/// The bytes of one bounded capture, held until the broker has sent them.
struct Captured(Vec<u8>);

impl tilo_action_broker::services::DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod sealed_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "a test: a failure is a panic"
    )]
    use super::{HeldSealed, sealed_document};
    use automerge::transaction::Transactable as _;
    use collab_types::{
        ArtifactId, AttestationId, ContentRetention, DocumentMutation, OpKind, SignedOp,
        generate_keypair,
    };

    /// A sealed write of `changes`, as a record's channel keeps one.
    fn sealed(
        keys: &channel_crypto::ChannelContentKeys,
        artifact: ArtifactId,
        seq: u64,
        changes: Vec<u8>,
    ) -> SignedOp {
        let plaintext = DocumentMutation::Incremental(changes)
            .encode_frames()
            .unwrap()
            .remove(0);
        let (metadata, ciphertext) = keys
            .seal_content(artifact, ContentRetention::Delta, &plaintext)
            .unwrap();
        let (_, author) = generate_keypair();
        SignedOp::new_unsigned_with_encryption(
            ciphertext,
            artifact,
            author,
            AttestationId::new(),
            OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            seq,
            metadata,
        )
    }

    #[test]
    fn writes_that_build_on_a_change_the_device_lacks_are_incomplete_not_pending() {
        let group = channel_crypto::ChannelGroup::create(
            collab_types::ChannelId::new(),
            channel_crypto::MemberKey::generate("ada").unwrap(),
        )
        .unwrap();
        let keys = group.content_keys().unwrap();
        let artifact = ArtifactId::new();
        // The record's making, kept to its app, then a change on its channel.
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "schema", 1_u64).unwrap();
        let made = doc.save_incremental();
        doc.put(automerge::ROOT, "value", 1_u64).unwrap();
        let changed = doc.save_incremental();

        let only_later = [sealed(&keys, artifact, 1, changed.clone())];
        assert!(matches!(
            sealed_document(&only_later, &keys, usize::MAX).unwrap(),
            HeldSealed::Incomplete { missing: 1 }
        ));
        let both = [
            sealed(&keys, artifact, 1, made),
            sealed(&keys, artifact, 2, changed),
        ];
        let HeldSealed::Readable(mut read) = sealed_document(&both, &keys, usize::MAX).unwrap()
        else {
            panic!("the whole history reads back");
        };
        assert_eq!(read.get_heads(), doc.get_heads());
    }
}
