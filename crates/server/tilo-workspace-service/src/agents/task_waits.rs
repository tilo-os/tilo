//! An automation a task waits for tells that task to go on.
//!
//! A task that waits names what can end its wait early: one of its person's automations, by
//! its id (Tasks' `waiting_on`). When that automation reaches the step that hands its
//! instruction over, the server looks first whether a task of the workspace waits for it: an
//! open task given to an agent by the automation's own person, read as that person may read it.
//! It then invokes Tasks' `resume` on that task as the automation acting for the person, with
//! what the automation hands over as what lets it go on, and the step is answered. Tasks decides
//! whether it goes on; the task's agent is triggered by that change on the task itself, and no
//! turn of its person's own conversation is spent on waking it.
//!
//! An automation of one person never tells a task given by another to go on, whatever id that
//! task's wait names.

use serde_json::{Map, Value};
use tilo_actions::remote::Caller;
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_resource_store::{Filter, Value as Stored};
use tilo_server_protocol::WorkspaceId;

use super::WorkspaceActionProvider;
use super::tl_tasks::{self, Reader};
use crate::workspaces::Workspaces;

/// The most of what an automation hands over that a task is told as what lets it go on, in
/// characters.
const MOST_CAUSE_CHARS: usize = 400;

/// Tell every task of `workspace` that waits for `person`'s automation `automation` to go on,
/// with `words` (what the automation hands over) as what lets it, through `actions`. Whether one
/// was told: none waits for most automations, and then whoever takes instructions takes this
/// one. A server whose action service takes no write tells none.
pub(super) fn told(
    workspaces: &Workspaces,
    actions: Option<&WorkspaceActions>,
    workspace: &str,
    person: &str,
    automation: &str,
    words: &str,
) -> bool {
    let Some(actions) = actions else {
        return false;
    };
    let Some(id) = WorkspaceId::parse(workspace) else {
        return false;
    };
    let provider = WorkspaceActionProvider::new(actions.clone(), id);
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    // The thread that asks is driving the automation and cannot wait on the runtime a second
    // time: the tasks are read and written on a thread of this step's own.
    let told = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("task-wait".into())
            .spawn_scoped(scope, || {
                runtime.block_on(go_on(
                    workspaces, &provider, workspace, person, automation, words,
                ))
            })
            .ok()
            .and_then(|waited| waited.join().ok())
    });
    match told {
        Some(Ok(told)) => told > 0,
        Some(Err(error)) => {
            tracing::warn!(%workspace, %automation, %error, "the tasks that wait for an automation are not read");
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
    provider: &WorkspaceActionProvider,
    workspace: &str,
    person: &str,
    automation: &str,
    words: &str,
) -> Result<usize, String> {
    let waiting = Reader::Person(workspaces, person)
        .tasks(
            workspace,
            vec![Filter::Is(
                "waiting_on".to_owned(),
                Stored::Text(automation.to_owned()),
            )],
        )
        .await?;
    let given_by = tl_tasks::person(person);
    let waiting: Vec<_> = waiting
        .into_iter()
        .filter(|task| !task.closed() && task.assigned_by.as_deref() == Some(given_by.as_str()))
        .collect();
    if waiting.is_empty() {
        return Ok(0);
    }
    let id = WorkspaceId::parse(workspace).ok_or("no workspace of this server")?;
    let root: [u8; 32] = hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("the automation works for nobody this server knows")?;
    let device = workspaces
        .person_device(id, root)
        .await
        .ok_or("its person is admitted to this workspace on no device")?;
    // Tasks pins who resumed it from who asks: the person, through their automation.
    let caller = Caller::authenticated(root, device)
        .in_workspace(workspace.to_owned())
        .delegated_to(automation.to_owned());
    let mut arguments = Map::new();
    arguments.insert("cause".into(), Value::from(cause_of(words)));
    // The wait this automation ends: Tasks refuses it when a newer wait names another.
    arguments.insert("on".into(), Value::from(automation));
    let mut told: usize = 0;
    for task in waiting {
        let resumed = tl_tasks::invoke(
            provider,
            &caller,
            workspace,
            &task.resource(),
            "resume",
            arguments.clone(),
        )
        .await;
        match resumed {
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

    /// A task given to the agent by its person waits on the person's automation; that automation
    /// tells it to go on with what it handed over, as the automation acting for the person. An
    /// older automation, whose wait a newer one replaced, ends nothing.
    #[cfg(feature = "apps")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_automation_a_task_waits_on_tells_it_to_go_on_and_an_older_one_does_not() {
        use serde_json::json;

        let host = tl_tasks::testing::Host::new().await;
        let id = host.new_task("Book the venue", json!({})).await;
        let task = tl_tasks::resource(&id);
        host.act(&task, "assign", json!({"who": {"agent": "scout"}}))
            .await;
        host.act(
            &task,
            "wait",
            json!({"what": "the venue's reply", "on": "older"}),
        )
        .await;
        host.act(
            &task,
            "wait",
            json!({"what": "the venue's reply", "on": "newer"}),
        )
        .await;
        let broker = host.broker(&[]);
        let provider = host.service(&broker);
        let told = |automation: &'static str| {
            go_on(
                &host.workspaces,
                &provider,
                &host.workspace,
                &host.person,
                automation,
                "The venue confirmed the date.",
            )
        };
        assert_eq!(told("older").await, Ok(0));
        assert_eq!(host.task(&id).await.waiting_on, "newer", "it waits still");
        assert_eq!(told("newer").await, Ok(1));
        let asked = broker.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "{asked:?}");
        let (caller, resource, action, arguments) = &asked[0];
        assert_eq!(
            (resource.as_str(), action.as_str()),
            (task.as_str(), "resume")
        );
        assert_eq!(arguments["on"], "newer");
        assert_eq!(arguments["cause"], "The venue confirmed the date.");
        assert!(caller.acting().is_agent(), "{caller:?}");
        let went_on = host.task(&id).await;
        assert_eq!(went_on.waiting_on, "");
        let reader = Reader::Person(&host.workspaces, &host.person);
        let work = reader.work(&host.workspace, &went_on).await.unwrap();
        assert_eq!(
            work.resumed.as_deref(),
            Some("The venue confirmed the date.")
        );
        host.end().await;
    }

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
