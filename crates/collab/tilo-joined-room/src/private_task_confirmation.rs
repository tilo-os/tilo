//! Confirmation bodies belong to owner custody, not the replicated room.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use std::collections::BTreeMap;
use tilo_desktop_model::{
    WorkspaceTaskConfirmation as Confirmation, WorkspaceTaskConfirmationAnswer as Answer,
    WorkspaceTaskConfirmationState as State,
};
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand as Command, WorkspaceTaskSnapshot};

pub(crate) type Confirmations = BTreeMap<String, Confirmation>;
pub type PreparedConfirmation = (Command, Option<(String, Confirmation)>);

/// Only stable identity and state metadata may cross the room boundary.
pub(crate) fn public_confirmation(full: &Confirmation) -> Confirmation {
    let mut public = full.clone();
    public.draft = tilo_actions::Draft::default();
    match &mut public.state {
        State::Confirmed { edits, .. } => edits.clear(),
        State::Declined { reason, .. } => *reason = None,
        State::Waiting => {}
    }
    public
}

pub(crate) fn validate(values: &Confirmations) -> Result<(), String> {
    if values.len() > 4096 {
        return Err("private confirmation capacity exhausted".into());
    }
    for value in values.values() {
        if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 512 * 1024 {
            return Err("private confirmation exceeds storage limit".into());
        }
        if serde_json::to_vec(&value.draft)
            .map_err(|e| e.to_string())?
            .len()
            > 256 * 1024
        {
            return Err("private confirmation draft exceeds storage limit".into());
        }
        if let State::Confirmed { edits, .. } = &value.state {
            value.draft.check_edits(edits)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_archives(
    confirmations: &BTreeMap<String, Vec<Confirmation>>,
    audit: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<(), String> {
    if confirmations.len() > 4096 || audit.len() > 4096 {
        return Err("private confirmation archive capacity exhausted".into());
    }
    let count = confirmations.values().map(Vec::len).sum::<usize>();
    if count > 4096 || audit.values().map(Vec::len).sum::<usize>() > 16384 {
        return Err("private confirmation archive capacity exhausted".into());
    }
    for values in confirmations.values() {
        for full in values {
            validate(&BTreeMap::from([(String::new(), full.clone())]))?;
        }
    }
    if serde_json::to_vec(&(confirmations, audit))
        .map_err(|e| e.to_string())?
        .len()
        > 72 * 1024 * 1024
    {
        return Err("private confirmation archive exceeds capacity".into());
    }
    Ok(())
}

impl NativeCommittedView {
    /// Owner-local policy snapshot. Never publish this value or return it to an
    /// unauthenticated caller. Broker readers must authorize their run first.
    /// # Errors
    /// A live confirmation lacks exact owner custody or its public binding changed.
    pub fn task_policy_snapshot(&self) -> Result<WorkspaceTaskSnapshot, String> {
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("private task document absent")?,
        )?;
        let mut snapshot = PrivateTaskConfirmationStore::from_confirmations(
            self.record.private_task_text.confirmations.clone(),
        )?
        .hydrate(snapshot)?;
        self.hydrate_private_models(&mut snapshot)?;
        Ok(snapshot)
    }
}

impl NativeWorkspaceOwner {
    /// Read the exact private draft and answer through authenticated task custody.
    /// Authority must be supplied by transport, never by application arguments.
    /// # Errors
    /// Reader authorization fails, the run changed, or custody is unavailable.
    pub fn workspace_task_private_confirmation(
        &self,
        room: RoomId,
        task_id: &str,
        run_epoch: u64,
        authority: &tilo_peer_state::workspace_task::WorkspaceTaskAuthority,
        now: std::time::SystemTime,
    ) -> Result<Option<Confirmation>, String> {
        let generation = self.with_current(room, |view| Ok(view.generation()))?;
        // Reuse the exact certificate, current membership, host pin and run
        // authorization shared by the instruction door. No private text escapes.
        self.workspace_task_private_instructions(room, task_id, run_epoch, authority, now)?;
        self.with_current(room, |view| {
            if view.generation() != generation {
                return Err("private task authorization changed; retry the read".into());
            }
            let state = view.task_policy_snapshot()?;
            let held = state
                .controls
                .get(task_id)
                .and_then(|c| c.confirmation.clone());
            if held
                .as_ref()
                .is_some_and(|held| held.run_epoch != run_epoch)
            {
                return Err("private task confirmation belongs to another run".into());
            }
            Ok(held)
        })
    }
}

/// Owner-private confirmations paired with a scrubbed canonical task snapshot.
/// Never publish this store or a hydrated snapshot.
#[derive(Clone, Default)]
pub struct PrivateTaskConfirmationStore {
    values: BTreeMap<String, Confirmation>,
}

impl PrivateTaskConfirmationStore {
    /// Replication-safe identity and state metadata, without draft or answer bodies.
    pub fn public_confirmation(full: &Confirmation) -> Confirmation {
        public_confirmation(full)
    }

    /// Validate restored owner custody.
    /// # Errors
    /// Capacity or draft validation fails.
    pub fn from_confirmations(values: BTreeMap<String, Confirmation>) -> Result<Self, String> {
        validate(&values)?;
        Ok(Self { values })
    }

    /// Validate historical custody without promoting archives to live approvals.
    /// # Errors
    /// The archive exceeds capacity or contains invalid drafts.
    pub fn validate_archives(
        confirmations: &BTreeMap<String, Vec<Confirmation>>,
        audit: &BTreeMap<String, Vec<serde_json::Value>>,
    ) -> Result<(), String> {
        validate_archives(confirmations, audit)
    }

    /// Full owner-private values, for private persistence only.
    pub fn confirmations(&self) -> &BTreeMap<String, Confirmation> {
        &self.values
    }

    /// Read only confirmation metadata, retaining compatibility with older and
    /// future task-control fields that are not part of confirmation custody.
    /// # Errors
    /// A control or confirmation binding is malformed.
    pub fn document_bindings(
        doc: &automerge::AutoCommit,
    ) -> Result<BTreeMap<String, Confirmation>, String> {
        use automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
        let Some((Value::Object(ObjType::Map), map)) = doc
            .get(ROOT, tilo_peer_state::workspace_task::TASK_CONTROL_KEY)
            .map_err(|e| e.to_string())?
        else {
            return Ok(BTreeMap::new());
        };
        let mut bindings = BTreeMap::new();
        for task in doc.keys(&map) {
            let Some((Value::Scalar(value), _)) =
                doc.get(&map, task.as_str()).map_err(|e| e.to_string())?
            else {
                return Err("task control entry is not a scalar".into());
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                return Err("task control entry is not bytes".into());
            };
            let control: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
            if let Some(full) = control.get("confirmation").filter(|value| !value.is_null()) {
                bindings.insert(
                    task,
                    serde_json::from_value(full.clone()).map_err(|e| e.to_string())?,
                );
            }
        }
        Ok(bindings)
    }

    /// Require exact current custody without parsing unrelated legacy controls.
    /// # Errors
    /// A public confirmation is missing its exact private body.
    pub fn validate_document(&self, doc: &automerge::AutoCommit) -> Result<(), String> {
        for (task, public) in Self::document_bindings(doc)? {
            let full = self
                .values
                .get(&task)
                .ok_or("private task confirmation unavailable")?;
            if public_confirmation(full) != public {
                return Err("private task confirmation binding changed".into());
            }
        }
        Ok(())
    }

    /// Restore private bodies only when their complete public binding matches.
    /// # Errors
    /// A current confirmation lacks exact private custody.
    pub fn hydrate(
        &self,
        mut snapshot: WorkspaceTaskSnapshot,
    ) -> Result<WorkspaceTaskSnapshot, String> {
        for (task, control) in &mut snapshot.controls {
            if let Some(public) = &control.confirmation {
                let private = self
                    .values
                    .get(task)
                    .ok_or("private task confirmation unavailable")?;
                if public_confirmation(private) != *public {
                    return Err("private task confirmation binding changed".into());
                }
                control.confirmation = Some(private.clone());
            }
        }
        Ok(snapshot)
    }

    /// Scrub a confirmation before applying it to replicated state.
    /// # Errors
    /// Draft validation or exact current private custody fails.
    pub fn prepare(
        &self,
        snapshot: &WorkspaceTaskSnapshot,
        command: &Command,
    ) -> Result<PreparedConfirmation, String> {
        match command {
            Command::RequestConfirmation {
                task_id,
                run_epoch,
                confirmation,
            } => {
                let mut one = BTreeMap::new();
                one.insert(task_id.clone(), confirmation.as_ref().clone());
                validate(&one)?;
                // The reducer retains an existing answer for the same request. Never
                // replace its private draft with a later caller's version.
                let existing = snapshot
                    .controls
                    .get(task_id)
                    .and_then(|c| c.confirmation.as_ref());
                let write = if existing.is_some_and(|held| held.id == confirmation.id) {
                    let held = self
                        .values
                        .get(task_id)
                        .ok_or("private task confirmation unavailable")?;
                    if public_confirmation(held) != *existing.ok_or("confirmation absent")? {
                        return Err("private task confirmation binding changed".into());
                    }
                    None
                } else {
                    Some((task_id.clone(), confirmation.as_ref().clone()))
                };
                Ok((
                    Command::RequestConfirmation {
                        task_id: task_id.clone(),
                        run_epoch: *run_epoch,
                        confirmation: Box::new(public_confirmation(confirmation)),
                    },
                    write,
                ))
            }
            Command::AnswerConfirmation {
                task_id,
                run_epoch,
                confirmation_id,
                answer,
            } => {
                let snapshot = self.hydrate(snapshot.clone())?;
                let mut full = snapshot
                    .controls
                    .get(task_id)
                    .and_then(|c| c.confirmation.clone())
                    .filter(|held| held.id == *confirmation_id && held.run_epoch == *run_epoch)
                    .ok_or("private task confirmation was replaced")?;
                let public_answer = match answer {
                    Answer::Confirm { edits } => {
                        full.draft.check_edits(edits)?;
                        // Person and time are filled from the canonical reducer result.
                        full.state = State::Confirmed {
                            person: String::new(),
                            at: 0,
                            edits: edits.clone(),
                        };
                        Answer::Confirm { edits: Vec::new() }
                    }
                    Answer::Decline { reason } => {
                        let reason = reason
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned);
                        if reason.as_ref().is_some_and(|s| s.len() > 2048) {
                            return Err("private decline reason exceeds storage limit".into());
                        }
                        full.state = State::Declined {
                            person: String::new(),
                            at: 0,
                            reason,
                        };
                        Answer::Decline { reason: None }
                    }
                };
                Ok((
                    Command::AnswerConfirmation {
                        task_id: task_id.clone(),
                        run_epoch: *run_epoch,
                        confirmation_id: confirmation_id.clone(),
                        answer: public_answer,
                    },
                    Some((task_id.clone(), full)),
                ))
            }
            _ => Ok((command.clone(), None)),
        }
    }

    /// Bind private changes to the accepted canonical result and prune stale values.
    /// # Errors
    /// The accepted public binding differs from the private write.
    pub fn retain(
        &mut self,
        snapshot: &WorkspaceTaskSnapshot,
        write: Option<(String, Confirmation)>,
    ) -> Result<(), String> {
        if let Some((task, mut full)) = write {
            let public = snapshot
                .controls
                .get(&task)
                .and_then(|c| c.confirmation.as_ref())
                .ok_or("accepted private confirmation absent")?;
            match (&mut full.state, &public.state) {
                (
                    State::Confirmed { person, at, .. },
                    State::Confirmed {
                        person: p, at: t, ..
                    },
                )
                | (
                    State::Declined { person, at, .. },
                    State::Declined {
                        person: p, at: t, ..
                    },
                ) => {
                    *person = p.clone();
                    *at = *t;
                }
                _ => {}
            }
            if public_confirmation(&full) != *public {
                return Err("accepted private confirmation binding changed".into());
            }
            self.values.insert(task, full);
        }
        // Settlement, cancellation and run replacement all drop private answers too.
        self.values.retain(|task, full| {
            snapshot
                .controls
                .get(task)
                .and_then(|c| c.confirmation.as_ref())
                .is_some_and(|public| public_confirmation(full) == *public)
        });
        validate(&self.values)
    }
}

pub(crate) fn prepare(
    view: &NativeCommittedView,
    command: &Command,
) -> Result<PreparedConfirmation, String> {
    let snapshot = tilo_peer_state::workspace_task::snapshot(
        view.peer
            .admitted_room()
            .ok_or("private task document absent")?,
    )?;
    PrivateTaskConfirmationStore::from_confirmations(
        view.record.private_task_text.confirmations.clone(),
    )?
    .prepare(&snapshot, command)
}

pub(crate) fn retain(
    view: &mut NativeCommittedView,
    write: Option<(String, Confirmation)>,
) -> Result<(), String> {
    let snapshot = tilo_peer_state::workspace_task::snapshot(
        view.peer
            .admitted_room()
            .ok_or("private task document absent")?,
    )?;
    let mut store = PrivateTaskConfirmationStore::from_confirmations(
        view.record.private_task_text.confirmations.clone(),
    )?;
    store.retain(&snapshot, write)?;
    view.record.private_task_text.confirmations = store.values;
    Ok(())
}
