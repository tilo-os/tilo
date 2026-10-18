//! Person-start MODEL snapshots from the shell's publication and committed Settings.
use super::NativeOwnerCommitService;
use crate::shell::host_window_contracts::{self, HostWindowContract};
use agent_provider_config::ProviderConfig;
use std::time::SystemTime;
use tilo_action_policy::ActionKind;
use tilo_joined_room::task_admission::model::{
    FrozenNativeModelSelection, NativeModelContract, NativeModelStartAuthority,
};
use tilo_joined_room::{NativeCommittedView, NativeWorkspaceOwner};

impl NativeOwnerCommitService {
    pub(super) fn read_task_model_contract(
        &self,
        room: &str,
        binding: &tilo_joined_room::task_policy::NativeTaskBinding,
    ) -> Result<NativeModelContract, String> {
        use tilo_relay_session::rooms::private::{PrivateOperation as O, PrivateReply as R};
        let route = super::task_mailboxes::remote::route(self, room, Some(binding), None)?;
        if let Some(route) = route {
            let R::TaskModelContract(encoded) = super::task_mailboxes::remote::command(
                route,
                O::NativeTaskRead {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                    agent: binding.agent_id.clone(),
                    evidence: Vec::new(),
                    operation: Box::new(O::TaskModelContract {
                        task: binding.task_id.clone(),
                        epoch: binding.epoch,
                        revision: binding.revision,
                    }),
                },
            )?
            else {
                return Err("unexpected private MODEL contract reply".into());
            };
            let contract: NativeModelContract = serde_json::from_str(&encoded)
                .map_err(|_| "invalid private MODEL contract reply")?;
            contract.source_for(room, &binding.task_id)?;
            return Ok(contract);
        }
        drop(route);
        let id = tilo_desktop_model::RoomId::parse(room).ok_or("invalid MODEL room")?;
        let owner = self.opened_owner("MODEL owner unavailable")?;
        let caller = owner.with_current(id, |view| Ok(view.device().clone()))?;
        owner.workspace_task_private_model_contract(
            id,
            binding,
            &tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Agent {
                attestor: caller,
                agent_id: binding.agent_id.clone(),
            },
            SystemTime::now(),
        )
    }

    pub(super) fn capture_person_model_start(
        &self,
        owner: &NativeWorkspaceOwner,
        request: &tilo_owner_commit::OwnerCommitRequest,
    ) -> Result<tilo_joined_room::task_admission::model_start::NativeModelPersonStartSnapshot, String>
    {
        let (key, device) = tilo_identity_host::broker::trusted_current_device(&request.room)
            .map_err(|_| "MODEL approving device key unavailable")?;
        let room = tilo_desktop_model::RoomId::parse(&request.room)
            .ok_or("MODEL approval room invalid")?;
        owner.with_current(room, |view| {
            if view.device() != &device {
                return Err("MODEL approving physical device changed".into());
            }
            Ok(())
        })?;
        // A replica does not carry the owner's private originating Chat
        // request. Fetch it through the approving person's authenticated door
        // before entering the synchronous signed-snapshot callback.
        let origin = if let tilo_owner_commit::CommitOperation::WorkspaceTaskCommand {
            command:
                tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun {
                    task_id,
                    expected_run_epoch,
                    ..
                },
            ..
        } = &request.operation
        {
            super::task_mailboxes::remote::person_instructions(
                self,
                &request.room,
                task_id,
                *expected_run_epoch,
                &device,
            )?
            .and_then(|text| text.agent_request.map(|origin| (task_id.clone(), origin)))
        } else {
            None
        };
        let windows = host_window_contracts::published(&self.tree.read());
        tilo_joined_room::task_admission::model_start::NativeModelPersonStartSnapshot::capture(
            owner,
            request,
            &ModelStart { windows, origin },
            &key,
            SystemTime::now(),
        )
    }

    pub(super) fn commit_task_with_model_start(
        &self,
        owner: &NativeWorkspaceOwner,
        request: tilo_owner_commit::OwnerCommitRequest,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
        self.check_task_executor_command(&request)?;
        // Snapshot tree -> owner, never acquire the tree in the owner callback.
        // Publication itself needs the tree writer, so release the read guard
        // before entering the owner's mutation/persistence turn.
        let windows = host_window_contracts::published(&self.tree.read());
        owner.commit_workspace_task_command_with_model_start(
            request,
            key,
            &ModelStart {
                windows,
                origin: None,
            },
            now,
        )
    }
}

pub(super) struct ModelStart {
    pub(super) windows: Vec<HostWindowContract>,
    pub(super) origin: Option<(String, tilo_desktop_model::WorkspaceAgentRequest)>,
}

impl NativeModelStartAuthority for ModelStart {
    fn approved_model_start(
        &self,
        view: &NativeCommittedView,
        task_id: &str,
        person: &tilo_user_identity::DeviceCert,
        _now: SystemTime,
    ) -> Result<NativeModelContract, String> {
        // Settings and the shared vault belong to this physical host's person.
        // Workspace editing permission is not consent to bill that account.
        // No shared-compute consent contract is installed here, so refuse a
        // different person's start rather than silently lending the profile.
        if person.user_public_key != view.device().user_public_key {
            return Err(
                "MODEL profile belongs to another person; shared compute approval required".into(),
            );
        }
        let state = tilo_peer_state::workspace_task::snapshot(
            view.peer()
                .admitted_room()
                .ok_or("MODEL start room absent")?,
        )?;
        let mut task = state
            .tasks
            .into_iter()
            .find(|task| task.id == task_id)
            .ok_or("MODEL start task absent")?;
        // Queueing strips the Chat request out of public task state. Recover
        // its exact record only from this candidate owner's private envelope.
        if let Some(request) = view.task_origin(task_id).or_else(|| {
            self.origin
                .as_ref()
                .filter(|(task, _)| task == task_id)
                .map(|(_, request)| request)
        }) {
            task.resource
                .as_mut()
                .ok_or("MODEL Chat origin resource absent")?
                .agent_request = Some(request.clone());
        }
        source_contract(
            &self.windows,
            &view.room().to_string(),
            &task,
            selected_model()?,
        )
    }
}

fn source_contract(
    windows: &[HostWindowContract],
    room: &str,
    task: &tilo_desktop_model::WorkspaceTask,
    selection: FrozenNativeModelSelection,
) -> Result<NativeModelContract, String> {
    // A task without an app origin is itself a genuine broker resource. Do not
    // invent an Agent window or select an unrelated foreground tab.
    let Some(resource) = task.resource.as_ref() else {
        return Ok(NativeModelContract::for_task(
            room,
            &task.id,
            ActionKind::Outside,
            selection,
        ));
    };
    let app = resource
        .app_id
        .as_deref()
        .ok_or("MODEL originating application absent")?;
    let record = resource
        .agent_request
        .as_ref()
        .map(|request| request.chat_record.as_str());
    if record == Some("") {
        return Err("MODEL originating Chat record absent".into());
    }
    let mut matches = windows.iter().filter(|window| {
        window.room == room
            && window.app == app
            && record.is_none_or(|record| window.record.as_deref() == Some(record))
    });
    let Some(window) = matches.next() else {
        return Ok(NativeModelContract::for_task(
            room,
            &task.id,
            ActionKind::Outside,
            selection,
        ));
    };
    if matches.next().is_some() {
        return Err("MODEL originating window ambiguous".into());
    }
    let mut selection = selection;
    selection
        .contract
        .as_object_mut()
        .ok_or("MODEL selection contract invalid")?
        .insert(
            "origin".into(),
            serde_json::to_value(window).map_err(|error| error.to_string())?,
        );
    Ok(NativeModelContract {
        window: window.window.clone(),
        request_model: window.request_model.clone(),
        kind: window.kind,
        selection,
    })
}

pub(super) fn selected_model() -> Result<FrozenNativeModelSelection, String> {
    // Only the installed committed source is authoritative; a disk mirror can
    // lag an account change or outlive removal of the Settings source.
    let source = settings_store::installed_source().ok_or("MODEL live Settings unavailable")?;
    let document = source
        .load_committed()
        .ok_or("MODEL committed Settings unavailable")?;
    let section = document
        .get(agent_provider_config::SETTINGS_SECTION)
        .and_then(serde_json::Value::as_object)
        .ok_or("MODEL provider Settings absent")?;
    let local = serde_json::from_value(
        section
            .get("config")
            .cloned()
            .ok_or("MODEL provider configuration absent")?,
    )
    .map_err(|_| "MODEL provider configuration invalid")?;
    let hosted = section
        .get("hosted")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| "MODEL hosted configuration invalid")?
        .unwrap_or_default();
    let config = ProviderConfig::from_stored(local, hosted).sanitised();
    if !config.validation_errors().is_empty() {
        return Err("MODEL provider configuration invalid".into());
    }
    let profile = config
        .active_profile()
        .ok_or("MODEL provider not selected")?;
    if !profile.enabled {
        return Err("MODEL selected provider disabled".into());
    }
    let model = profile
        .default_model
        .as_ref()
        .filter(|model| !model.is_empty())
        .ok_or("MODEL model not selected")?;
    Ok(FrozenNativeModelSelection {
        app: "so.tilo.agent".into(),
        provider: profile.id.clone(),
        model: model.clone(),
        // Full nonsecret account/options metadata, but no resolved HTTP endpoint:
        // transport preparation happens later and is not approval authority.
        contract: serde_json::json!({"profile": profile}),
    })
}
