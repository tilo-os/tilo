//! Original-launch creation and bounded captures of this profile's admitted room.
use super::{Committed, RoomOwner, SystemTime};
use tilo_action_broker::services::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};
use tilo_joined_room::{CommittedDocument, DocumentRead};
use tilo_peer_state::{BoundedStatePeer, PeerLimits};
use tilo_user_identity::{DeviceCert, RevocationList};

impl RoomOwner {
    fn check_custody(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        if context.route().cluster_id != self.room
            || context.certificate() != &self.governor
            || self.key.verifying_key().to_bytes() != self.governor.device_public_key
        {
            return Err("committed document lacks this runtime's governor custody".into());
        }
        let current = tilo_identity_host::broker::trusted_current_device_public(&self.room)
            .map_err(|error| format!("runtime selected custody unavailable: {error}"))?;
        if current.user_root_public_key != self.governor.user_public_key
            || current.device_public_key != self.governor.device_public_key
        {
            return Err("runtime selected document identity changed".into());
        }
        context.revalidate()
    }

    fn declared(&self, context: &NativeDocumentReadContext, kind: &str) -> Result<(), String> {
        if context.record_kind() != Some(kind) {
            return Err("application initialization kind changed".into());
        }
        if context.development_source().is_some()
            && self.creation.dev
            && (self.creation.declared_kinds)(&context.route().app_id).is_none()
        {
            tilo_joined_room::ApplicationRecord::declared(
                &context.route().app_id,
                &context.route().record_id,
                kind,
                &[kind.to_owned()],
            )?;
            return Ok(());
        }
        let evidence = context
            .application_creation()
            .filter(|evidence| evidence.valid())
            .ok_or("application creation requires original signed launch declarations")?;
        tilo_joined_room::ApplicationRecord::declared(
            &context.route().app_id,
            &context.route().record_id,
            kind,
            &evidence.declared_kinds,
        )?;
        Ok(())
    }

    fn publish_candidate(
        &self,
        committed: &mut Committed,
        candidate: tilo_peer_state::StatePeer,
    ) -> Result<(), String> {
        let next = Committed {
            durable: candidate.durable()?,
            peer: candidate,
            generation: committed
                .generation
                .checked_add(1)
                .ok_or("runtime room generation exhausted")?,
        };
        super::save(&self.path, &self.governor, &next).map_err(|error| error.to_string())?;
        *committed = next;
        Ok(())
    }

    pub(super) fn capture_record(
        &self,
        durable: &[u8],
        reader: [u8; 32],
        route: (&str, &str),
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<DocumentRead, String> {
        let now = SystemTime::now();
        let peer = BoundedStatePeer::restore(
            durable,
            &self.room,
            &self.governor,
            std::slice::from_ref(&self.governor),
            &RevocationList::new(),
            now,
            &PeerLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        let document = peer
            .read()
            .capture_document(reader, route, now, maximum, &mut |envelope| {
                self.keys
                    .get()
                    .ok_or("committed history keys unavailable")?
                    .open_history_write(
                        tilo_desktop_model::RoomId::parse(&self.room)
                            .ok_or("invalid runtime room")?,
                        envelope,
                    )
            })
            .map_err(|error| error.to_string())?;
        let revision = document.revision();
        if known_revision == Some(revision) {
            return Ok(DocumentRead::Unchanged(revision));
        }
        Ok(DocumentRead::Captured(CommittedDocument {
            revision,
            bytes: document.into_bytes(),
        }))
    }

    pub(super) fn prepare_record(
        &self,
        creator: &DeviceCert,
        route: (&str, &str),
    ) -> Result<bool, String> {
        if creator != &self.governor
            || self.key.verifying_key().to_bytes() != creator.device_public_key
        {
            return Err("application creator lacks current governor custody".into());
        }
        let mut committed = self
            .committed
            .lock()
            .map_err(|_| "runtime room owner lock poisoned")?;
        let now = SystemTime::now();
        committed.peer.permissions(creator.device_public_key, now)?;
        if committed.peer.document(route.0, route.1).is_some() {
            return Ok(false);
        }
        let mut candidate = committed.peer.clone();
        candidate.publish_document(
            route.0,
            route.1,
            automerge::AutoCommit::new().save(),
            &self.key,
            now,
        )?;
        self.publish_candidate(&mut committed, candidate)?;
        drop(committed);
        Ok(true)
    }

    pub(super) fn initialize_record(
        &self,
        creator: &DeviceCert,
        route: (&str, &str),
        revision: [u8; 32],
        document: &[u8],
    ) -> Result<[u8; 32], String> {
        if creator != &self.governor
            || self.key.verifying_key().to_bytes() != creator.device_public_key
        {
            return Err("application initialization lacks current governor custody".into());
        }
        if document.is_empty() || document.len() > 64 * 1024 {
            return Err("application initialization exceeds bootstrap limit".into());
        }
        let mut committed = self
            .committed
            .lock()
            .map_err(|_| "runtime room owner lock poisoned")?;
        if !matches!(
            self.capture_record(
                &committed.durable,
                creator.device_public_key,
                route,
                Some(revision),
                64 * 1024
            )?,
            DocumentRead::Unchanged(_)
        ) {
            return Err("application initialization revision changed".into());
        }
        let mut document =
            automerge::AutoCommit::load(document).map_err(|error| error.to_string())?;
        let mut candidate = committed.peer.clone();
        candidate.publish_document(
            route.0,
            route.1,
            document.save(),
            &self.key,
            SystemTime::now(),
        )?;
        // Validate and obtain the next exact revision before making it durable.
        let capture = self.capture_record(
            &candidate.durable()?,
            creator.device_public_key,
            route,
            None,
            64 * 1024,
        )?;
        let DocumentRead::Captured(capture) = capture else {
            return Err("initialized application capture unavailable".into());
        };
        self.publish_candidate(&mut committed, candidate)?;
        drop(committed);
        Ok(capture.revision)
    }
}
impl NativeDocumentReads for RoomOwner {
    fn requires_room(&self, room: &str) -> bool {
        room == self.room
    }
    fn prepare_application(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Result<Option<runtime_collab::committed_document::CreationWorkspace>, String> {
        self.check_custody(context)?;
        let route = context.route();
        // Existing records need no new declaration or creation receipt.
        if self
            .committed
            .lock()
            .map_err(|_| "runtime room owner lock poisoned")?
            .peer
            .document(&route.app_id, &route.record_id)
            .is_some()
        {
            return Ok(None);
        }
        let kind = context
            .record_kind()
            .ok_or("application creation requires SDK resource kind")?;
        self.declared(context, kind)?;
        let created =
            self.prepare_record(context.certificate(), (&route.app_id, &route.record_id))?;
        self.check_custody(context)?;
        let name = self.workspace.name();
        let name = if !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control) {
            name
        } else {
            self.room.clone()
        };
        Ok(
            created.then(|| runtime_collab::committed_document::CreationWorkspace {
                id: self.room.clone(),
                name,
            }),
        )
    }
    fn initialize_application(
        &self,
        context: &NativeDocumentReadContext,
        revision: [u8; 32],
        kind: &str,
        document: &[u8],
    ) -> Result<[u8; 32], String> {
        self.authorize(context)?;
        self.declared(context, kind)?;
        let route = context.route();
        let revision = self.initialize_record(
            context.certificate(),
            (&route.app_id, &route.record_id),
            revision,
            document,
        )?;
        self.authorize(context)?;
        Ok(revision)
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        self.check_custody(context)?;
        let route = context.route();
        self.committed
            .lock()
            .map_err(|_| "runtime room owner lock poisoned")?
            .peer
            .check_document_read(
                context.certificate().device_public_key,
                &route.app_id,
                &route.record_id,
                SystemTime::now(),
            )?;
        self.check_custody(context)
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.authorize(context)?;
        let route = context.route();
        let read = self.capture_record(
            &self
                .committed
                .lock()
                .map_err(|_| "runtime room owner lock poisoned")?
                .durable,
            context.certificate().device_public_key,
            (&route.app_id, &route.record_id),
            known_revision,
            maximum,
        )?;
        self.authorize(context)?;
        Ok(super::super::committed_documents::native_read(read))
    }
}
