//! Joining: one ordered admission of a connection's identity, route,
//! custody and channel handle.

use super::*;

impl AppConnection {
    /// Admit `join` and hold its channel for this connection: the identity
    /// is verified, a room still being joined or placed is waited for, the
    /// route's access and the participant's delegation are decided, and the
    /// route is held and published.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered join admission: identity, route, custody and channel handle"
    )]
    pub(super) async fn join(&self, join: Join) -> Result<JoinedChannel, BrokerError> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let mut phases = JoinPhases::start();
        let Join {
            channel: channel_id,
            identity,
            participant,
            record_kind,
        } = join;
        let route = CrdtRoute::from_channel_key(&channel_id);
        // Decoded first, so the match scrutinizes a plain value: the join
        // presentations each hold a `Drop`, which must not live past the arm.
        let task_presentation =
            runtime_collab::TaskAgentJoinIdentity::decode(&identity).map_err(|error| {
                BrokerError::failed(format!(
                    "task agent join presentation is malformed: {error}"
                ))
            })?;
        let mut verified_identity = match task_presentation {
            Some(presentation) => {
                if self
                    .worker
                    .as_ref()
                    .and_then(|worker| worker.host_scope.as_ref())
                    .is_some()
                {
                    return Err(BrokerError::failed(
                        "a host worker never joins as a task agent".into(),
                    ));
                }
                super::verify_task_agent_join(
                    &route,
                    &presentation,
                    state.membership.as_ref(),
                    state
                        .trusted_devices
                        .as_ref()
                        .map(|devices| devices.as_slice()),
                    state.certificate_authority.get().map(Arc::as_ref),
                )
                .map_err(|error| {
                    BrokerError::failed(format!("collaboration join identity rejected: {error}"))
                })?
            }
            None => admit_join_presentation(
                &route,
                &super::join_presentation(&identity).map_err(BrokerError::failed)?,
                state.membership.as_ref(),
                state
                    .trusted_devices
                    .as_ref()
                    .map(|devices| devices.as_slice()),
                state.certificate_authority.get().map(Arc::as_ref),
                self.worker
                    .as_ref()
                    .and_then(|worker| worker.host_scope.as_ref()),
            )
            .map_err(|error| {
                BrokerError::failed(format!("collaboration join identity rejected: {error}"))
            })?,
        };
        phases.mark("verify");
        // A room still being joined or placed on its host holds the join
        // until apps can open documents in it, whichever client asks.
        if let Some(memberships) = state.services.workspace_memberships.clone() {
            let mut status = memberships.room_status(&route.cluster_id);
            while matches!(status, runtime_collab::RoomStatus::Waiting { .. }) {
                status = memberships
                    .room_status_change(&route.cluster_id, status)
                    .await;
            }
            if let runtime_collab::RoomStatus::Refused(reason) = status {
                return Err(BrokerError::failed(reason));
            }
        }
        // A renamed app keeps using the record of an id it had before (`kept_record`).
        let record_route = route.clone();
        let (route, as_app) =
            kept_record::working_route(self.lease.local_media.as_deref(), &record_route);
        // An attested launch joins its own route; another app's only to
        // read a record whose kinds it declared it reads.
        let route_access = match self.lease.local_media.as_ref() {
            Some(app) => {
                let person_room = verified_identity
                    .verified
                    .owns_person_room(&route.cluster_id);
                let now = state.policy_now_ns().map_err(BrokerError::failed)? / 1_000_000_000;
                attested_route_access(&app.app_id, &route, person_room, || {
                    identity_reads_record(
                        state.membership.as_ref(),
                        &verified_identity,
                        &route,
                        now,
                    )
                })
                .map_err(BrokerError::failed)?
            }
            None => RouteAccess::Own,
        };
        if let Some(worker) = &self.worker {
            if record_route != worker.route
                || verified_identity.verified.user_public_key != worker.root
                || verified_identity.verified.device_public_key != worker.device
            {
                return Err(BrokerError::failed(
                    "worker descriptor route or certificate mismatch".into(),
                ));
            }
            if let Some(registration) = &worker.registration {
                if registration.route != route
                    || verified_identity.certificate.as_ref() != Some(&registration.certificate)
                {
                    return Err(BrokerError::failed(
                        "registered worker exact certificate changed".into(),
                    ));
                }
                verified_identity.worker_registration = Some(registration.clone());
            }
        } else if state.durable_owner_required && !route.app_id.is_empty() {
            return Err(BrokerError::failed(
                "application requires host-owned worker descriptor admission".into(),
            ));
        }
        let record_kind = route_record_kind(
            &record_route,
            joined_record_kind(record_kind.as_deref().unwrap_or_default())?,
        )?;
        let person = matches!(participant, Participant::Person { .. });
        if (verified_identity.host_scope.is_some()
            || verified_identity.worker_registration.is_some())
            && !person
        {
            return Err(BrokerError::failed(
                "host worker cannot delegate an agent identity".into(),
            ));
        }
        // A task agent joins only as itself: its verified task delegation is
        // its certification, and a person's handle would name it otherwise.
        super::task_agents::joins_as(
            &verified_identity,
            match &participant {
                Participant::Person { .. } => "person",
                Participant::Agent { .. } => "agent",
            },
        )
        .map_err(BrokerError::failed)?;
        match participant {
            Participant::Person {
                handle: participant_handle,
            } => {
                // A Person handle is presentation, not its certified identity.
                let participant_handle = participant_handle
                    .trim()
                    .chars()
                    .take(64)
                    .collect::<String>();
                if !participant_handle.is_empty() {
                    verified_identity.display_handle = Some(participant_handle);
                }
            }
            Participant::Agent {
                id: participant_handle,
            } if verified_identity.task.is_some() => {
                // A task agent's run keeps it: it acts on its delegated task
                // alone, and the tracker says the task still goes.
                state
                    .admit_task_join(&route.cluster_id, &participant_handle, &verified_identity)
                    .map_err(BrokerError::failed)?;
            }
            Participant::Agent {
                id: participant_handle,
            } => {
                // Unlike a display handle, this names the exact workspace
                // principal. Normalize as enrollment does, then validate
                // its byte bound; truncation can silently select another
                // admitted Agent that shares the same display prefix.
                let participant_handle = participant_handle.trim();
                // Bound allocation before Unicode case normalization;
                // every normalized character consumes at least one byte.
                if participant_handle.chars().take(129).count() > 128 {
                    return Err(BrokerError::failed(
                        "invalid canonical workspace agent identifier".into(),
                    ));
                }
                let participant_handle = participant_handle.to_lowercase();
                tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                    agent: participant_handle.clone(),
                }
                .validate()
                .map_err(BrokerError::failed)?;
                let membership = state.membership.as_ref().ok_or_else(|| {
                    BrokerError::failed("agent session has no workspace authority".into())
                })?;
                let now = state.policy_now_ns().map_err(BrokerError::failed)? / 1_000_000_000;
                membership
                    .decide_in_room(&route.cluster_id, |workspace| {
                        admit_agent_delegation_at(
                            workspace,
                            &mut verified_identity,
                            &participant_handle,
                            now,
                        )
                    })
                    .and_then(std::convert::identity)
                    .map_err(|error| {
                        BrokerError::failed(format!("agent collaboration join rejected: {error}"))
                    })?;
            }
        }
        // An app the person opens, launched or restored into a tab, is
        // let author the room as its person may let it: asked where
        // every opening of an app joins. The join goes on meanwhile:
        // the app asks for the room's keys again until the grant is in.
        if person
            && verified_identity.agent_id.is_none()
            && let Some(certificate) = verified_identity.certificate.clone()
        {
            let state = Arc::clone(&state);
            let route = route.clone();
            tokio::task::spawn_local(async move {
                if let Err(error) = state.admit_opening_app(&route, &certificate).await {
                    tracing::warn!(
                        room = %route.cluster_id,
                        app = %route.app_id,
                        %error,
                        "an opening app was not let author its room"
                    );
                }
            });
        }
        tracing::info!(
            target: "tilo_collab_probe",
            channel = %channel_id,
            peer_id,
            role = ?verified_identity.role,
            principal = %verified_identity.principal_id,
            acting_as = ?verified_identity.agent_id,
            device = ?{
                collab_types::Ed25519PublicKey::from_bytes(
                    verified_identity.verified.device_public_key,
                )
            },
            "PROBE join_channel",
        );
        if self.worker.is_none()
            && person
            && let Some(registration) = host_workers::packaged_app_registration(
                self.app_worker.as_ref(),
                &route,
                &verified_identity,
            )
        {
            verified_identity.worker_registration = Some(registration);
        }
        phases.mark("admit");
        let ensured = channel_id.clone();
        let retained = verified_identity.clone();
        let hold = self.holds.hold(&state, &self.directories, peer_id, &route);
        phases.mark("hold");
        let hold_id = hold.id();
        let handle = JoinedChannel {
            person_chat_views: RefCell::default(),
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: state.clone(),
            directories: Rc::clone(&self.directories),
            channel_id,
            as_app,
            peer_id: hold_id,
            connection_id: peer_id,
            verified_identity,
            record_kind,
            route_access,
            lease: Some(Rc::clone(&self.lease)),
            _hold: Some(hold),
        };
        // Refused, the handle and its hold drop here and withdraw again.
        state.retain_host_worker(hold_id, &route, &retained)?;
        phases.mark("retain-worker");
        state.ensure_channel(&ensured);
        phases.mark("ensure-channel");
        state
            .publisher()
            .local_route(&crate::route::CrdtRoute::from_channel_key(&ensured));
        phases.mark("publish-route");
        phases.report(&ensured);
        Ok(handle)
    }
}
