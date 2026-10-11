//! The same durable owners serve commits and bounded SDK reads.
use super::{Joined, RoomOwner};
use std::sync::Arc;
use tilo_action_broker::services::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};
use tilo_peer_state::owner_commit::OwnerCommitService;

pub(super) struct RoomOwnerServices {
    pub(super) commits: Arc<dyn OwnerCommitService>,
    pub(super) documents: Option<Arc<RoomOwner>>,
}

pub(super) struct Documents {
    pub(super) own: Option<Arc<RoomOwner>>,
    pub(super) joined: Arc<Joined>,
}
impl Documents {
    fn for_room(&self, room: &str) -> Result<&dyn NativeDocumentReads, String> {
        if let Some(own) = &self.own
            && NativeDocumentReads::requires_room(own.as_ref(), room)
        {
            return Ok(own.as_ref());
        }
        if NativeDocumentReads::requires_room(self.joined.as_ref(), room) {
            return Ok(self.joined.as_ref());
        }
        Err("this runtime does not own that committed document room".into())
    }
}
impl NativeDocumentReads for Documents {
    fn requires_room(&self, room: &str) -> bool {
        self.for_room(room).is_ok()
    }
    fn prepare_application(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Result<Option<runtime_collab::committed_document::CreationWorkspace>, String> {
        self.for_room(&context.route().cluster_id)?
            .prepare_application(context)
    }
    fn initialize_application(
        &self,
        context: &NativeDocumentReadContext,
        revision: [u8; 32],
        kind: &str,
        document: &[u8],
    ) -> Result<[u8; 32], String> {
        self.for_room(&context.route().cluster_id)?
            .initialize_application(context, revision, kind, document)
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        self.for_room(&context.route().cluster_id)?
            .authorize(context)
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.for_room(&context.route().cluster_id)?
            .capture(context, known_revision, maximum)
    }
}

pub(super) fn native_read(read: tilo_joined_room::DocumentRead) -> NativeDocumentRead {
    match read {
        tilo_joined_room::DocumentRead::Unchanged(revision) => {
            NativeDocumentRead::Unchanged(revision)
        }
        tilo_joined_room::DocumentRead::Captured(document) => NativeDocumentRead::Captured {
            revision: document.revision,
            body: Box::new(Captured(document.bytes)),
        },
    }
}
struct Captured(Vec<u8>);
impl tilo_action_broker::services::DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "committed_document_tests.rs"]
mod tests;

pub(super) fn host_service(
    owner: Option<Arc<RoomOwner>>,
    joined: &Arc<Joined>,
) -> Option<Arc<dyn NativeDocumentReads>> {
    cfg!(target_os = "linux").then(|| {
        Arc::new(Documents {
            own: owner,
            joined: Arc::clone(joined),
        }) as Arc<dyn NativeDocumentReads>
    })
}

impl RoomOwnerServices {
    pub(super) fn publisher(
        &self,
        joined: &Arc<Joined>,
    ) -> Arc<tilo_action_broker::collab_outbound::PeerPublisher> {
        tilo_action_broker::collab_outbound::PeerPublisher::with_optional_owner_commit(
            None,
            Arc::new(super::Owners {
                own: Arc::clone(&self.commits),
                joined: Arc::clone(joined),
            }),
        )
    }
}
