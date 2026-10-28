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
        // A room's governor seals a record's genesis when the app is first
        // opened there; this device contributes its own first change to it,
        // never a record created in the clear on anyone's behalf.
        context.revalidate()?;
        Ok(Some(creation_workspace(&self.tree, room)))
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
