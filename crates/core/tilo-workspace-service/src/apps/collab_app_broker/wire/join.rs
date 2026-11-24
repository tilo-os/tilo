//! Joining: one ordered admission of a connection's identity, route,
//! ownership and channel handle.

use super::*;

impl AppConnection {
    /// Admit `join` and hold its channel for this connection: the identity
    /// is verified, a workspace still being joined or placed is waited for, the
    /// route's access and the member's delegation are decided, and the
    /// route is held and published.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered join admission: identity, route, ownership and channel handle"
    )]
    pub(super) async fn join(&self, join: Join) -> Result<JoinedChannel, BrokerError> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let mut phases = JoinPhases::start();
        let Join {
            channel: channel_id,
            identity,
            member,
            handle: announced,
            resource_type,
        } = join;
        let route = CrdtRoute::from_channel_key(&channel_id);
        let mut verified_identity = admit_join_presentation(
            &route,
            &super::join_presentation(&identity).map_err(BrokerError::failed)?,
            state.membership.as_ref(),
            state
                .trusted_devices
                .as_ref()
                .map(|devices| devices.as_slice()),
            state.certificate_authority.get().map(Arc::as_ref),
        )
        .map_err(|error| {
            BrokerError::failed(format!("collaboration join identity rejected: {error}"))
        })?;
        phases.mark("verify");
        // A workspace still being joined or placed on its host holds the join
        // until apps can open documents in it, whichever client asks.
        if let Some(memberships) = state.services.workspace_memberships.clone() {
            let mut status = memberships.workspace_status(&route.cluster_id);
            while matches!(status, runtime_collab::WorkspaceStatus::Waiting { .. }) {
                status = memberships
                    .workspace_status_change(&route.cluster_id, status)
                    .await;
            }
            if let runtime_collab::WorkspaceStatus::Refused(reason) = status {
                return Err(BrokerError::failed(reason));
            }
        }
        let resource_route = route.clone();
        // An attested launch joins its own route; another app's only to
        // read a record whose kinds it declared it reads, as the device's one
        // reader lets the person read it.
        let route_access = match self.lease.local_media.as_ref() {
            Some(app) => {
                let own_workspace = verified_identity
                    .verified
                    .owns_person_workspace(&route.cluster_id);
                let access = attested_route_access(&app.app_id, &route, own_workspace)
                    .map_err(BrokerError::failed)?;
                if access == RouteAccess::ReadOnly {
                    state
                        .person_reads_joined(&verified_identity, &app.app_id, &route)
                        .await
                        .map_err(BrokerError::failed)?;
                }
                access
            }
            None => RouteAccess::Own,
        };
        let person = member.is_none();
        // The app a person's launcher started is the worker of that app's
        // routes on this device: the launcher verified which app it released
        // and who it released it for, which is the same durable admission a
        // host-mounted worker's private descriptor carries. Decided here,
        // before the route is held, so a durable broker refuses an
        // unattested application instead of admitting it and withdrawing it.
        // The app is the worker of its own route whichever actor joins it:
        // a person posting in Chat, and the workspace's kept agent opening the
        // route its package declares, both speak for that app's records.
        // An app reading another app's record it declares it reads is no
        // worker of that route: it changes nothing there and publishes nothing
        // on it, so its attested launch and the person's read of the record
        // admit it without the route's worker descriptor.
        let packaged_app = self
            .worker
            .is_none()
            .then(|| {
                host_workers::packaged_app_registration(
                    self.app_worker.as_ref(),
                    &route,
                    &verified_identity,
                )
            })
            .flatten();
        if let Some(worker) = &self.worker {
            if resource_route != worker.route
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
        } else if state.durable_owner_required
            && !route.app_id.is_empty()
            && matches!(route_access, RouteAccess::Own)
            && packaged_app.is_none()
            && !kept_for_every_app_of_the_workspace(&resource_route)
        {
            return Err(BrokerError::failed(
                "application requires host-owned worker descriptor admission".into(),
            ));
        }
        let resource_type = route_resource_type(
            &resource_route,
            joined_resource_type(resource_type.as_deref().unwrap_or_default())?,
        )?;
        let launcher_attested = packaged_app.is_some();
        if let Some(registration) = packaged_app {
            verified_identity.worker_registration = Some(registration);
        }
        // A connection holding a worker descriptor (a host authority's process,
        // or a host-mounted worker) never delegates an agent identity: its
        // authority is the person's own. A packaged app's launcher attestation
        // is not that — it only says which app's route this connection speaks
        // for — so the workspace's own agent joins its package's route through
        // its record (`admit_listed_agent`) like any app of the workspace.
        let descriptor_worker =
            verified_identity.worker_registration.is_some() && !launcher_attested;
        if descriptor_worker && !person {
            return Err(BrokerError::failed(
                "host worker cannot delegate an agent identity".into(),
            ));
        }
        match member {
            None => {
                // A person's handle is presentation, not their certified
                // identity.
                let announced = announced.trim().chars().take(64).collect::<String>();
                if !announced.is_empty() {
                    verified_identity.display_handle = Some(announced);
                }
            }
            Some(MemberId::Agent(member_handle)) => {
                // Unlike a display handle, this names the exact workspace
                // actor. Normalize as enrollment does, then validate
                // its byte bound; truncation can silently select another
                // admitted Agent that shares the same display prefix.
                let member_handle = member_handle.trim();
                // Bound allocation before Unicode case normalization;
                // every normalized character consumes at least one byte.
                if member_handle.chars().take(129).count() > 128 {
                    return Err(BrokerError::failed(
                        "invalid canonical workspace agent identifier".into(),
                    ));
                }
                let member_handle = member_handle.to_lowercase();
                tilo_peer_state::workspace_membership::valid_agent_id(&member_handle)
                    .map_err(BrokerError::failed)?;
                let roster = state.services.agents.as_deref().ok_or_else(|| {
                    BrokerError::failed("agent session has no workspace record to join on".into())
                })?;
                admit_listed_agent(
                    roster,
                    &route.cluster_id,
                    &mut verified_identity,
                    &member_handle,
                )
                .map_err(|error| {
                    BrokerError::failed(format!("agent collaboration join rejected: {error}"))
                })?;
            }
            Some(MemberId::Person(_) | MemberId::Bot(_)) => {
                return Err(BrokerError::failed(
                    "only a person's own device or their agent joins a channel".into(),
                ));
            }
        }
        tracing::info!(
            target: "tilo_collab_probe",
            channel = %channel_id,
            peer_id,
            role = ?verified_identity.role,
            actor = %verified_identity.actor_key,
            acting_as = ?verified_identity.agent_id,
            device = ?{
                collab_types::Ed25519PublicKey::from_bytes(
                    verified_identity.verified.device_public_key,
                )
            },
            "PROBE join_channel",
        );
        phases.mark("admit");
        let ensured = channel_id.clone();
        let retained = verified_identity.clone();
        let hold = self.holds.hold(&state, &self.directories, peer_id, &route);
        phases.mark("hold");
        let hold_id = hold.id();
        let handle = JoinedChannel {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: state.clone(),
            directories: Rc::clone(&self.directories),
            channel_id,
            peer_id: hold_id,
            connection_id: peer_id,
            verified_identity,
            resource_type,
            route_access,
            lease: Some(Rc::clone(&self.lease)),
            _hold: Some(hold),
        };
        // Refused, the handle and its hold drop here and withdraw again.
        state.retain_host_worker(hold_id, &route, &retained)?;
        phases.mark("retain-worker");
        state.ensure_channel(&ensured);
        phases.mark("ensure-channel");
        phases.report(&ensured);
        Ok(handle)
    }
}

/// Whether `route` names a record every app of its workspace opens
/// ([`collab_types::workspace_document`]): the workspace keeps it rather than the app that
/// writes it, so any admitted app of the workspace holds its route, and
/// [`attested_route_access`] lets it change the record as the workspace's grants on the
/// owner decide.
fn kept_for_every_app_of_the_workspace(route: &CrdtRoute) -> bool {
    collab_types::shared_workspace_document(&route.app_id, &route.document_id).is_some()
}
