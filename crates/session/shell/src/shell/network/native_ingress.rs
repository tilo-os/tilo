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
        if !self.governed_elsewhere(room) {
            // A room hosted on this device keeps its records in sealed
            // channels: nothing here commits an app's record in the clear.
            return Err("this room's records are sealed channels its apps open themselves".into());
        }
        // The host governing the room created the empty record for this
        // device; the bootstrap is this member's own change to it, which
        // the host admits like any other.
        let revision = owner.initialize_application_record_as_member(
            room,
            &certificate,
            &identity.device_signing_key,
            initialization,
            SystemTime::now(),
        )?;
        self.schedule_native_owner_publication(room);
        Ok(revision)
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
        // A room joined through a site is reached over its host's HTTPS
        // routes; every other over the iroh endpoint its hint names.
        let https = match self.hosted_sites.origin(room) {
            Some(origin) => Some(crate::network::actor::HttpsRoute(
                super::hosted_sites::HostHttp::new(&origin)?,
            )),
            None => None,
        };
        self.cmd_tx
            .try_send(NetCmd::NativeConnect(connect, https))
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
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .any(|slot| room_topic_id(slot.room()) == ws)
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
                        route: None,
                    });
                }
                true
            }
            NeighbourMsg::MlsRoutedAccessRequest {
                room: named,
                request,
                route,
                ..
            } => {
                if RoomId::parse(named) == Some(room) {
                    self.queue_channel_governance(InboundChannelGovernance::AccessRequest {
                        room,
                        request: request.clone(),
                        route: Some(route.clone()),
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
    /// The other member devices the governance of enrolled `room` admits
    /// now: neither this device nor the room's governor. `None` for a room
    /// this device holds no native owner for.
    pub(super) fn governed_member_devices(&self, room: RoomId) -> Option<Vec<iroh::EndpointId>> {
        self.admitted_devices(room, false)
    }

    /// The other devices the governance of enrolled `room` admits now, its
    /// governor's among them: every device one of its people may be on,
    /// the host's own when the room is hosted on a person's device. `None`
    /// for a room this device holds no native owner for.
    pub(super) fn governed_devices(&self, room: RoomId) -> Option<Vec<iroh::EndpointId>> {
        self.admitted_devices(room, true)
    }

    /// The other devices the governance of `room` admits now, the
    /// governor's only when `governor_too`.
    fn admitted_devices(&self, room: RoomId, governor_too: bool) -> Option<Vec<iroh::EndpointId>> {
        let owner = self.opened_native_owner()?;
        owner
            .with_current(room, |view| {
                let now = SystemTime::now();
                let own = view.device().device_public_key;
                let governor = view.governor().device_public_key;
                Ok(view
                    .peer()
                    .certificates()
                    .into_iter()
                    .filter(|cert| {
                        cert.device_public_key != own
                            && (governor_too || cert.device_public_key != governor)
                            && view.peer().permissions(cert.device_public_key, now).is_ok()
                    })
                    .filter_map(|cert| iroh::EndpointId::from_bytes(&cert.device_public_key).ok())
                    .collect())
            })
            .ok()
    }

    /// The identity the governance of the enrolled room on topic `ws`
    /// admitted for `device`, a member's device the room's relay
    /// authenticated: a browser of the room announces no identity of its
    /// own to this device.
    ///
    /// Asked for inbound events on the frame loop, so it reads only an
    /// owner already open: the tick holds inbound events back while one is
    /// opening ([`ShellNetwork::tick`]), and without one no device is
    /// governed.
    pub(super) fn governed_identity(
        &self,
        ws: u64,
        device: iroh::EndpointId,
    ) -> Option<crate::network::identity::PeerIdentity> {
        let room = self.room_for_topic(ws)?;
        let owner = self.opened_native_owner()?;
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
    /// Like [`Self::governed_identity`], it reads only an owner already
    /// open, and allows nothing without one.
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
        let Some(owner) = self.opened_native_owner() else {
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
    ///
    /// Read from the room's committed enrollment, which names its governor
    /// from the moment the room is installed, never from the tree's
    /// projection of it: a joining device is replayed the room's sealed
    /// writes together with the room's document, and the tree learns the
    /// room is sealed only once that document is admitted. A sealed write
    /// taken for a committed one meanwhile waits for a checkpoint of its
    /// record that the governor never commits.
    pub(super) fn native_room_seals_records(&self, room: RoomId) -> bool {
        self.opened_native_owner().is_some_and(|owner| {
            owner
                .with_current(room, |view| {
                    Ok(view
                        .peer()
                        .authority()
                        .capabilities
                        .stewards_room_channels())
                })
                .unwrap_or(false)
        })
    }
    /// Whether `input` is admitted together with the inputs of its session
    /// queued right before and after it: an app write the owner commits.
    pub(super) fn admits_in_batch(&self, input: &native_ingress::Input) -> bool {
        matches!(input.message(), NeighbourMsg::AppDocument { .. })
            && !self.sealed_record_write(input.message())
    }
    /// Handle `input` here, admitting it in place: a commit through the
    /// room's owner, which publishes the state file.
    pub(super) fn dispatch_native_input(&self, input: native_ingress::Input) {
        if self.dispatch_channel_traffic(&input) {
            return;
        }
        self.dispatch_native_inputs(vec![input]);
    }

    /// Admit `inputs`, all of one session and none channel traffic, here,
    /// in one commit through the room's owner.
    pub(super) fn dispatch_native_inputs(&self, inputs: Vec<native_ingress::Input>) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        self.complete_native_inputs(super::native_admission::admit(inputs, identity));
    }

    /// Handle `input` on the frame loop: what needs no admission here, its
    /// admission on the admission worker, which no later event overtakes.
    pub(super) fn dispatch_native_input_off_loop(&self, input: native_ingress::Input) {
        if self.dispatch_channel_traffic(&input) {
            return;
        }
        self.dispatch_native_inputs_off_loop(vec![input]);
    }

    /// [`Self::dispatch_native_inputs`] on the admission worker.
    pub(super) fn dispatch_native_inputs_off_loop(&self, inputs: Vec<native_ingress::Input>) {
        let Some(identity) = self.host_identity.clone() else {
            return;
        };
        if let Some(inputs) = self.admit_off_loop(inputs, identity) {
            // No worker to take them: admitting late would reorder them.
            self.dispatch_native_inputs(inputs);
        }
    }

    /// Ask `room`'s host to replay what it retains, for app messages this
    /// device let go before what they waited for arrived.
    pub(super) fn request_native_replay(&self, room: RoomId) {
        let _ = self.cmd_tx.try_send(NetCmd::NativeReplay {
            ws: room_topic_id(room),
        });
    }

    /// What follows an admission, on the thread handling inbound events:
    /// each input's remainder in the order they came, and the owner's
    /// publication once for the commit.
    pub(super) fn complete_native_inputs(&self, admitted: super::native_admission::Admitted) {
        let super::native_admission::Admitted { inputs, changed } = admitted;
        let room = inputs.first().map(|(input, _)| input.room());
        for (input, report) in inputs {
            self.complete_native_input(input, changed, report);
        }
        // A genuine committed change can enable successors or retire custody.
        // An unchanged replay must not enqueue another expensive recovery pass.
        if changed && let Some(room) = room {
            let _ = self.cmd_tx.try_send(NetCmd::NativePublish {
                ws: room_topic_id(room),
            });
        }
    }

    /// What follows one input's admission.
    fn complete_native_input(
        &self,
        input: native_ingress::Input,
        changed: bool,
        report: Result<tilo_peer_state::AdmissionReport, String>,
    ) {
        let report = match report {
            Ok(report) => report,
            Err(error) if error == tilo_peer_state::HELD_FOR_SURFACE => {
                tracing::debug!(room = %input.room(), "native relay input held for its app surface");
                // A surface this device has no copy of is a newer build of
                // the app: its update brings it.
                if let NeighbourMsg::AppDocument {
                    app_id, envelope, ..
                } = input.message()
                    && let Some(surface) = envelope.surface
                {
                    crate::shell::app_versions::surface_named(app_id, surface);
                }
                self.native_admission.hold(input);
                return;
            }
            Err(error) => {
                tracing::warn!(%error,room=%input.room(),"native relay admission denied");
                return;
            }
        };
        if report.summary().replay_wanted {
            self.request_native_replay(input.room());
        }
        tracing::debug!(
            room = %input.room(),
            author = %hex::encode(&input.author()[..4]),
            kind = input.message().kind(),
            retained = input.retained(),
            changed,
            "native relay input admitted"
        );
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
