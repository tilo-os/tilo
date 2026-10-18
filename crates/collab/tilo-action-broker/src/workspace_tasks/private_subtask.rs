//! Typed construction of an atomic owner-private child task request.
use tilo_desktop_model::WorkspaceTaskRoute;
use tilo_peer_state::workspace_task::WorkspaceTaskCommand;

/// Queue a child with a public title and a separate private execution goal.
///
/// Submit this command through the native workspace owner with authenticated
/// person authority. The owner validates parent access, agent membership and
/// route before atomically retaining the child and its private instruction.
/// This does not approve or start execution, and must not be applied directly
/// to peer-state or published into the room document.
#[must_use]
pub fn queue_private_subtask(
    parent: impl Into<String>,
    title: impl Into<String>,
    goal: impl Into<String>,
    agent: impl Into<String>,
    route: WorkspaceTaskRoute,
) -> WorkspaceTaskCommand {
    WorkspaceTaskCommand::QueuePrivateSubtask {
        parent_task_id: parent.into(),
        title: title.into(),
        instruction: goal.into(),
        assigned_agent: agent.into(),
        route,
    }
}
