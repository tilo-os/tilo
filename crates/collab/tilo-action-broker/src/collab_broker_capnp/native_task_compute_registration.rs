//! Managed identity admission and private first-use Start orchestration.
use super::{
    AdmittedJoinIdentity, CapnpBrokerState, CapnpChannelHandle, InvocationEnvelope,
    PendingInvocation,
};

#[derive(Clone, Debug)]
pub(super) struct ManagedActorBinding {
    pub(super) custody: std::sync::Arc<dyn crate::services::ManagedApplicationIdentity>,
    pub(super) delegation: tilo_workspace_join::delegation::Delegation,
}
impl PartialEq for ManagedActorBinding {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.custody, &other.custody) && self.delegation == other.delegation
    }
}
impl Eq for ManagedActorBinding {}

impl CapnpBrokerState {
    /// The protected launcher, not the joining UI's person label, selects the actor.
    pub(super) fn admit_managed_task_identity(
        &self,
        app: &super::native_media::LocalAppConnection,
        route: &super::CrdtRoute,
        identity: &mut AdmittedJoinIdentity,
    ) -> Result<(), String> {
        let agent = app
            .managed_task_agent
            .as_deref()
            .ok_or("managed task actor absent")?;
        let matches_route = (&app.workspace_id, &app.app_id) == (&route.cluster_id, &route.app_id);
        if !app.current() || app.app_id != "so.tilo.agent" || !matches_route {
            return Err("managed task launch route or custody changed".into());
        }
        let custody = app
            .managed_identity
            .as_ref()
            .ok_or("managed actor custody absent")?;
        let (certificate, delegation, proof) = custody.identity()?;
        let now_ns = self.policy_now_ns()?;
        if certificate.user_public_key != identity.verified.user_public_key
            || delegation.delegate.agent_id() != Some(agent)
            || delegation.device_public_key != certificate.device_public_key
            || !delegation.permits_app(&route.app_id)
            || !delegation.permits_workspace(
                tilo_workspace_join::room_workspace_id(&route.cluster_id)
                    .map_err(|error| error.to_string())?,
            )
        {
            return Err("managed actor identity or room changed".into());
        }
        delegation
            .verify(&certificate.user_public_key, now_ns)
            .map_err(|error| error.to_string())?;
        self.certificate_authority
            .get()
            .ok_or("managed actor authority unavailable")?
            .authorize_join(
                uuid::Uuid::parse_str(&route.cluster_id).map_err(|error| error.to_string())?,
                &certificate,
            )?;
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: route.cluster_id.clone(),
            device_certificate: serde_json::to_vec(&certificate)
                .map_err(|error| error.to_string())?,
            proof: proof.to_vec(),
        };
        let verified = super::verify_join_presentation(
            route,
            &presentation,
            &certificate.user_public_key,
            now_ns,
            false,
            true,
        )?;
        self.membership
            .as_ref()
            .ok_or("managed task membership unavailable")?
            .decide_in_room(&route.cluster_id, |workspace| {
                super::admit_agent_delegation_at(workspace, identity, agent, now_ns / 1_000_000_000)
            })??;
        // Keep the person's capped membership role, but all signed operations
        // and executor registration are bound to the protected actor's key.
        identity.verified = verified;
        identity.certificate = Some(certificate);
        identity.host_scope = None;
        identity.worker_registration = None;
        identity.managed_actor = Some(ManagedActorBinding {
            custody: custody.clone(),
            delegation,
        });
        Ok(())
    }

    pub(super) async fn complete_native_compute_start(
        &self,
        room: &str,
        envelope: &InvocationEnvelope,
        pending: &PendingInvocation,
        request: &tilo_owner_commit::OwnerCommitRequest,
        service: &dyn crate::services::WorkspaceMemberships,
    ) -> Result<Option<tilo_owner_commit::OwnerCommitReceipt>, String> {
        use tilo_owner_commit::CommitOperation;
        use tilo_peer_state::workspace_task::WorkspaceTaskCommand;
        let CommitOperation::WorkspaceTaskCommand {
            command:
                WorkspaceTaskCommand::ApproveRun {
                    task_id,
                    agent_id,
                    expected_run_epoch,
                    after_revision,
                },
            ..
        } = &request.operation
        else {
            return Ok(None);
        };
        let snapshot = service.task_snapshot(room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| &task.id == task_id)
            .ok_or("task absent")?;
        // Assignment is not a live executor lease. Every native MODEL Start
        // freezes the source and original ceiling, including offline assignments.
        // Other routes retain their ordinary owner-backed approval path.
        if task.route != tilo_desktop_model::WorkspaceTaskRoute::Model {
            return Ok(None);
        }
        let control = snapshot
            .controls
            .get(task_id)
            .ok_or("task control absent")?;
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::Queued
            || task
                .assigned_agent
                .as_deref()
                .is_some_and(|agent| agent != agent_id.as_str())
            || control.active_run.is_some()
            || control.run_epoch != *expected_run_epoch
            || control
                .directions
                .last()
                .map_or(0, |direction| direction.revision)
                != *after_revision
        {
            return Err("queued task frontier changed; review before starting".into());
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("task Start original custody unavailable")?;
        original.owner_person_certificate(self, room, envelope)?;
        let registry = self
            .services
            .native_task_executors
            .as_ref()
            .ok_or("native executor registration unavailable")?;
        let launcher = self
            .services
            .launcher
            .as_ref()
            .ok_or("native executor launch unavailable")?;
        let workspace = self
            .membership
            .as_ref()
            .ok_or("native Start workspace authority unavailable")?
            .decide_in_room(room, |workspace| {
                workspace
                    .cloned()
                    .ok_or_else(|| "native Start workspace absent".to_string())
            })??;
        // Freeze the actual person's frontier, permission ceiling and complete
        // host MODEL contract before provisioning can change room governance.
        let prepared = service.prepare_task_start(request.clone())?;
        if pending
            .cancellation_fence
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("task Start cancelled".into());
        }
        original.owner_person_certificate(self, room, envelope)?;
        if registry.live_executor(room, agent_id).is_none() {
            launcher.launch_task_executor(agent_id, &workspace)?;
        }
        let executor = await_native_executor(registry, room, agent_id).await?;
        if !self.is_accepting()
            || pending
                .cancellation_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("task Start cancelled or broker retired".into());
        }
        original.owner_person_certificate(self, room, envelope)?;
        if !registry.is_current(&executor) {
            return Err("protected executor subscription retired before Start".into());
        }
        // This capability rechecks the original frontier, current person and
        // actor standing, governor continuity and exact frozen MODEL contract.
        let receipt = prepared.approve(executor).await?;
        receipt.validate_for(request)?;
        Ok(Some(receipt))
    }
}

/// Wait for the protected executor subscription the Start just launched.
async fn await_native_executor(
    registry: &crate::services::NativeTaskExecutorRegistry,
    room: &str,
    agent_id: &str,
) -> Result<crate::services::NativeTaskExecutor, String> {
    Ok(
        tokio::time::timeout(crate::services::APPLICATION_STARTUP_TIMEOUT, async {
            loop {
                let changed = registry.changed();
                if let Some(executor) = registry.live_executor(room, agent_id) {
                    break executor;
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| "native executor did not subscribe before the opening deadline")?,
    )
}

impl CapnpChannelHandle {
    pub(super) fn register_native_task_executor(
        &self,
        current: &AdmittedJoinIdentity,
        agent: &str,
    ) -> Result<(), String> {
        let Some(registry) = &self.state.services.native_task_executors else {
            return Ok(());
        };
        let Some(app) = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
        else {
            return Ok(());
        };
        let route = self.route();
        let matches_route = (&app.workspace_id, &app.app_id) == (&route.cluster_id, &route.app_id);
        if !app.current()
            || app.app_id != "so.tilo.agent"
            || !matches_route
            || app.managed_task_agent.as_deref() != Some(agent)
            || current.agent_id.as_deref() != Some(agent)
        {
            return Ok(());
        }
        if current.managed_actor.is_none() {
            return Err("native executor lacks protected actor custody".into());
        }
        let protected = app
            .protected_model
            .as_ref()
            .ok_or("native executor lacks protected MODEL launch")?;
        protected
            .validate(&app.app_id, Some(&app.workspace_id))
            .map_err(|error| error.to_string())?;
        if protected.agent != agent {
            return Err("native executor MODEL actor changed".into());
        }
        let certificate = current
            .certificate
            .clone()
            .ok_or("native task executor certificate absent")?;
        let state = std::sync::Arc::downgrade(&self.state);
        let app = app.clone();
        let admitted = current.clone();
        let check_route = route.clone();
        let check = std::sync::Arc::new(move || {
            app.current()
                && state.upgrade().is_some_and(|state| {
                    state.is_accepting()
                        && state.revalidate_admission(&check_route, &admitted).is_ok()
                })
        });
        registry.register(
            self.peer_id,
            crate::services::NativeTaskExecutor {
                room: route.cluster_id,
                agent: agent.into(),
                certificate,
            },
            check,
        )
    }
}
