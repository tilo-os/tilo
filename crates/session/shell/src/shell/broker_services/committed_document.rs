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
        })? {
            context.revalidate()?;
            return Ok(None);
        }
        // A missing record is made by the host that governs the room
        // ([`Self::prepare_application_elsewhere`]). This device never signs
        // a record's genesis into a room document itself: a room hosted on
        // a device, this one included, keeps its records in sealed
        // channels each app opens itself.
        Err("this room's records are sealed channels its apps open themselves".into())
    }
    fn prepare_application_elsewhere(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Option<tilo_action_broker::services::PreparingElsewhere> {
        let room = super::super::state::RoomId::parse(&context.route().cluster_id)?;
        if !self.host_governs(room) {
            return None;
        }
        Some(preparing_at_host(
            Arc::clone(self.network.as_ref()?),
            self.opened_owner("").ok()?,
            (context.clone(), self.tree.clone()),
            room,
        ))
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
        // Every held room's app records are native documents, whether this
        // device commits them or the room's host does: a sealed room's records
        // are the ones a cold app has to read through the host.
        super::super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().holds_room(room))
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
        let owner = &self.opened_owner("native document owner unavailable")?;
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
        // The projection a steward reads for the same room: its grants and
        // the certificates of the devices it admitted, the reader's among
        // them.
        let projected =
            super::super::channel_policy::next_policy_change(&self.tree.read(), room, after, &[]);
        Ok(committed.into_iter().chain(projected).min())
    }
}

impl NativeOwnerCommitService {
    /// Whether another device governs `room` and it is a host (a home
    /// relay), which creates the room's records on a member's request.
    fn host_governs(&self, room: super::super::state::RoomId) -> bool {
        self.network.as_ref().is_some_and(|network| {
            self.opened_owner("")
                .is_ok_and(|owner| network.host_creates_records(&owner, room))
        })
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

/// How long the record a host created may take to reach this device's
/// admitted room. With the host's own answer (at most the room-admin
/// timeout) it stays inside what a launching app waits for its first read.
const HOST_RECORD_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

/// [`prepare_at_host`] as the broker awaits it: logged, and answered as the
/// workspace a created record names.
fn preparing_at_host(
    network: Arc<super::super::network::ShellNetwork>,
    owner: super::super::native_workspace_owner::NativeWorkspaceOwner,
    (context, tree): (
        NativeDocumentReadContext,
        super::super::state::WorkspaceTreeHandle,
    ),
    room: super::super::state::RoomId,
) -> tilo_action_broker::services::PreparingElsewhere {
    Box::pin(async move {
        let prepared = prepare_at_host(&network, &owner, &context, room).await;
        let route = context.route();
        match &prepared {
            Ok(created) => tracing::info!(
                app = %route.app_id, record = %route.record_id, %room, created,
                "the room's host holds the app's record"
            ),
            Err(error) => tracing::error!(
                app = %route.app_id, record = %route.record_id, %room, %error,
                "the room's host did not prepare the app's record"
            ),
        }
        prepared.map(|created| created.then(|| creation_workspace(&tree, room)))
    })
}

/// In a room a host governs (a home relay), have the host create the
/// channel's missing record, empty and signed by the host, then wait until
/// it is in this device's admitted room. The same original launch evidence
/// and declared kind a local creation requires are checked first; the host
/// checks the caller's membership and the app's authorship again. Answers
/// whether the host created the record for this caller, which then
/// initializes it as its own change.
async fn prepare_at_host(
    network: &super::super::network::ShellNetwork,
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
    tilo_joined_room::ApplicationRecord::declared(
        &route.app_id,
        &route.record_id,
        context
            .record_kind()
            .ok_or("application creation requires SDK resource kind")?,
        &evidence.declared_kinds,
    )?;
    network.check_native_custody(owner, room)?;
    let created = network
        .prepare_application_record_at_host(
            room,
            context.certificate(),
            &route.app_id,
            &route.record_id,
        )
        .await
        .ok_or("no host governs the room")??;
    // The host's answer is not this device's barrier: wait until the record
    // it published is in this device's admitted room.
    tokio::time::timeout(HOST_RECORD_ADMISSION_TIMEOUT, async {
        loop {
            network.check_native_custody(owner, room)?;
            if holds()? {
                return Ok::<(), String>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| "the host created the app's record but it did not reach this device in time")??;
    context.revalidate()?;
    Ok(created)
}

/// The bytes of one bounded capture, held until the broker has sent them.
struct Captured(Vec<u8>);

impl tilo_action_broker::services::DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}
