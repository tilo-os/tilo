//! An automation a task waits for tells that task to go on.
//!
//! A task that waits names what can end its wait early: one of its person's automations, by
//! its id (the `on` of Tasks' `wait`, kept as the task's `waiting_on`). When that automation
//! reaches the step that hands its instruction over, the server looks first whether a task of
//! the workspace waits for it: an open task with an agent, given by the automation's own
//! person. It then invokes Tasks' `resume` on that task for the person, as their automation,
//! with what the automation hands over as what lets it go on, and the step is answered. The
//! task's agent is started again by its own trigger on that change, told those words, and no
//! turn of its person's own conversation is spent on waking it.
//!
//! An automation of one person never tells a task given by another to go on, whatever id that
//! task's wait names.

use serde_json::{Map, json};
use tilo_actions::remote::Caller;
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use super::triggered::TASKS;
use super::{WorkspaceActionProvider, invoke_named, task_work};
use crate::workspaces::Workspaces;

/// The most of what an automation hands over that a task is told as what lets it go on, in
/// characters.
const MOST_CAUSE_CHARS: usize = 400;

/// Tell every task of `workspace` that waits for `person`'s automation `automation` to go on,
/// with `words` (what the automation hands over) as what lets it, through `actions`. Whether
/// one was told: none waits for most automations, and then whoever takes instructions takes
/// this one.
pub(super) fn told(
    workspaces: &Workspaces,
    actions: &WorkspaceActions,
    (workspace, person): (&str, &str),
    automation: &str,
    words: &str,
) -> bool {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    // The thread that asks is driving the automation and cannot wait on the runtime a second
    // time: the tasks are read and told on a thread of this step's own.
    let told = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("task-wait".into())
            .spawn_scoped(scope, || {
                runtime.block_on(go_on(
                    workspaces,
                    actions,
                    (workspace, person),
                    automation,
                    words,
                ))
            })
            .ok()
            .and_then(|waited| waited.join().ok())
    });
    match told {
        Some(Ok(told)) => told > 0,
        Some(Err(error)) => {
            tracing::warn!(%workspace, %automation, %error, "the tasks that wait for an automation are not told");
            false
        }
        None => false,
    }
}

/// What a task is told lets it go on: `words` on one line, cut to what such a line keeps.
fn cause_of(words: &str) -> String {
    let line = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return "the automation it waited for came".to_owned();
    }
    line.chars().take(MOST_CAUSE_CHARS).collect()
}

/// [`told`], on the runtime: how many tasks were told to go on.
async fn go_on(
    workspaces: &Workspaces,
    actions: &WorkspaceActions,
    (workspace, person): (&str, &str),
    automation: &str,
    words: &str,
) -> Result<usize, String> {
    let member = tilo_action_policy::MemberId::person(person);
    let waiting: Vec<task_work::Held> = task_work::tasks(
        workspaces,
        workspace,
        vec![tilo_resource_store::Filter::Is(
            "waiting_on".to_owned(),
            tilo_resource_store::Value::Text(automation.to_owned()),
        )],
    )
    .await?
    .into_iter()
    .filter(|task| !task.closed() && task.with_an_agent() && task.given_by(&member))
    .collect();
    if waiting.is_empty() {
        return Ok(0);
    }
    let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
    let root: [u8; 32] = hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("the automation works for nobody this server knows")?;
    let device = workspaces
        .person_device(workspace_id, root)
        .await
        .ok_or("its person is admitted to this workspace on no device")?;
    // Tasks' `resume` is the person's, carried out by their automation.
    let caller = Caller::authenticated(root, device)
        .in_workspace(workspace.to_owned())
        .running_shortcut(automation.to_owned());
    let service = WorkspaceActionProvider::new(actions.clone(), workspace_id);
    let route = DocumentRoute {
        app_id: TASKS.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let mut arguments = Map::new();
    arguments.insert("cause".into(), json!(cause_of(words)));
    let mut told: usize = 0;
    for task in waiting {
        // Tasks' own rules decide as well: a task that waits for nothing any more is not told.
        match invoke_named(
            &service,
            &caller,
            &route,
            &task.address(),
            "resume",
            arguments.clone(),
        )
        .await
        {
            Ok(_) => told = told.saturating_add(1),
            Err(error) => {
                tracing::warn!(%workspace, task = %task.id, %error, "a task that waits for an automation is not told to go on");
            }
        }
    }
    Ok(told)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_lets_a_task_go_on_is_one_line_of_what_the_automation_hands_over() {
        assert_eq!(
            cause_of("  The venue\nconfirmed   the date. "),
            "The venue confirmed the date."
        );
        assert_eq!(cause_of(" \n "), "the automation it waited for came");
        assert_eq!(cause_of(&"word ".repeat(200)).chars().count(), 400);
    }
}
