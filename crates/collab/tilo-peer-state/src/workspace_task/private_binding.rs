//! Bind private custody only on an unpublished, accepted owner candidate.
use super::*;

pub(crate) fn bind_private_instruction(
    room: &AutoCommit,
    result: &WorkspaceTaskMutationResult,
    reference: &str,
) -> Result<AutoCommit, String> {
    let digest = reference
        .strip_prefix("private-instruction:")
        .ok_or("invalid private instruction reference")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid private instruction reference".into());
    }
    let WorkspaceTaskMutationResult::Queued { task_id } = result else {
        return Err("private instruction requires an accepted queue result".into());
    };
    let mut controls = controls_of(room)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("private task control absent")?;
    if !control
        .receipts
        .iter()
        .any(|receipt| &receipt.result == result)
    {
        return Err("private instruction queue result was not retained".into());
    }
    let task = tilo_desktop_model::projection::workspace_tasks_of(room)
        .into_iter()
        .find(|task| &task.id == task_id)
        .ok_or("private task absent")?;
    if task.route == WorkspaceTaskRoute::Manual
        || control
            .instruction
            .as_deref()
            .is_some_and(|text| text != reference)
        || task
            .resource
            .as_ref()
            .is_some_and(|resource| resource.agent_request.is_some())
    {
        return Err("private instruction requires sanitized managed task custody".into());
    }
    if let Some(old) = &control.private_instruction_ref {
        if old != reference {
            return Err("private instruction binding changed".into());
        }
        if control.instruction.is_none() {
            return Ok(room.clone());
        }
    }
    if control.private_instruction_ref.is_none()
        && (control.run_epoch != 0
            || control.active_run.is_some()
            || !control.directions.is_empty()
            || task.status != WorkspaceTaskStatus::Queued)
    {
        return Err("private instruction queue frontier changed before binding".into());
    }
    control.instruction = None;
    control.private_instruction_ref = Some(reference.to_owned());
    let mut candidate = room.clone();
    project_controls(&mut candidate, &controls)?;
    candidate.commit();
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued() -> (AutoCommit, WorkspaceTaskMutationResult) {
        let mut room = AutoCommit::new();
        let applied = apply(
            &mut room,
            uuid::Uuid::from_u128(1),
            [7; 32],
            &AdmittedTaskActor::Person {
                id: "person".into(),
                device: [8; 32],
            },
            &WorkspaceTaskCommand::Queue {
                title: "Task".into(),
                assigned_agent: "agent".into(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
            1,
        )
        .unwrap();
        (room, applied.result)
    }

    #[test]
    fn binding_is_exact_canonical_idempotent_and_survives_projection() {
        let (room, result) = queued();
        let reference = private_instruction_reference("private body");
        let mut bound = bind_private_instruction(&room, &result, &reference).unwrap();
        assert!(
            controls_of(&room)
                .unwrap()
                .values()
                .all(|c| c.private_instruction_ref.is_none())
        );
        let before = bound.save();
        let mut replay = bind_private_instruction(&bound, &result, &reference).unwrap();
        assert_eq!(before, replay.save());
        assert!(
            bind_private_instruction(&bound, &result, &private_instruction_reference("other"))
                .is_err()
        );
        let controls = controls_of(&bound).unwrap();
        project_controls(&mut bound, &controls).unwrap();
        let restored = AutoCommit::load(&bound.save()).unwrap();
        assert_eq!(controls_of(&restored).unwrap(), controls);
        assert!(controls.values().all(|c| c.instruction.is_none()
            && c.private_instruction_ref.as_deref() == Some(reference.as_str())));
    }

    #[test]
    fn opaque_about_instruction_moves_to_canonical_reference_without_plaintext() {
        let (mut room, result) = queued();
        let reference = private_instruction_reference("about body");
        let mut controls = controls_of(&room).unwrap();
        controls.values_mut().next().unwrap().instruction = Some(reference.clone());
        project_controls(&mut room, &controls).unwrap();
        let bound = bind_private_instruction(&room, &result, &reference).unwrap();
        let controls = controls_of(&bound).unwrap();
        let control = controls.values().next().unwrap();
        assert_eq!(
            control.private_instruction_ref.as_deref(),
            Some(reference.as_str())
        );
        assert!(control.instruction.is_none());
    }

    #[test]
    fn rejects_unaccepted_results_noncanonical_references_and_public_fallback() {
        let (mut room, result) = queued();
        assert!(bind_private_instruction(&room, &result, "private-instruction:FAKE").is_err());
        let reference = private_instruction_reference("body");
        assert!(bind_private_instruction(&room, &result, &reference.to_uppercase()).is_err());
        assert!(
            bind_private_instruction(
                &room,
                &WorkspaceTaskMutationResult::Finished {
                    task_id: "task".into()
                },
                &reference
            )
            .is_err()
        );
        assert!(
            bind_private_instruction(
                &room,
                &WorkspaceTaskMutationResult::Queued {
                    task_id: "absent".into()
                },
                &reference
            )
            .is_err()
        );
        let mut controls = controls_of(&room).unwrap();
        let control = controls.values_mut().next().unwrap();
        control.instruction = Some("body".into());
        project_controls(&mut room, &controls).unwrap();
        assert!(bind_private_instruction(&room, &result, &reference).is_err());
        controls.values_mut().next().unwrap().instruction = None;
        controls.values_mut().next().unwrap().receipts.clear();
        project_controls(&mut room, &controls).unwrap();
        assert!(bind_private_instruction(&room, &result, &reference).is_err());
    }
}
