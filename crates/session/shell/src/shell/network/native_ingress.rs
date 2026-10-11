use super::{
    InboundChannelGovernance, NeighbourMsg, NetCmd, RoomId, ShellNetwork, SystemTime, room_topic_id,
};
use crate::{network::actor::native_ingress, shell::native_workspace_owner::NativeWorkspaceOwner};
impl ShellNetwork {
    /// Existing enrolled governor and current selected native reporter custody.
    /// No identity selection, enrollment or grant is performed here.
    pub(crate) fn feedback_custody(
        &self,
        room: RoomId,
    ) -> Result<
        (
            tilo_user_identity::DeviceCert,
            tilo_user_identity::DeviceCert,
        ),
        String,
    > {
        let owner = self.native_owner()?;
        self.check_native_custody(&owner, room)?;
        owner.with_current(room, |view| {
            view.peer()
                .permissions(view.governor().device_public_key, SystemTime::now())?;
            Ok((view.device().clone(), view.governor().clone()))
        })
    }
    pub(crate) fn prepare_native_application(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        creator: &tilo_user_identity::DeviceCert,
        route: &tilo_joined_room::ApplicationRecord,
    ) -> Result<bool, String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native custody unavailable")?;
        let (certificate, _) = owner.with_current(room, |view| {
            identity.current_native_material(view.workspace())
        })?;
        if &certificate != creator {
            return Err("application creator differs from selected native custody".into());
        }
        owner.prepare_application_record(
            room,
            &certificate,
            &identity.device_signing_key,
            route,
            SystemTime::now(),
        )
    }

    pub(crate) fn initialize_native_application(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        creator: &tilo_user_identity::DeviceCert,
        initialization: tilo_joined_room::ApplicationInitialization<'_>,
    ) -> Result<[u8; 32], String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native custody unavailable")?;
        let (certificate, _) = owner.with_current(room, |view| {
            identity.current_native_material(view.workspace())
        })?;
        if &certificate != creator {
            return Err("application initialization creator custody changed".into());
        }
        owner.initialize_application_record(
            room,
            &certificate,
            &identity.device_signing_key,
            initialization,
            SystemTime::now(),
        )
    }

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
    /// Take what the room's governor never admits into its replica: channel
    /// governance, and in a room whose records are sealed channels, a
    /// member's sealed write, which only the record's channel members open.
    /// Returns whether `input` was one of them.
    fn dispatch_channel_traffic(&self, input: &native_ingress::Input) -> bool {
        let room = input.room();
        match input.message() {
            NeighbourMsg::MlsGovernance {
                room: named, frame, ..
            } => {
                if RoomId::parse(named) == Some(room) {
                    self.queue_channel_governance(InboundChannelGovernance::Frame {
                        room,
                        frame: frame.clone(),
                    });
                }
                true
            }
            NeighbourMsg::MlsAccessRequest {
                room: named,
                request,
                ..
            } => {
                if RoomId::parse(named) == Some(room) {
                    self.queue_channel_governance(InboundChannelGovernance::AccessRequest {
                        room,
                        request: request.clone(),
                    });
                }
                true
            }
            NeighbourMsg::MlsGovernanceAck {
                room: named, ack, ..
            } => {
                if RoomId::parse(named) == Some(room) {
                    self.queue_channel_governance(InboundChannelGovernance::Ack {
                        room,
                        ack: ack.clone(),
                    });
                }
                true
            }
            NeighbourMsg::AppDocument {
                peer,
                app_id,
                cluster_id,
                record_id,
                envelope,
            } if envelope.encryption.is_some() && self.native_room_seals_records(room) => {
                if *peer == self.local_peer_hash || cluster_id != &room.to_string() {
                    return true;
                }
                // The relay authenticated the author; the room's governance
                // says what it may write. The broker checks the signature,
                // the role and the channel's gate before any app sees it.
                match input.standing() {
                    Ok(author) => self.handle_document_message(
                        *peer,
                        app_id.clone(),
                        cluster_id.clone(),
                        record_id.clone(),
                        author,
                        input.retained(),
                        (**envelope).clone(),
                    ),
                    Err(_) => {
                        tracing::debug!(room = %room, "sealed write from a device without standing");
                    }
                }
                true
            }
            _ => false,
        }
    }
    /// Whether `msg` is a sealed record write to an enrolled room whose
    /// records are sealed channels: the governor never commits it, so it
    /// goes to the room's relay as written.
    pub(super) fn sealed_record_write(&self, msg: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            cluster_id,
            envelope,
            ..
        } = msg
        else {
            return false;
        };
        envelope.encryption.is_some()
            && RoomId::parse(cluster_id).is_some_and(|room| self.native_room_seals_records(room))
    }
    /// The identity the governance of the enrolled room on topic `ws`
    /// admitted for `device`, a member's device the room's relay
    /// authenticated: a browser of the room announces no identity of its
    /// own to this device.
    pub(super) fn governed_identity(
        &self,
        ws: u64,
        device: iroh::EndpointId,
    ) -> Option<crate::network::identity::PeerIdentity> {
        let room = self.room_for_topic(ws)?;
        let owner = self.native_owner().ok()?;
        owner
            .with_current(room, |view| {
                let now = SystemTime::now();
                let cert = view
                    .peer()
                    .certificates()
                    .into_iter()
                    .find(|cert| cert.device_public_key == *device.as_bytes())
                    .ok_or("not a member device")?;
                view.peer().permissions(cert.device_public_key, now)?;
                Ok(crate::network::identity::PeerIdentity {
                    device_pubkey: cert.device_public_key,
                    user_pubkey: cert.user_public_key,
                    capabilities: cert.capabilities.clone(),
                    device_cert: cert,
                })
            })
            .ok()
    }

    /// Native presence uses admitted governance, never the legacy hello cache.
    pub(super) fn governed_presence_allows(
        &self,
        ws: u64,
        device: iroh::EndpointId,
        app: &str,
        cluster: &str,
    ) -> bool {
        let Some(room) = self.unique_room_for_topic(ws) else {
            return false;
        };
        if room.to_string() != cluster || app.is_empty() {
            return false;
        }
        let Ok(owner) = self.native_owner() else {
            return false;
        };
        owner
            .with_current(room, |view| {
                view.peer()
                    .check_document_read(*device.as_bytes(), app, "", SystemTime::now())
            })
            .is_ok()
    }
    /// Whether `room` is an enrolled room whose records are sealed
    /// channels its members write themselves (its governor stewards them).
    pub(super) fn native_room_seals_records(&self, room: RoomId) -> bool {
        self.replica.lock().is_ok_and(|replica| {
            let tree = replica.tree.read();
            tree.requires_native_owner(room) && !tree.commits_native_documents(room)
        })
    }
    pub(super) fn dispatch_native_input(&self, input: &native_ingress::Input) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        if self.dispatch_channel_traffic(input) {
            return;
        }
        let generation = input.generation().ok();
        let report = match input.admit(identity) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error,room=%input.room(),"native relay admission denied");
                return;
            }
        };
        // A genuine committed change can enable successors or retire custody.
        // An unchanged replay must not enqueue another expensive recovery pass.
        if generation != input.generation().ok() {
            let _ = self.cmd_tx.try_send(NetCmd::NativePublish {
                ws: room_topic_id(input.room()),
            });
        }
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
            let Ok(author) = input.standing() else {
                return;
            };
            self.handle_document_message(
                *peer,
                app_id.clone(),
                cluster_id.clone(),
                record_id.clone(),
                author,
                input.retained(),
                (**envelope).clone(),
            );
        }
    }
}
