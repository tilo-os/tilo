//! Person answers are resolved from the owner's admitted state, never a wire flag.
use super::*;
use tilo_desktop_model::WorkspaceTaskConfirmationState;

impl NativeTaskActionRequirement {
    /// The owner ledger consumes this asking reference atomically for the child UUID.
    /// It must additionally compare its request-time `confirmation_ceiling_digest`.
    /// # Errors
    /// Policy denies, the person has not answered, or exact approved arguments differ.
    pub fn confirmation_binding(
        &self,
        peer: &tilo_peer_state::StatePeer,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<Option<(String, uuid::Uuid)>, String> {
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            peer.admitted_room().ok_or("native room absent")?,
        )?;
        self.confirmation_binding_with_snapshot(peer, &snapshot, exact, now)
    }

    /// Resolve only the exact authenticated owner's answer; never a caller flag.
    /// # Errors
    /// Current policy, private draft or exact invocation differs.
    pub fn confirmation_binding_with_snapshot(
        &self,
        peer: &tilo_peer_state::StatePeer,
        snapshot: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
        exact: &crate::task_admission::NativeInvocationBinding,
        now: SystemTime,
    ) -> Result<Option<(String, uuid::Uuid)>, String> {
        let answer = self.check_exact(peer, exact, now)?;
        let invocation = self
            .invocation
            .as_ref()
            .ok_or("native exact invocation absent")?;
        if let Some(child) = &invocation.activation_for {
            if self.confirmation.is_some()
                || self.action != "open"
                || self.resource_kind != "applications.app"
                || self.resource != format!("app.{}", self.app)
                || self.need != tilo_actions::ActionNeed::of(tilo_actions::ActionKind::Undoable)
            {
                return Err("native activation does not name a required application open".into());
            }
            let child_invocation = child
                .invocation
                .as_ref()
                .ok_or("native activation child absent")?;
            let id =
                blake3::hash(format!("native-host-activation:{}", child_invocation.id).as_bytes());
            let activation = uuid::Uuid::from_bytes(
                *id.as_bytes()
                    .first_chunk::<16>()
                    .ok_or("native activation digest invalid")?,
            );
            if child_invocation.activation_for.is_some()
                || child_invocation.request != child_invocation.id
                || invocation.id != activation
                || invocation.request != child_invocation.id
                || self.app != child.app
                || digest(&self.binding)? != digest(&child.binding)?
                || invocation.provider != child_invocation.provider
                || invocation.arguments_digest != child_invocation.arguments_digest
                || invocation.private_run_digest != child_invocation.private_run_digest
            {
                return Err("native activation reviewed child changed".into());
            }
            let mut child_exact = exact.clone();
            child_exact.invocation = child_invocation.id;
            child_exact.request = child_invocation.request;
            child_exact.resource = child.resource.clone();
            child_exact.action = child.action.clone();
            child_exact.arguments_digest = child_invocation.arguments_digest;
            child_exact.contract_digest = child_invocation.contract_digest;
            child_exact.ceiling_digest = child.ceiling_digest()?;
            let approved =
                child.confirmation_binding_with_snapshot(peer, snapshot, &child_exact, now)?;
            if answer == Answer::Ask && approved.is_none() {
                return Err(self.waiting());
            }
            return Ok(approved);
        }
        let approved = self.person_confirmation(snapshot)?;
        if answer == Answer::Ask && approved.is_none() {
            return Err(self.waiting());
        }
        Ok(approved)
    }

    pub(super) fn person_confirmation(
        &self,
        state: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
    ) -> Result<Option<(String, uuid::Uuid)>, String> {
        let Some(reference) = self.confirmation.as_deref() else {
            return Ok(None);
        };
        let invocation = self
            .invocation
            .as_ref()
            .ok_or("native exact invocation absent")?;
        if invocation.id != invocation.request || invocation.activation_for.is_some() {
            return Err("native confirmation must bind one child invocation".into());
        }
        let run = self.binding.as_ref().ok_or("native task binding absent")?;
        let control = state
            .controls
            .get(&run.task_id)
            .ok_or("native task control absent")?;
        let approval = control
            .active_run
            .as_ref()
            .and_then(|run| run.approval.as_ref())
            .ok_or("native task starter absent")?;
        let held = control
            .confirmation
            .as_ref()
            .ok_or("native person answer absent")?;
        let asking: uuid::Uuid = reference
            .parse()
            .map_err(|_| "native confirmation reference is not an asking UUID")?;
        if held.asking != asking
            || held.asking.to_string() != reference
            || held.run_epoch != run.epoch
            || held.agent_id != run.agent_id
            || held.app != self.app
            || held.resource_kind != self.resource_kind
            || tilo_actions::selection::logical_resource_id(&held.resource)
                != tilo_actions::selection::logical_resource_id(&self.resource)
            || held.action != self.action
            || held.kind != self.need.kind
            || held.id
                != tilo_desktop_model::WorkspaceTaskConfirmation::id_for(
                    &tilo_actions::selection::logical_resource_id(&held.resource),
                    &held.action,
                    &held.arguments_digest,
                )
        {
            return Err("native person answer does not bind this exact action".into());
        }
        let WorkspaceTaskConfirmationState::Confirmed { person, edits, .. } = &held.state else {
            return Err(self.waiting());
        };
        if person != &approval.person {
            return Err("native confirmation was not answered by the run's starter".into());
        }
        let text = invocation.arguments.as_deref().unwrap_or("{}");
        let text = text
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .unwrap_or(text);
        let final_arguments: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(text)
                .map_err(|_| "native confirmed arguments are not an object")?;
        // Recover the original edited fields from the owner's draft, leaving all
        // other private fields intact. A redacted/partial draft need not reconstruct
        // the whole object: its ORIGINAL digest binds those unedited private values.
        let reverse: Vec<tilo_actions::DraftEdit> = edits
            .iter()
            .map(|edit| {
                let field = held
                    .draft
                    .fields
                    .iter()
                    .find(|field| field.path == edit.path)
                    .ok_or("native approved edit field absent")?;
                let text = match &field.value {
                    tilo_actions::DraftValue::Text { text }
                    | tilo_actions::DraftValue::Number { text } => text.clone(),
                    tilo_actions::DraftValue::Flag { on } => on.to_string(),
                    _ => return Err("native approved edit has no exact original value"),
                };
                Ok(tilo_actions::DraftEdit {
                    path: edit.path.clone(),
                    text,
                })
            })
            .collect::<Result<_, &str>>()?;
        let original = held.draft.apply_edits(&final_arguments, &reverse)?;
        if tilo_actions::draft::arguments_digest(&original) != held.arguments_digest
            || held.draft.apply_edits(&original, edits)? != final_arguments
        {
            return Err("native final arguments differ from the person's exact answer".into());
        }
        Ok(Some((reference.to_owned(), invocation.request)))
    }
}
