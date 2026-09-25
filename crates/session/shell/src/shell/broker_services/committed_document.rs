//! Reads the same native owner; local app storage is never an authority fallback.
use super::*;
impl NativeDocumentReads for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        super::super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().requires_native_owner(room))
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        let room = super::super::state::RoomId::parse(&route.cluster_id)
            .ok_or("invalid committed document room")?;
        let owner = self
            .owner
            .as_ref()
            .ok_or("native document owner unavailable")?;
        let network = self
            .network
            .as_ref()
            .ok_or("native selected custody unavailable")?;
        network.check_native_custody(owner, room)?;
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
        _known_revision: Option<[u8; 32]>,
        _maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.authorize(context)?;
        // This seam deliberately cannot call StatePeer::document (clone/save).
        // Installation awaits the actual owner's BoundedStatePeer + same-domain
        // NativeWorkTurn capture. A nominal broker byte token is not a substitute.
        Err("bounded native document capture is not installed".into())
    }
}
