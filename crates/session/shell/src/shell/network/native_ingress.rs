use super::*;
use crate::{network::actor::native_ingress, shell::native_workspace_owner::NativeWorkspaceOwner};
impl ShellNetwork {
    pub(crate) fn check_native_custody(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
    ) -> Result<(), String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native custody unavailable")?;
        owner.with_current(room, |view| {
            let (certificate, _) = identity.current_native_material(view.workspace())?;
            if &certificate != view.device() {
                return Err("selected native custody differs from committed enrollment".into());
            }
            view.peer()
                .permissions(certificate.device_public_key, SystemTime::now())?;
            if !view.peer().certificates().contains(&certificate) {
                return Err("selected certificate differs from current admitted roster".into());
            }
            Ok(())
        })
    }

    pub(crate) fn connect_native_room(
        &self,
        owner: NativeWorkspaceOwner,
        room: RoomId,
    ) -> Result<(), String> {
        if !self.join_topic(room_topic_id(room)) {
            return Err("native topic queue unavailable".into());
        }
        let connect = native_ingress::Connect::new(owner, room)?;
        self.cmd_tx
            .try_send(NetCmd::NativeConnect(connect))
            .map_err(|_| "native relay queue unavailable".into())
    }
    pub(super) fn native_document_committed(&self, msg: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = msg
        else {
            return false;
        };
        let Some(room) = RoomId::parse(cluster_id) else {
            return false;
        };
        let Ok(owner) = self.native_owner() else {
            return false;
        };
        if self.check_native_custody(&owner, room).is_err() {
            return false;
        }
        owner
            .with_current(room, |view| {
                view.peer()
                    .lookup_exact_local_document(app_id, record_id, envelope, SystemTime::now())
                    .map(|admitted| admitted.is_some())
                    .map_err(|e| e.to_string())
            })
            .unwrap_or(false)
    }
    pub(super) fn native_topic_required(&self, ws: u64) -> bool {
        let Ok(replica) = self.replica.lock() else {
            return true;
        };
        let tree = replica.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .any(|slot| room_topic_id(slot.room) == ws && tree.requires_native_owner(slot.room))
    }
    pub(super) fn dispatch_native_input(&self, input: &native_ingress::Input) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        let report = match input.admit(identity) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error,room=%input.room(),"native relay admission denied");
                return;
            }
        };
        // Actual governor admission can enable successors or retire custody.
        let _ = self.cmd_tx.try_send(NetCmd::NativePublish {
            ws: room_topic_id(input.room()),
        });
        // The durable owner already projected governed shell state. Only an exact
        // admitted app operation may now reach the existing broker fanout.
        if !matches!(
            report.primary(),
            tilo_peer_state::InputDisposition::Accepted(_)
                | tilo_peer_state::InputDisposition::Duplicate(_)
        ) {
            return;
        }
        if let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = input.message()
        {
            if *peer == self.local_peer_hash || cluster_id != &input.room().to_string() {
                return;
            }
            let Ok(role) = input.role() else { return };
            self.handle_document_message(
                *peer,
                app_id.clone(),
                cluster_id.clone(),
                record_id.clone(),
                role,
                input.retained(),
                (**envelope).clone(),
            );
        }
    }
}
