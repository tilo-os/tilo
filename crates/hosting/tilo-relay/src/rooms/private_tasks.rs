//! Governor-private task text and mail. Call only under the governed room lock.
//! The authenticated certificate, never a request's actor label, selects custody.
use super::GovernedRoom;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    path::Path,
};
use tilo_action_policy::{ActionKind, TreePath};
use tilo_actions::task_messaging::{TaskMessage, TaskMessageStatus, TaskRunAddress};
use tilo_peer_state::{
    MemberAuthority,
    workspace_task::{self, WorkspaceTaskSnapshot},
};
use tilo_relay_session::rooms::private::{PrivateOperation, PrivateReply};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

#[cfg(test)]
mod tests;

type MailboxChannels = BTreeMap<(std::path::PathBuf, String), tokio::sync::watch::Sender<()>>;

fn mailbox_channels() -> &'static std::sync::Mutex<MailboxChannels> {
    static CHANNELS: std::sync::OnceLock<std::sync::Mutex<MailboxChannels>> =
        std::sync::OnceLock::new();
    CHANNELS.get_or_init(Default::default)
}

const MAX_MESSAGES: usize = 4096;
const MAX_PAGE: usize = 4;
const MAX_FILE_BYTES: u64 = 72 * 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mailboxes {
    version: u16,
    room: String,
    sequence: u64,
    messages: BTreeMap<String, TaskMessage>,
    watermarks: BTreeMap<String, u64>,
    #[serde(default)]
    outputs: BTreeMap<String, Vec<tilo_actions::task_messaging::TaskOutputRef>>,
}

// Same schema as private_baseline's extraction file. This file is never served
// wholesale and is not part of a bootstrap, publication or public room snapshot.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskText {
    instruction: Option<String>,
    agent_request: Option<serde_json::Value>,
    directions: BTreeMap<u64, String>,
}

pub(super) fn read_private<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot open private task custody".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read private task custody")?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("private task custody exceeds capacity".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "invalid private task custody".into())
}

impl Mailboxes {
    #[cfg(test)]
    fn validate(&self, room: &str) -> Result<(), String> {
        self.validate_with_delivery(room, None)
    }

    fn validate_with_delivery(
        &self,
        room: &str,
        ledger: Option<&tilo_joined_room::task_admission::TaskAdmissionLedger>,
    ) -> Result<(), String> {
        if self.version != 1
            || self.room != room
            || self.messages.len() > MAX_MESSAGES
            || self.watermarks.len() > MAX_MESSAGES
            || self.outputs.values().map(Vec::len).sum::<usize>() > MAX_MESSAGES
        {
            return Err("invalid private task mailbox envelope".into());
        }
        for (key, outputs) in &self.outputs {
            let address: TaskRunAddress =
                serde_json::from_str(key).map_err(|_| "invalid output cursor")?;
            for output in outputs {
                output.validate()?;
                if output.task_run != address {
                    return Err("private output source changed".into());
                }
            }
        }
        let mut sequences = BTreeSet::new();
        for (id, message) in &self.messages {
            message.validate()?;
            if id != &message.message_id
                || message.sequence > self.sequence
                || message.received_at < 0
                || !sequences.insert(message.sequence)
                || (message.status == TaskMessageStatus::DeliveredToModel
                    && ledger.is_none_or(|ledger| !ledger.model_delivered_message(message)))
            {
                return Err("invalid retained private task message".into());
            }
        }
        for (key, watermark) in &self.watermarks {
            let address: TaskRunAddress =
                serde_json::from_str(key).map_err(|_| "invalid task mailbox cursor")?;
            address.validate()?;
            if serde_json::to_string(&address).map_err(|e| e.to_string())? != *key
                || *watermark > self.sequence
                || (*watermark != 0
                    && !self.messages.values().any(|message| {
                        message.recipient == address && message.sequence == *watermark
                    }))
            {
                return Err("private task cursor lacks retained run custody".into());
            }
        }
        Ok(())
    }
}

fn address(
    state: &WorkspaceTaskSnapshot,
    task: &str,
    epoch: u64,
) -> Result<TaskRunAddress, String> {
    let task = state
        .tasks
        .iter()
        .find(|t| t.id == task)
        .ok_or("private task absent")?;
    let control = state
        .controls
        .get(&task.id)
        .ok_or("private task control absent")?;
    if control.run_epoch != epoch {
        return Err("private task epoch replaced".into());
    }
    let address = TaskRunAddress {
        task_id: task.id.clone(),
        run_epoch: epoch,
        agent_id: task
            .assigned_agent
            .clone()
            .ok_or("private task actor absent")?,
    };
    address.validate()?;
    Ok(address)
}

type TaskCaller = (MemberAuthority, Option<String>);
type AuthorizedTaskOperation = (TaskCaller, PrivateOperation, Option<TaskRunAddress>);

struct TaskCommand<'a> {
    state: &'a WorkspaceTaskSnapshot,
    caller: &'a TaskCaller,
    source: &'a Option<TaskRunAddress>,
    now: SystemTime,
}

impl GovernedRoom {
    /// Subscribe under the same room lock as authorization and cursor inspection.
    pub(super) fn private_task_watch(
        &self,
        caller: &DeviceCert,
        operation: PrivateOperation,
    ) -> Result<tokio::sync::watch::Receiver<()>, String> {
        let PrivateOperation::NativeTaskRead {
            task,
            epoch,
            agent,
            operation: inner,
            ..
        } = &operation
        else {
            return Err("task mailbox watch requires native host custody".into());
        };
        if !matches!(inner.as_ref(), PrivateOperation::TaskMailboxWatch { .. }) {
            return Err("not a private task mailbox watch".into());
        }
        self.private_task_command(caller, operation.clone())?;
        let key = serde_json::to_string(&TaskRunAddress {
            task_id: task.clone(),
            run_epoch: *epoch,
            agent_id: agent.clone(),
        })
        .map_err(|_| "invalid private task watch address")?;
        let mut channels = mailbox_channels()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        channels.retain(|_, sender| sender.receiver_count() > 0);
        Ok(channels
            .entry((self.path.clone(), key))
            .or_insert_with(|| tokio::sync::watch::channel(()).0)
            .subscribe())
    }

    fn task_caller(
        &self,
        caller: &DeviceCert,
        now: SystemTime,
    ) -> Result<(MemberAuthority, Option<String>), String> {
        if !self.peer.certificates().iter().any(|cert| cert == caller) {
            return Err("private task caller certificate is not current".into());
        }
        let member = self.peer.permissions(caller.device_public_key, now)?;
        let agent = if member.delegate.is_some() {
            let (cert, delegation, withdrawn) = self
                .peer
                .governance()
                .governed_agent(&caller.device_public_key)
                .ok_or("private task caller is not a governed agent")?;
            if cert != *caller || withdrawn.is_some() {
                return Err("private task agent certificate replaced or withdrawn".into());
            }
            // permissions above verifies this device's live signed delegation,
            // revocation, room scope and membership; the id comes from that row.
            Some(
                delegation
                    .delegate
                    .agent_id()
                    .ok_or("private task delegate is not an agent")?
                    .to_owned(),
            )
        } else {
            if member.person != hex::encode(caller.user_public_key) {
                return Err("private task reader is not a person".into());
            }
            None
        };
        Ok((member, agent))
    }

    fn live_task(
        &self,
        state: &WorkspaceTaskSnapshot,
        address: &TaskRunAddress,
        now: SystemTime,
    ) -> Result<(), String> {
        let task = state
            .tasks
            .iter()
            .find(|t| t.id == address.task_id)
            .ok_or("private task absent")?;
        let control = state
            .controls
            .get(&address.task_id)
            .ok_or("private task control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("private task run ended")?;
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
            || control.run_epoch != address.run_epoch
            || run.epoch != address.run_epoch
            || run.agent_id != address.agent_id
            || task.assigned_agent.as_deref() != Some(address.agent_id.as_str())
        {
            return Err("private task run stopped or replaced".into());
        }
        let approval = run.approval.as_ref().ok_or("private task starter absent")?;
        self.peer
            .workspace()
            .check_task_execution_actor(&address.agent_id, Some(approval), now)
    }

    fn authorize_task_read(
        &self,
        state: &WorkspaceTaskSnapshot,
        task: &str,
        epoch: u64,
        caller: &(MemberAuthority, Option<String>),
        now: SystemTime,
    ) -> Result<(), String> {
        let entry = state
            .tasks
            .iter()
            .find(|t| t.id == task)
            .ok_or("private task absent")?;
        let control = state
            .controls
            .get(task)
            .ok_or("private task control absent")?;
        if epoch > control.run_epoch {
            return Err("private task epoch has not existed".into());
        }
        if let Some(agent) = &caller.1 {
            let address = address(state, task, epoch)?;
            if address.agent_id != *agent {
                return Err("private task belongs to another agent".into());
            }
            self.live_task(state, &address, now)
        } else {
            let starter = control
                .active_run
                .as_ref()
                .and_then(|run| run.approval.as_ref())
                .filter(|_| control.run_epoch == epoch)
                .is_some_and(|approval| approval.person == caller.0.person)
                || entry.audit.iter().any(|event| {
                    event.actor == caller.0.person
                        && event.action() == "approve-run"
                        && matches!(&event.what, tilo_actions::Did::Verb(verb)
                            if verb.args.get("run") == tilo_actions::Scalar::of(&epoch).as_ref())
                });
            if (entry.created_by != caller.0.person && !starter)
                || !caller
                    .0
                    .allows(&TreePath::room(self.room()), ActionKind::Undoable, now)
            {
                return Err("private task reader is not its authorized person".into());
            }
            Ok(())
        }
    }

    fn load_task_mail(&self) -> Result<Mailboxes, String> {
        let store = read_private(&self.path.with_extension("private-task-mailboxes-v1.json"))?
            .unwrap_or_else(|| Mailboxes {
                version: 1,
                room: self.room().into(),
                ..Default::default()
            });
        let ledger = self.load_native_admission()?;
        store.validate_with_delivery(self.room(), Some(&ledger))?;
        Ok(store)
    }

    pub(super) fn persist_model_delivery(
        &self,
        receipt: &tilo_joined_room::task_admission::model_delivery::NativeModelDeliveryReceipt,
    ) -> Result<(), String> {
        let mut store = self.load_task_mail()?;
        receipt.apply_to_messages(&mut store.messages)?;
        self.persist_task_mail(&store)
    }

    fn persist_task_mail(&self, store: &Mailboxes) -> Result<(), String> {
        let ledger = self.load_native_admission()?;
        store.validate_with_delivery(self.room(), Some(&ledger))?;
        let bytes = serde_json::to_vec(store).map_err(|_| "cannot encode private task mailbox")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("private task mailbox exceeds capacity".into());
        }
        tilo_files::native::atomic::write_private(
            &self.path.with_extension("private-task-mailboxes-v1.json"),
            &bytes,
        )
        .map_err(|_| "cannot persist private task mailbox".into())
    }

    /// Private operations are serialized with Stop and room membership changes.
    /// The endpoint must authenticate possession of `caller`'s device key first.
    pub(super) fn private_task_command(
        &self,
        caller: &DeviceCert,
        op: PrivateOperation,
    ) -> Result<PrivateReply, String> {
        let now = SystemTime::now();
        let authenticated = self.task_caller(caller, now)?;
        let (caller, op, source) = self.normalize_task_operation(caller, op, authenticated, now)?;
        let state = workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
        if let Some(source) = &source {
            self.live_task(&state, source, now)?;
        }
        let cx = TaskCommand {
            state: &state,
            caller: &caller,
            source: &source,
            now,
        };
        match op {
            PrivateOperation::TaskMailboxWatch { task, epoch, .. } => {
                let source = source
                    .as_ref()
                    .ok_or("task mailbox watch requires physical host run custody")?;
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                let sequence = self
                    .load_task_mail()?
                    .messages
                    .values()
                    .filter(|message| message.recipient == *source)
                    .map(|message| message.sequence)
                    .max()
                    .unwrap_or(0);
                Ok(PrivateReply::Changed(sequence))
            }
            PrivateOperation::TaskModelContract {
                task,
                epoch,
                revision,
            } => {
                let source = source
                    .as_ref()
                    .ok_or("MODEL contract requires physical host run custody")?;
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                let control = state
                    .controls
                    .get(&task)
                    .ok_or("MODEL task control absent")?;
                let run = control.active_run.as_ref().ok_or("MODEL run ended")?;
                let public = control
                    .model_authority
                    .as_ref()
                    .ok_or("MODEL approval absent")?;
                let models: BTreeMap<
                    String,
                    tilo_desktop_model::task::WorkspaceTaskModelAuthority,
                > = read_private(
                    &self
                        .path
                        .with_extension("private-task-model-authority-v1.json"),
                )?
                .unwrap_or_default();
                let full = models.get(&task).ok_or("MODEL private contract absent")?;
                let mut bound = full.clone();
                bound.contract = workspace_task::private_model_contract_reference(&full.contract)?;
                if &bound != public
                    || full.epoch != epoch
                    || run.epoch != epoch
                    || run.agent_id != source.agent_id
                    || run.applied_direction_revision != revision
                    || control
                        .directions
                        .last()
                        .map_or(0, |direction| direction.revision)
                        != revision
                    || run
                        .approval
                        .as_ref()
                        .is_none_or(|approval| approval.person != full.starter.person)
                {
                    return Err("MODEL private contract run binding changed".into());
                }
                let contract: tilo_joined_room::task_admission::model::NativeModelContract =
                    serde_json::from_value(full.contract.clone()).map_err(|e| e.to_string())?;
                contract.source_for(self.room(), &task)?;
                if contract.request_model != full.request_model || contract.kind != full.kind {
                    return Err("MODEL private contract policy binding changed".into());
                }
                Ok(PrivateReply::TaskModelContract(
                    serde_json::to_string(&contract).map_err(|e| e.to_string())?,
                ))
            }
            PrivateOperation::TaskConfirmationInvocation {
                task,
                epoch,
                revision,
                asking,
            } => {
                let source = source
                    .as_ref()
                    .ok_or("native confirmation requires exact source custody")?;
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                let snapshot = self.task_policy_snapshot()?;
                let starter = snapshot
                    .controls
                    .get(&task)
                    .and_then(|c| c.active_run.as_ref())
                    .and_then(|r| r.approval.as_ref())
                    .ok_or("native confirmation starter absent")?;
                let binding = tilo_joined_room::task_policy::NativeTaskBinding {
                    task_id: task,
                    agent_id: source.agent_id.clone(),
                    epoch,
                    revision,
                    starter: Some(starter.person.clone()),
                };
                Ok(PrivateReply::TaskConfirmationInvocation(
                    self.load_native_admission()?
                        .frozen_confirmation_invocation(&snapshot, &binding, asking)?,
                ))
            }
            PrivateOperation::TaskRunDigest {
                task,
                epoch,
                revision,
            } => {
                let source = source
                    .as_ref()
                    .ok_or("private run digest requires native source custody")?;
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                // Both inputs are read under the same governed-room writer lock.
                let snapshot = self.task_policy_snapshot()?;
                let run = snapshot
                    .controls
                    .get(&task)
                    .and_then(|control| control.active_run.as_ref())
                    .ok_or("private run ended")?;
                let starter = run.approval.as_ref().ok_or("private run starter absent")?;
                let binding = tilo_joined_room::task_policy::NativeTaskBinding {
                    task_id: task.clone(),
                    agent_id: source.agent_id.clone(),
                    epoch,
                    revision,
                    starter: Some(starter.person.clone()),
                };
                let PrivateReply::TaskInstructions(encoded) =
                    self.task_instructions(&cx, &task, epoch)?
                else {
                    return Err("unexpected private instruction reply".into());
                };
                let private: tilo_joined_room::NativePrivateTaskInstructions =
                    serde_json::from_str(&encoded)
                        .map_err(|_| "invalid private task instructions")?;
                let digest = tilo_joined_room::task_admission::private_run::private_run_digest(
                    tilo_desktop_model::RoomId::parse(self.room()).ok_or("invalid native room")?,
                    &snapshot,
                    &binding,
                    &private,
                )?;
                Ok(PrivateReply::TaskRunDigest(digest))
            }
            PrivateOperation::TaskConfirmation { task, epoch } => {
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                let hydrated = self.task_policy_snapshot()?;
                let confirmation = hydrated
                    .controls
                    .get(&task)
                    .and_then(|control| control.confirmation.as_ref());
                if confirmation.is_some_and(|held| held.run_epoch != epoch) {
                    return Err("private task confirmation belongs to another run".into());
                }
                let encoded = confirmation
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                Ok(PrivateReply::TaskConfirmation(encoded))
            }
            PrivateOperation::TaskReportTranscript { task, epoch } => {
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                let mut reports: Vec<_> = self
                    .load_task_reports()?
                    .into_values()
                    .filter(|report| {
                        report.task_run.task_id == task && report.task_run.run_epoch == epoch
                    })
                    .collect();
                reports.sort_by_key(|report| report.sequence);
                Ok(PrivateReply::TaskReportTranscript(reports))
            }
            PrivateOperation::TaskOutputs { task, epoch } => {
                self.authorize_task_read(&state, &task, epoch, &caller, now)?;
                let outputs = self
                    .load_task_mail()?
                    .outputs
                    .into_values()
                    .flatten()
                    .filter(|output| {
                        output.task_run.task_id == task && output.task_run.run_epoch == epoch
                    })
                    .collect();
                Ok(PrivateReply::TaskOutputs(outputs))
            }
            PrivateOperation::TaskRecordOutputs {
                task,
                epoch,
                outputs,
            } => self.task_record_outputs(&cx, &task, epoch, outputs),
            PrivateOperation::TaskInstructions { task, epoch } => {
                self.task_instructions(&cx, &task, epoch)
            }
            PrivateOperation::TaskMessages { task, epoch } => self.task_messages(&cx, &task, epoch),
            PrivateOperation::TaskAcknowledge { task, epoch, ids } => {
                self.task_acknowledge(&cx, &task, epoch, &ids)
            }
            PrivateOperation::TaskSend {
                recipient,
                invocation,
                send,
            } => self.task_send(&cx, &recipient, invocation, send),
            _ => Err("not a private task operation".into()),
        }
    }

    fn normalize_task_operation(
        &self,
        caller: &DeviceCert,
        op: PrivateOperation,
        authenticated: TaskCaller,
        now: SystemTime,
    ) -> Result<AuthorizedTaskOperation, String> {
        if let PrivateOperation::NativeTaskRead {
            task,
            epoch,
            agent,
            evidence,
            operation,
        } = op
        {
            self.native_host(caller)?;
            let source = TaskRunAddress {
                task_id: task,
                run_epoch: epoch,
                agent_id: agent.clone(),
            };
            self.validate_native_task_operation(caller, &source, &evidence, &operation, now)?;
            Ok(((authenticated.0, Some(agent)), *operation, Some(source)))
        } else if matches!(
            op,
            PrivateOperation::TaskSend { .. }
                | PrivateOperation::TaskRecordOutputs { .. }
                | PrivateOperation::TaskMailboxWatch { .. }
        ) {
            Err("private task effects require exact native dispatch custody".into())
        } else {
            Ok((authenticated, op, None))
        }
    }

    fn validate_native_task_operation(
        &self,
        caller: &DeviceCert,
        source: &TaskRunAddress,
        evidence: &[u8],
        operation: &PrivateOperation,
        now: SystemTime,
    ) -> Result<(), String> {
        let task = source.task_id.as_str();
        let epoch = source.run_epoch;
        let agent = source.agent_id.as_str();
        if let PrivateOperation::TaskSend {
            recipient,
            invocation,
            send,
        } = operation
        {
            let requirement: tilo_joined_room::task_policy::NativeTaskActionRequirement =
                serde_json::from_slice(evidence).map_err(|_| "native task contract absent")?;
            let bound = requirement
                .binding
                .as_ref()
                .ok_or("native inbox source absent")?;
            if bound.task_id != task
                || bound.epoch != epoch
                || bound.agent_id != agent
                || requirement.resource != format!("desktop:{recipient}")
                || requirement.app != "so.tilo.desktop"
                || requirement.resource_kind != "workspace-task"
                || requirement.action != "send-message"
                || requirement.need.field.as_deref() != Some("inbox")
                || requirement
                    .invocation
                    .as_ref()
                    .is_none_or(|exact| exact.id != *invocation)
            {
                return Err("native inbox contract or source mismatch".into());
            }
            requirement.check_task_message_payload(recipient, *invocation, send)?;
            let ledger = self.load_native_admission()?;
            let exact = ledger.dispatched_invocation(*invocation, caller.device_public_key)?;
            ledger.check_retained_action(&self.peer, &requirement, exact, now)?;
        } else if let PrivateOperation::TaskRecordOutputs {
            task: target,
            epoch: run,
            outputs,
        } = operation
        {
            if target != task || *run != epoch {
                return Err("native output source changed".into());
            }
            let requirement: tilo_joined_room::task_policy::NativeTaskActionRequirement =
                serde_json::from_slice(evidence).map_err(|_| "native output contract absent")?;
            let bound = requirement
                .binding
                .as_ref()
                .ok_or("native output binding absent")?;
            if bound.task_id != task || bound.epoch != epoch || bound.agent_id != agent {
                return Err("native output binding changed".into());
            }
            let ledger = self.load_native_admission()?;
            for output in outputs {
                output.validate()?;
                let id = uuid::Uuid::parse_str(&output.invocation)
                    .map_err(|_| "invalid native output invocation")?;
                let exact = ledger.dispatched_invocation(id, caller.device_public_key)?;
                ledger.check_retained_action(&self.peer, &requirement, exact, now)?;
                if exact.task != task
                    || exact.epoch != epoch
                    || exact.agent != agent
                    || exact.ceiling_digest != requirement.ceiling_digest()?
                {
                    return Err("native output lacks original source custody".into());
                }
            }
        } else if !matches!(operation, PrivateOperation::TaskConfirmationInvocation { task: target, epoch: run, .. } | PrivateOperation::TaskRunDigest { task: target, epoch: run, .. } | PrivateOperation::TaskModelContract { task: target, epoch: run, .. } | PrivateOperation::TaskConfirmation { task: target, epoch: run } | PrivateOperation::TaskInstructions { task: target, epoch: run } | PrivateOperation::TaskMessages { task: target, epoch: run } | PrivateOperation::TaskMailboxWatch { task: target, epoch: run, .. } | PrivateOperation::TaskAcknowledge { task: target, epoch: run, .. } | PrivateOperation::TaskOutputs { task: target, epoch: run } | PrivateOperation::TaskReportTranscript { task: target, epoch: run } if target == task && *run == epoch)
        {
            return Err("native private read operation binding mismatch".into());
        }
        Ok(())
    }

    fn task_record_outputs(
        &self,
        cx: &TaskCommand<'_>,
        task: &str,
        epoch: u64,
        outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
    ) -> Result<PrivateReply, String> {
        let state = cx.state;
        let caller = cx.caller;
        let source = cx.source;
        let now = cx.now;
        if source.is_none() {
            return Err("task output requires native source custody".into());
        }
        self.authorize_task_read(state, task, epoch, caller, now)?;
        let source = address(state, task, epoch)?;
        let key = serde_json::to_string(&source).map_err(|_| "invalid task output source")?;
        let mut store = self.load_task_mail()?;
        let retained = store.outputs.entry(key).or_default();
        for output in outputs {
            output.validate()?;
            if output.task_run != source {
                return Err("native output source changed".into());
            }
            if !retained.contains(&output) {
                retained.push(output);
            }
        }
        self.persist_task_mail(&store)?;
        Ok(PrivateReply::Done)
    }

    fn task_instructions(
        &self,
        cx: &TaskCommand<'_>,
        task: &str,
        epoch: u64,
    ) -> Result<PrivateReply, String> {
        let state = cx.state;
        let caller = cx.caller;
        let now = cx.now;
        let control = state.controls.get(task).ok_or("private task absent")?;
        self.authorize_task_read(state, task, epoch, caller, now)?;
        let mut texts: BTreeMap<String, TaskText> =
            read_private(&self.path.with_extension("private-task-text-v1.json"))?
                .unwrap_or_default();
        let text = texts
            .remove(task)
            .ok_or("private task instructions unavailable")?;
        let instruction = text.instruction.as_deref().or_else(|| {
            text.agent_request
                .as_ref()
                .and_then(|r| r.get("instruction"))
                .and_then(serde_json::Value::as_str)
        });
        let reference = control
            .private_instruction_ref
            .as_ref()
            .ok_or("private task instruction binding absent")?;
        if instruction
            .is_none_or(|text| workspace_task::private_instruction_reference(text) != *reference)
            || text
                .agent_request
                .as_ref()
                .and_then(|r| r.get("instruction"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| {
                    workspace_task::private_instruction_reference(text) != *reference
                })
        {
            return Err("private task instruction binding mismatch".into());
        }
        for direction in &control.directions {
            if text.directions.get(&direction.revision).is_none_or(|text| {
                workspace_task::private_instruction_reference(text) != direction.text
            }) {
                return Err("private task direction binding mismatch".into());
            }
        }
        let encoded =
            serde_json::to_string(&text).map_err(|_| "cannot encode private task instructions")?;
        Ok(PrivateReply::TaskInstructions(encoded))
    }

    fn task_messages(
        &self,
        cx: &TaskCommand<'_>,
        task: &str,
        epoch: u64,
    ) -> Result<PrivateReply, String> {
        let state = cx.state;
        let caller = cx.caller;
        let now = cx.now;
        self.authorize_task_read(state, task, epoch, caller, now)?;
        let store = self.load_task_mail()?;
        let watermark = if caller.1.is_some() {
            let address = address(state, task, epoch)?;
            let key = serde_json::to_string(&address).map_err(|e| e.to_string())?;
            store.watermarks.get(&key).copied().unwrap_or_default()
        } else {
            0
        };
        let mut messages: Vec<_> = store
            .messages
            .values()
            .filter(|m| {
                m.recipient.task_id == task
                    && m.recipient.run_epoch == epoch
                    && (caller.1.is_none() || m.sequence > watermark)
            })
            .cloned()
            .collect();
        messages.sort_by_key(|m| m.sequence);
        if caller.1.is_some() {
            messages.truncate(MAX_PAGE);
        }
        Ok(PrivateReply::TaskMessages(messages))
    }

    fn task_acknowledge(
        &self,
        cx: &TaskCommand<'_>,
        task: &str,
        epoch: u64,
        ids: &[String],
    ) -> Result<PrivateReply, String> {
        let state = cx.state;
        let caller = cx.caller;
        let now = cx.now;
        if caller.1.is_none() || ids.is_empty() || ids.len() > MAX_PAGE {
            return Err("invalid private task acknowledgement".into());
        }
        self.authorize_task_read(state, task, epoch, caller, now)?;
        let address = address(state, task, epoch)?;
        let key = serde_json::to_string(&address).map_err(|e| e.to_string())?;
        let mut store = self.load_task_mail()?;
        let prior = store.watermarks.get(&key).copied().unwrap_or_default();
        let mut sequence = 0;
        for id in ids {
            let message = store
                .messages
                .get(id)
                .ok_or("acknowledged private task message absent")?;
            if message.recipient != address {
                return Err("private task acknowledgement has different custody".into());
            }
            sequence = sequence.max(message.sequence);
        }
        if store.messages.values().any(|m| {
            m.recipient == address
                && m.sequence > prior
                && m.sequence <= sequence
                && !ids.contains(&m.message_id)
        }) {
            return Err("private task acknowledgement skipped mail".into());
        }
        // A client cursor is not evidence of model delivery. Retain all
        // bodies and never promote Accepted to DeliveredToModel.
        store.watermarks.insert(key, prior.max(sequence));
        self.persist_task_mail(&store)?;
        Ok(PrivateReply::Done)
    }

    fn task_send_source(cx: &TaskCommand<'_>) -> Result<TaskRunAddress, String> {
        let state = cx.state;
        let caller = cx.caller;
        let source = cx.source;
        let agent = caller
            .1
            .as_ref()
            .ok_or("private task sender must be an authenticated agent")?;
        // The wire carries no source task claim. Refuse ambiguity rather
        // than allowing a device to choose another run's identity.
        let sources: Vec<_> = state
            .tasks
            .iter()
            .filter(|task| {
                task.assigned_agent.as_deref() == Some(agent.as_str())
                    && task.status == tilo_desktop_model::WorkspaceTaskStatus::InProgress
                    && state
                        .controls
                        .get(&task.id)
                        .is_some_and(|c| c.active_run.is_some())
            })
            .collect();
        let task = if let Some(source) = source {
            state
                .tasks
                .iter()
                .find(|task| task.id == source.task_id)
                .ok_or("native task source absent")?
        } else {
            match sources.as_slice() {
                [task] => *task,
                _ => return Err("private task sender needs exactly one active run".into()),
            }
        };
        let control = state
            .controls
            .get(&task.id)
            .ok_or("private task sender control absent")?;
        address(state, &task.id, control.run_epoch)
    }

    fn authorize_task_send(
        &self,
        command: &TaskCommand<'_>,
        recipient: &str,
        epoch: u64,
    ) -> Result<(TaskRunAddress, TaskRunAddress), String> {
        let state = command.state;
        let now = command.now;
        let source = Self::task_send_source(command)?;
        let control = state
            .controls
            .get(&source.task_id)
            .ok_or("private task sender control absent")?;
        self.live_task(state, &source, now)?;
        let recipient = address(state, recipient, epoch)?;
        self.live_task(state, &recipient, now)?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("private task sender run ended")?;
        if control.directions.last().map_or(0, |d| d.revision) != run.applied_direction_revision {
            return Err("private task sender instructions changed".into());
        }
        // normalize_task_operation already verified this exact native effect's
        // payload and dispatched ledger custody under this same writer lock.
        // Requiring standalone agent Allow here would erase starter delegation
        // and the person's genuinely admitted Ask answer.
        if command.source.is_none() {
            return Err("private task send has no exact dispatched source".into());
        }
        Ok((source, recipient))
    }

    fn task_send(
        &self,
        cx: &TaskCommand<'_>,
        recipient: &str,
        invocation: uuid::Uuid,
        send: tilo_actions::task_messaging::TaskMessageSend,
    ) -> Result<PrivateReply, String> {
        let now = cx.now;
        send.validate()?;
        if invocation.is_nil() {
            return Err("nil private task invocation".into());
        }
        let (source, recipient) = self.authorize_task_send(cx, recipient, send.run_epoch)?;
        let mut store = self.load_task_mail()?;
        let id = invocation.to_string();
        if let Some(old) = store.messages.get(&id) {
            if old.source != source || old.recipient != recipient || old.text != send.text {
                return Err(
                    "private task invocation reused with different custody or content".into(),
                );
            }
            return Ok(PrivateReply::TaskSent(old.clone()));
        }
        if store.messages.len() >= MAX_MESSAGES {
            return Err("private task mailbox full; no mail dropped".into());
        }
        store.sequence = store
            .sequence
            .checked_add(1)
            .ok_or("private task sequence exhausted")?;
        let message = TaskMessage {
            version: 1,
            message_id: id.clone(),
            source,
            recipient,
            received_at: i64::try_from(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .map_err(|_| "private task clock precedes epoch")?
                    .as_secs(),
            )
            .map_err(|_| "private task clock overflow")?,
            sequence: store.sequence,
            text: send.text,
            status: TaskMessageStatus::Accepted,
        };
        store.messages.insert(id, message.clone());
        self.persist_task_mail(&store)?;
        // Notify only after durable acceptance, only this recipient run, and never
        // with text. Retries return the retained receipt above without a new wake.
        let key = serde_json::to_string(&message.recipient)
            .map_err(|_| "invalid private task recipient")?;
        if let Some(sender) = mailbox_channels()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(self.path.clone(), key))
        {
            sender.send_replace(());
        }
        Ok(PrivateReply::TaskSent(message))
    }
}
