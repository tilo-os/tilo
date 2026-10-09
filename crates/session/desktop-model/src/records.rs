//! A workspace's task records, as every host keeps them: status changes and
//! broker-authenticated audit events, stamped with the time they were made.

use crate::{Workspace, WorkspaceTask, WorkspaceTaskAuditEvent, WorkspaceTaskStatus};

impl Workspace {
    /// Move task `task_id` to `status` at `now` (Unix seconds), noting who
    /// did it in the task's audit. The changed task, or `None` when the task
    /// is absent or already has that status.
    pub fn set_task_status_at(
        &mut self,
        task_id: &str,
        status: WorkspaceTaskStatus,
        actor: &str,
        now: i64,
    ) -> Option<&WorkspaceTask> {
        let task = self.tasks.iter_mut().find(|task| task.id == task_id)?;
        if task.status == status {
            return None;
        }
        task.status = status;
        task.updated_at = now;
        task.audit.push(WorkspaceTaskAuditEvent::new(
            now,
            actor,
            "set-status",
            [("status", tilo_actions::Scalar::from(status.key()))],
        ));
        Some(task)
    }

    /// Append the progress `actor` reports, in their own words, to task
    /// `task_id`'s audit at `now` (Unix seconds) without changing its state:
    /// agent progress is shared audit data, not a substitute for a document
    /// write. The task, or `None` when absent.
    pub fn append_task_audit_at(
        &mut self,
        task_id: &str,
        actor: &str,
        progress: impl Into<String>,
        now: i64,
    ) -> Option<&WorkspaceTask> {
        let task = self.tasks.iter_mut().find(|task| task.id == task_id)?;
        task.updated_at = now;
        task.audit
            .push(WorkspaceTaskAuditEvent::new(now, actor, "report", []).noting(progress));
        Some(task)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Workspace, WorkspaceId, WorkspaceTask, WorkspaceTaskRoute, WorkspaceTaskStatus};

    fn workspace_with_task() -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.tasks.push(WorkspaceTask {
            id: "task-1".into(),
            title: "Draft".into(),
            created_by: "person-1".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Manual,
            status: WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        });
        workspace
    }

    #[test]
    fn status_changes_are_stamped_and_audited_once() {
        let mut workspace = workspace_with_task();
        let target = WorkspaceTaskStatus::Done;
        let task = workspace
            .set_task_status_at("task-1", target, "person-1", 42)
            .unwrap();
        assert_eq!(task.status, target);
        assert_eq!(task.updated_at, 42);
        let event = task.audit.last().unwrap();
        assert_eq!((event.at, event.actor.as_str()), (42, "person-1"));
        assert_eq!(event.action(), "set-status");
        assert_eq!(
            event.entry().what,
            Some(event.what.clone()),
            "the event reads as a history entry"
        );
        // The same status again changes nothing and records nothing.
        assert!(
            workspace
                .set_task_status_at("task-1", target, "person-1", 43)
                .is_none()
        );
        assert_eq!(workspace.tasks[0].audit.len(), 1);
        assert!(
            workspace
                .set_task_status_at("absent", target, "person-1", 43)
                .is_none()
        );
    }

    #[test]
    fn audit_events_leave_the_status_alone() {
        let mut workspace = workspace_with_task();
        let status = workspace.tasks[0].status;
        let task = workspace
            .append_task_audit_at("task-1", "agent-1", "Wrote the outline", 7)
            .unwrap();
        assert_eq!(task.status, status);
        assert_eq!(task.updated_at, 7);
        let event = task.audit.last().unwrap();
        assert_eq!(event.action(), "report");
        assert_eq!(event.note.as_deref(), Some("Wrote the outline"));
        assert!(
            workspace
                .append_task_audit_at("absent", "agent-1", "x", 8)
                .is_none()
        );
    }
}
