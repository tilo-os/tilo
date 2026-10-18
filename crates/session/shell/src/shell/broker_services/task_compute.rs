//! Native compute eligibility and owner-custodied first-use Start.
use agent_provider_config::management::CredentialStatus;
use agent_provider_config::{ProviderProfile, ProviderProtocol};

/// Metadata only: never prepares transport, starts a runner, resolves a secret,
/// or calls a provider. Approval separately freezes the complete profile.
pub(super) fn configured_model() -> Result<String, String> {
    let selection = super::model_start::selected_model()?;
    let profile: ProviderProfile = serde_json::from_value(
        selection
            .contract
            .get("profile")
            .cloned()
            .ok_or("MODEL profile absent")?,
    )
    .map_err(|_| "MODEL profile invalid")?;
    check_profile_credential(&profile, |_| credential_status(&profile))?;
    Ok(selection.model)
}

/// Use the same origin-bound vault namespace as the gateway without resolving
/// bytes or routing a Settings-only administration RPC through the Agent door.
fn credential_status(profile: &ProviderProfile) -> Result<CredentialStatus, String> {
    if profile.sign_in.is_some() {
        return sign_in_status(profile);
    }
    let store = accounts::ServiceCredentials::open();
    if store
        .legacy_agent_provider_status(&profile.id)
        .map_err(|_| "MODEL credential custody unavailable")?
    {
        return Ok(CredentialStatus::MigrationRequired);
    }
    let authorization = accounts::ServiceAuthorization::new(
        tilo_model_gateway::SERVICE_NAME,
        "provider-api-key",
        &profile.endpoint,
    )
    .map_err(|_| "MODEL credential origin invalid")?;
    let reference = authorization
        .credential_ref(&profile.id)
        .map_err(|_| "MODEL credential profile invalid")?;
    Ok(
        if store
            .status(&reference, &authorization)
            .map_err(|_| "MODEL credential custody unavailable")?
        {
            CredentialStatus::Configured
        } else {
            CredentialStatus::Missing
        },
    )
}

#[cfg(target_os = "linux")]
fn sign_in_status(profile: &ProviderProfile) -> Result<CredentialStatus, String> {
    if !profile.signs_in_with_google() {
        return Err("MODEL sign-in issuer unsupported".into());
    }
    let id = accounts::AccountId::new(agent_provider_config::sign_in_grant_id(&profile.id));
    let state = account_custody::Custodian::live()
        .oauth_grant_state(&id)
        .map_err(|_| "MODEL sign-in custody unavailable")?;
    Ok(if state == account_custody::GrantState::Usable {
        CredentialStatus::Configured
    } else {
        CredentialStatus::Missing
    })
}

#[cfg(not(target_os = "linux"))]
fn sign_in_status(_profile: &ProviderProfile) -> Result<CredentialStatus, String> {
    Err("MODEL sign-in custody unavailable".into())
}

fn check_profile_credential(
    profile: &ProviderProfile,
    status: impl FnOnce(&str) -> Result<CredentialStatus, String>,
) -> Result<(), String> {
    // Both local and external OpenAI-compatible services may deliberately have
    // no authentication. A missing reference is not a missing configured key.
    if profile.sign_in.is_none() && profile.credential.is_none() {
        return if profile.protocol == ProviderProtocol::OpenAiCompatible {
            Ok(())
        } else {
            Err("MODEL provider requires a credential".into())
        };
    }
    match status(&profile.id)? {
        CredentialStatus::Configured => Ok(()),
        CredentialStatus::Missing => Err("MODEL credential not configured".into()),
        CredentialStatus::MigrationRequired => Err("MODEL credential migration required".into()),
    }
}
use super::NativeOwnerCommitService;
use tilo_action_broker::services::NativeTaskComputeRegistration;
use tilo_user_identity::DeviceCert;

#[cfg(test)]
#[path = "task_compute_tests.rs"]
mod tests;

#[derive(serde::Deserialize)]
pub(super) struct GovernorCompute {
    #[serde(rename = "configuredModel")]
    pub(super) configured_model: Option<String>,
    workers: Vec<GovernorWorker>,
}

#[derive(serde::Deserialize)]
struct GovernorWorker {
    agent_id: String,
    workspace: String,
    profile: String,
    eligible: bool,
    configured: bool,
    registered: bool,
}

struct PendingStart {
    service: NativeOwnerCommitService,
    prepared: tilo_joined_room::PreparedNativeTaskStart,
    room: String,
    agent: String,
    caller: DeviceCert,
}

impl tilo_action_broker::services::PendingNativeTaskStart for PendingStart {
    fn approve(
        &self,
        executor: tilo_action_broker::services::NativeTaskExecutor,
    ) -> tilo_owner_commit::CommitFuture {
        // The owner transaction is synchronous and completes before releasing
        // custody; the returned future owns only its resulting receipt/error.
        let result = (|| {
            if executor.room != self.room || executor.agent != self.agent {
                return Err("protected executor differs from prepared Start".into());
            }
            let owner = self
                .service
                .opened_owner("native Start owner unavailable")?;
            let room =
                super::super::state::RoomId::parse(&self.room).ok_or("invalid Start room")?;
            if let Some(network) = &self.service.network {
                network.check_native_custody(&owner, room)?;
                if network.governed_elsewhere(room) {
                    return Err("prepared Start requires the actual governor".into());
                }
            }
            let (key, current) = tilo_identity_host::broker::trusted_current_device(&self.room)
                .map_err(|error| format!("Start governor custody unavailable: {error}"))?;
            if current != self.caller {
                return Err("selected native person changed while Start was pending".into());
            }
            configured_model()?;
            let windows = crate::shell::host_window_contracts::published(&self.service.tree.read());
            if !self.service.task_executors.is_current(&executor) {
                return Err("protected executor subscription retired before Start".into());
            }
            let receipt = owner.approve_prepared_task_start(
                &self.prepared,
                &executor.certificate,
                &key,
                &super::model_start::ModelStart {
                    windows,
                    origin: None,
                },
                std::time::SystemTime::now(),
            )?;
            if let Some(network) = &self.service.network {
                network.schedule_native_owner_publication(room);
            }
            Ok(receipt)
        })();
        Box::pin(async move { result })
    }
}

impl NativeOwnerCommitService {
    pub(super) fn prepare_native_task_start(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
    ) -> Result<Box<dyn tilo_action_broker::services::PendingNativeTaskStart>, String> {
        use tilo_owner_commit::CommitOperation;
        use tilo_peer_state::workspace_task::WorkspaceTaskCommand;
        let CommitOperation::WorkspaceTaskCommand {
            authority: tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person { caller },
            command: WorkspaceTaskCommand::ApproveRun { agent_id, .. },
            ..
        } = &request.operation
        else {
            return Err("prepared native Start requires ApproveRun".into());
        };
        let owner = self.opened_owner("native Start owner unavailable")?;
        let room_id =
            super::super::state::RoomId::parse(&request.room).ok_or("invalid Start room")?;
        if let Some(network) = &self.network {
            network.check_native_custody(&owner, room_id)?;
            if network.governed_elsewhere(room_id) {
                return Err("prepared Start requires the actual governor".into());
            }
        }
        let (_, selected) = tilo_identity_host::broker::trusted_current_device(&request.room)
            .map_err(|error| format!("selected native person unavailable: {error}"))?;
        if &selected != caller {
            return Err(
                "first-use Start requires the actual selected native person's device".into(),
            );
        }
        owner.with_current(room_id, |view| {
            if view.device() != caller || view.governor() != caller {
                return Err(
                    "managed provisioning for a foreign workspace owner is unsupported".into(),
                );
            }
            Ok(())
        })?;
        let caller = caller.clone();
        configured_model()?;
        let room = request.room.clone();
        let agent = agent_id.clone();
        // Freeze actual source and full profile before any executor enrollment.
        let windows = crate::shell::host_window_contracts::published(&self.tree.read());
        let prepared = owner.prepare_task_start(
            request,
            &super::model_start::ModelStart {
                windows,
                origin: None,
            },
            std::time::SystemTime::now(),
        )?;
        Ok(Box::new(PendingStart {
            service: self.clone(),
            prepared,
            room,
            agent,
            caller,
        }))
    }

    pub(super) fn governor_task_compute(
        &self,
        room: &str,
        caller: &DeviceCert,
    ) -> Result<Option<GovernorCompute>, String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid compute room")?;
        let Some(network) = self
            .network
            .as_ref()
            .filter(|network| network.governed_elsewhere(room_id))
        else {
            return Ok(None);
        };
        let owner = self.opened_owner("native compute owner unavailable")?;
        network.check_native_custody(&owner, room_id)?;
        let check = || {
            owner.with_current(room_id, |view| {
                if view.device() != caller {
                    return Err(
                        "governor compute query requires the actual current caller device".into(),
                    );
                }
                view.peer()
                    .permissions(caller.device_public_key, std::time::SystemTime::now())?;
                Ok(())
            })
        };
        check()?;
        // The existing certified governor route signs as this exact device.
        // This is a no-argument read action; no local model/vault is sampled.
        let answer = network.invoke_governor(
            room_id,
            "so.tilo.desktop",
            "desktop:tasks",
            "read-task-compute",
            None,
        );
        let value = answer
            .wait_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| "governor compute query timed out")?
            .ok_or("governor compute query stopped")??;
        check()?;
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| "invalid governor compute metadata".into())
    }

    pub(super) fn check_task_executor_command(
        &self,
        request: &tilo_owner_commit::OwnerCommitRequest,
    ) -> Result<(), String> {
        use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};
        let tilo_owner_commit::CommitOperation::WorkspaceTaskCommand {
            authority, command, ..
        } = &request.operation
        else {
            return Ok(());
        };
        let (WorkspaceTaskCommand::BindExecutor {
            task_id, agent_id, ..
        }
        | WorkspaceTaskCommand::ApproveRun {
            task_id, agent_id, ..
        }) = command
        else {
            return Ok(());
        };
        let WorkspaceTaskAuthority::Person { caller } = authority else {
            return Err("only a person may select task compute".into());
        };
        let snapshot =
            tilo_action_broker::services::WorkspaceMemberships::task_snapshot(self, &request.room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| &task.id == task_id)
            .ok_or("task absent")?;
        if task.route != tilo_desktop_model::WorkspaceTaskRoute::Model {
            return Ok(());
        }
        if !self
            .current_task_compute(&request.room, caller)?
            .unwrap_or_default()
            .iter()
            .any(|entry| {
                entry.agent_id == *agent_id
                    && entry.current_certified_executor
                    && entry.model_ready
                    && entry.permitted
            })
        {
            return Err("task requires a configured authenticated native executor".into());
        }
        Ok(())
    }

    pub(super) fn current_task_compute(
        &self,
        room: &str,
        caller: &DeviceCert,
    ) -> Result<Option<Vec<NativeTaskComputeRegistration>>, String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid compute room")?;
        let owner = self.opened_owner("native compute owner unavailable")?;
        owner.health()?;
        if let Some(network) = &self.network {
            network.check_native_custody(&owner, room_id)?;
            if network.governed_elsewhere(room_id) {
                let sample = self
                    .governor_task_compute(room, caller)?
                    .ok_or("governor compute metadata unavailable")?;
                return Ok(Some(
                    sample
                        .workers
                        .into_iter()
                        .filter(|worker| worker.workspace == room)
                        .map(|worker| NativeTaskComputeRegistration {
                            agent_id: worker.agent_id,
                            workspace: worker.workspace,
                            profile: worker.profile,
                            current_certified_executor: worker.registered,
                            model_ready: worker.configured,
                            // The governor omits forbidden candidates. Offline
                            // retained entries remain selectable for Start, but
                            // registered=false still prevents ordinary binding.
                            permitted: worker.eligible || (!worker.registered && worker.configured),
                        })
                        .collect(),
                ));
            }
        }
        // Authenticate before inspecting configuration; no gateway or vault
        // wait runs while holding the owner's serialized turn.
        owner.with_current(room_id, |view| {
            if !view.peer().certificates().iter().any(|cert| cert == caller) {
                return Err("compute caller certificate changed".into());
            }
            view.peer()
                .permissions(caller.device_public_key, std::time::SystemTime::now())?;
            Ok(())
        })?;
        let profile = configured_model().ok();
        // Subscription checks re-enter owner admission. Release the owner's
        // read guard before sampling them so a queued writer cannot deadlock us.
        let authority = owner.with_current(room_id, |view| {
            Ok((view.device() == view.governor())
                .then(|| (view.peer().clone(), view.device().clone())))
        })?;
        let Some((peer, governor)) = authority else {
            return Ok(None);
        };
        self.task_executors
            .sample(&peer, &governor, room, caller, profile.as_deref())
            .map(Some)
    }
}
