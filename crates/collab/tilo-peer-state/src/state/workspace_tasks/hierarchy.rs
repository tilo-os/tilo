//! Live write checks for both sides of a coordination hierarchy edit.
use super::*;

impl StatePeer {
    pub(super) fn check_task_metadata_authority(
        &self,
        command: &WorkspaceTaskCommand,
        actor: &AdmittedTaskActor,
        doc: &automerge::AutoCommit,
        now: SystemTime,
    ) -> Result<(), String> {
        let mut targets = Vec::new();
        match command {
            WorkspaceTaskCommand::CreateSubtask { parent_task_id, .. }
            | WorkspaceTaskCommand::QueuePrivateSubtask { parent_task_id, .. } => {
                targets.push(parent_task_id.as_str());
            }
            WorkspaceTaskCommand::MoveTask {
                task_id,
                parent_task_id,
                ..
            } => {
                targets.push(task_id.as_str());
                targets.extend(parent_task_id.as_deref());
            }
            WorkspaceTaskCommand::SetMetadata { task_id, metadata } => {
                targets.push(task_id.as_str());
                targets.extend(metadata.parent_task_id.as_deref());
            }
            WorkspaceTaskCommand::QueueWithMetadata { metadata, .. } => {
                targets.extend(metadata.parent_task_id.as_deref());
            }
            WorkspaceTaskCommand::SetLabels { task_id, .. }
            | WorkspaceTaskCommand::SetAssignees { task_id, .. }
            | WorkspaceTaskCommand::BindExecutor { task_id, .. } => {
                targets.push(task_id.as_str());
            }
            _ => return Ok(()),
        }
        let AdmittedTaskActor::Person { device, .. } = actor else {
            return Err("only a person may change task metadata".into());
        };
        let member = self.permissions(*device, now)?;
        let tasks = tilo_desktop_model::projection::workspace_tasks_of(doc);
        let mut seen = std::collections::BTreeSet::new();
        for target in targets {
            let mut next = Some(target);
            let mut chain = std::collections::BTreeSet::new();
            while let Some(id) = next {
                if !chain.insert(id) {
                    return Err("task hierarchy contains a cycle".into());
                }
                let task = tasks
                    .iter()
                    .find(|task| task.id == id)
                    .ok_or("task or parent is absent from this workspace")?;
                if seen.insert(id) {
                    let node = TreePath::room(&self.core.room)
                        .app(tilo_desktop_model::workspace::WORKSPACE_GOVERNANCE_APP_ID)
                        .kind(tilo_desktop_model::task::TASK_KIND)
                        .resource(format!("desktop:{id}"));
                    if !member.allows(&node, ActionKind::Undoable, now) {
                        return Err("task or parent is not writable".into());
                    }
                }
                next = task.parent_task_id.as_deref();
            }
        }
        Ok(())
    }
}
