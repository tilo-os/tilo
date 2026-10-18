//! Durable owner-private mail. Nothing in this module mutates a room document.
use crate::{NativeWorkspaceOwner, RoomId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::SystemTime};
use tilo_actions::task_messaging::{
    TaskMessage, TaskMessageSend, TaskMessageStatus, TaskRunAddress,
};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskSnapshot};

const MAX_MESSAGES: usize = 4096;
const MAX_PAGE: usize = 4;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateTaskMailboxStore {
    version: u16,
    #[serde(default)]
    pub(crate) messages: BTreeMap<String, TaskMessage>,
    #[serde(default)]
    watermarks: BTreeMap<String, u64>,
    #[serde(default)]
    pub(crate) outputs: BTreeMap<String, tilo_actions::task_messaging::TaskOutputRef>,
    #[serde(default)]
    sequence: u64,
}
impl std::fmt::Debug for PrivateTaskMailboxStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateTaskMailboxStore { private: redacted }")
    }
}
impl Default for PrivateTaskMailboxStore {
    fn default() -> Self {
        Self {
            version: 1,
            messages: BTreeMap::new(),
            watermarks: BTreeMap::new(),
            outputs: BTreeMap::new(),
            sequence: 0,
        }
    }
}
impl PrivateTaskMailboxStore {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.messages.len() > MAX_MESSAGES
            || self.watermarks.len() > MAX_MESSAGES
        {
            return Err("invalid private task mailbox version or capacity".into());
        }
        crate::private_task_outputs::validate(&self.outputs)?;
        let mut sequences = std::collections::BTreeSet::new();
        for (id, message) in &self.messages {
            message.validate()?;
            if id != &message.message_id
                || message.sequence > self.sequence
                || !sequences.insert(message.sequence)
            {
                return Err("private task mailbox identity mismatch".into());
            }
        }
        for (run, watermark) in &self.watermarks {
            let address: TaskRunAddress = serde_json::from_str(run).map_err(|e| e.to_string())?;
            address.validate()?;
            if *watermark > self.sequence
                || serde_json::to_string(&address).map_err(|e| e.to_string())? != *run
                || (*watermark != 0
                    && !self.messages.values().any(|message| {
                        message.recipient == address && message.sequence == *watermark
                    }))
            {
                return Err(
                    "private task mailbox watermark lacks exact retained run custody".into(),
                );
            }
        }
        Ok(())
    }
    fn inbox(&self, address: &TaskRunAddress, unread: bool) -> Result<Vec<TaskMessage>, String> {
        self.validate()?;
        let key = serde_json::to_string(address).map_err(|e| e.to_string())?;
        let watermark = self.watermarks.get(&key).copied().unwrap_or_default();
        let mut messages: Vec<_> = self
            .messages
            .values()
            .filter(|message| {
                message.recipient == *address && (!unread || message.sequence > watermark)
            })
            .cloned()
            .collect();
        messages.sort_by_key(|message| message.sequence);
        if unread {
            messages.truncate(MAX_PAGE);
        }
        Ok(messages)
    }
}

pub(crate) fn address(
    snapshot: &WorkspaceTaskSnapshot,
    task_id: &str,
    epoch: u64,
) -> Result<TaskRunAddress, String> {
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("mailbox task absent")?;
    let control = snapshot
        .controls
        .get(task_id)
        .ok_or("mailbox task control absent")?;
    if control.run_epoch != epoch {
        return Err("mailbox run replaced".into());
    }
    Ok(TaskRunAddress {
        task_id: task_id.into(),
        run_epoch: epoch,
        agent_id: task
            .assigned_agent
            .clone()
            .ok_or("mailbox task actor absent")?,
    })
}

pub(crate) fn live(
    snapshot: &WorkspaceTaskSnapshot,
    address: &TaskRunAddress,
) -> Result<(), String> {
    let current = snapshot
        .tasks
        .iter()
        .find(|task| task.id == address.task_id)
        .ok_or("mailbox task absent")?;
    let run = snapshot
        .controls
        .get(&address.task_id)
        .and_then(|control| control.active_run.as_ref())
        .ok_or("mailbox task run ended")?;
    if current.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
        || run.epoch != address.run_epoch
        || run.agent_id != address.agent_id
        || current.assigned_agent.as_deref() != Some(address.agent_id.as_str())
    {
        return Err("mailbox task run stopped or replaced".into());
    }
    Ok(())
}

impl NativeWorkspaceOwner {
    /// Subscribe before reading an inbox. Notifications contain no contents or
    /// authority: every wake requires a fresh authorized owner observation.
    pub fn workspace_task_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.0.task_changes.subscribe()
    }

    /// Native caller custody and the exact published inbox capability are checked
    /// by `authorize` inside the same turn as Stop and durable owner publication.
    /// Never supply this callback from application arguments.
    ///
    /// # Errors
    /// Refuses stale run proofs, unauthorized recipients, conflicting IDs, or capacity exhaustion.
    pub fn send_task_message(
        &self,
        room: RoomId,
        source: TaskRunAddress,
        recipient_task: &str,
        invocation: uuid::Uuid,
        send: TaskMessageSend,
        authorize: impl FnOnce(&crate::NativeCommittedView) -> Result<(), String>,
    ) -> Result<TaskMessage, String> {
        send.validate()?;
        source.validate()?;
        if invocation.is_nil() {
            return Err("nil task message invocation".into());
        }
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| {
            authorize(view)?;
            let snapshot = tilo_peer_state::workspace_task::snapshot(
                view.peer().admitted_room().ok_or("mailbox room absent")?,
            )?;
            live(&snapshot, &source)?;
            let source_run = snapshot
                .controls
                .get(&source.task_id)
                .and_then(|control| control.active_run.as_ref())
                .ok_or("source task run ended")?;
            view.peer().workspace().check_task_execution_actor(
                &source.agent_id,
                source_run.approval.as_ref(),
                SystemTime::now(),
            )?;
            let recipient = address(&snapshot, recipient_task, send.run_epoch)?;
            live(&snapshot, &recipient)?;
            let run = snapshot
                .controls
                .get(&recipient.task_id)
                .and_then(|control| control.active_run.as_ref())
                .ok_or("recipient task run ended")?;
            view.peer().workspace().check_task_execution_actor(
                &recipient.agent_id,
                run.approval.as_ref(),
                SystemTime::now(),
            )?;
            Ok((view.clone(), recipient))
        })?;
        let store = &mut candidate.0.record.private_task_mailboxes;
        store.validate()?;
        let id = invocation.to_string();
        if let Some(old) = store.messages.get(&id) {
            if old.source != source || old.recipient != candidate.1 || old.text != send.text {
                return Err(
                    "task message invocation reused with different content or custody".into(),
                );
            }
            return Ok(old.clone());
        }
        if store.messages.len() >= MAX_MESSAGES {
            return Err("private task mailbox full; no unread mail was dropped".into());
        }
        store.sequence = store
            .sequence
            .checked_add(1)
            .ok_or("task message sequence exhausted")?;
        let message = TaskMessage {
            version: 1,
            message_id: id.clone(),
            source,
            recipient: candidate.1,
            received_at: i64::try_from(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_secs(),
            )
            .map_err(|e| e.to_string())?,
            sequence: store.sequence,
            text: send.text,
            status: TaskMessageStatus::Accepted,
        };
        store.messages.insert(id, message.clone());
        store.validate()?;
        candidate.0.record.generation = candidate
            .0
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_room(room, candidate.0, None)?;
        Ok(message)
    }

    /// Retain an exact-run observation cursor from the native runner.
    /// A runner report alone is not proof of model delivery: it never promotes
    /// Accepted to `DeliveredToModel` or permits pruning the retained body.
    ///
    /// # Errors
    /// Refuses stale recipient authority or messages outside the exact task inbox.
    pub fn acknowledge_task_messages(
        &self,
        room: RoomId,
        task_id: &str,
        epoch: u64,
        authority: &WorkspaceTaskAuthority,
        ids: &[String],
    ) -> Result<(), String> {
        if ids.is_empty()
            || ids.len() > MAX_PAGE
            || !matches!(authority, WorkspaceTaskAuthority::Agent { .. })
        {
            return Err("invalid native inbox acknowledgement".into());
        }
        let _turn = self.0.mutation.lock();
        self.workspace_task_private_instructions(
            room,
            task_id,
            epoch,
            authority,
            SystemTime::now(),
        )?;
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            candidate
                .peer()
                .admitted_room()
                .ok_or("mailbox room absent")?,
        )?;
        let address = address(&snapshot, task_id, epoch)?;
        live(&snapshot, &address)?;
        let store = &mut candidate.record.private_task_mailboxes;
        let key = serde_json::to_string(&address).map_err(|e| e.to_string())?;
        let prior = store.watermarks.get(&key).copied().unwrap_or_default();
        let mut sequence = 0;
        for id in ids {
            let message = store
                .messages
                .get(id)
                .ok_or("acknowledged task message absent")?;
            if message.recipient != address {
                return Err("task message acknowledgement is for another run".into());
            }
            sequence = sequence.max(message.sequence);
        }
        if store.messages.values().any(|message| {
            message.recipient == address
                && message.sequence <= sequence
                && message.sequence > prior
                && !ids.contains(&message.message_id)
        }) {
            return Err("task inbox acknowledgement skipped unincorporated mail".into());
        }
        // No DeliveredToModel transition is inferred from client-provided IDs.
        // A separately verified trusted prompt receipt is required for that.
        let watermark = store.watermarks.entry(key).or_default();
        *watermark = (*watermark).max(sequence);
        store.validate()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_room(room, candidate, None)
    }

    /// Human conversation reads and native inbox reads share exact-run custody,
    /// but never hydrate private data into a public workspace snapshot.
    ///
    /// # Errors
    /// Refuses callers without current authority over the exact task and epoch.
    pub fn task_messages(
        &self,
        room: RoomId,
        task_id: &str,
        epoch: u64,
        authority: &WorkspaceTaskAuthority,
        unread: bool,
    ) -> Result<Vec<TaskMessage>, String> {
        self.with_current(room, |view| {
            // Authorization and bodies must come from one committed generation;
            // pending durable mutations must not block this read-only observation.
            view.workspace_task_private_instructions(
                room,
                task_id,
                epoch,
                authority,
                SystemTime::now(),
            )?;
            let snapshot = tilo_peer_state::workspace_task::snapshot(
                view.peer().admitted_room().ok_or("mailbox room absent")?,
            )?;
            if matches!(authority, WorkspaceTaskAuthority::Person { .. }) && !unread {
                // Owner inspection is historical; never derive its actor from
                // today's assignment. Model/unread delivery stays live-fenced.
                let store = &view.record.private_task_mailboxes;
                store.validate()?;
                let mut messages: Vec<_> = store
                    .messages
                    .values()
                    .filter(|message| {
                        message.recipient.task_id == task_id && message.recipient.run_epoch == epoch
                    })
                    .cloned()
                    .collect();
                messages.sort_by_key(|message| message.sequence);
                return Ok(messages);
            }
            let address = address(&snapshot, task_id, epoch)?;
            view.record.private_task_mailboxes.inbox(&address, unread)
        })
    }
}
