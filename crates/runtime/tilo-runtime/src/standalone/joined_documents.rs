//! SDK reads use the same enrolled owner and selected relay custody.
use super::{Connect, Joined, RoomId, SystemTime};
use tilo_action_broker::services::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};
impl NativeDocumentReads for Joined {
    fn requires_room(&self, room: &str) -> bool {
        RoomId::parse(room).is_some_and(|room| self.owner.contains_room(room))
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        let room = RoomId::parse(&route.cluster_id).ok_or("invalid committed document room")?;
        let (_, cert, _, continuity) =
            Connect::new(self.owner.clone(), room)?.material(&self.custody)?;
        if context.certificate() != &cert {
            return Err("committed document caller certificate changed".into());
        }
        self.owner.with_current(room, |view| {
            if view.continuity() != continuity || view.device() != &cert {
                return Err("committed document selected custody changed".into());
            }
            view.peer().check_document_read(
                cert.device_public_key,
                &route.app_id,
                &route.record_id,
                SystemTime::now(),
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
        self.authorize(context)?;
        let route = context.route();
        let room = RoomId::parse(&route.cluster_id).ok_or("invalid committed document room")?;
        let read = self.owner.with_current(room, |view| {
            view.read_document_with_history(
                context.certificate().device_public_key,
                (&route.app_id, &route.record_id),
                known_revision,
                maximum,
                SystemTime::now(),
                &mut |envelope| {
                    self.keys
                        .get()
                        .ok_or("committed history keys unavailable")?
                        .open_history_write(room, envelope)
                },
            )
        })?;
        self.authorize(context)?;
        Ok(super::super::committed_documents::native_read(read))
    }
}
