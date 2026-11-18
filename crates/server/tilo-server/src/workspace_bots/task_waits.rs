//! An automation a task waits for tells that task to go on.
//!
//! A task that waits names what can end its wait early: one of its person's automations, by
//! its id ([`tasks_model::WaitFor::event`]). When that automation reaches the step that hands
//! its instruction over, the server looks first whether a task of the workspace waits for it
//! ([`tasks_model::waiting_for`]): an open issue with an agent, given for the automation's own
//! person. It then writes Tasks' `resume` on that issue for the person, with what the
//! automation hands over as what lets it go on, and the step is answered. The task's agent is
//! asked again from the issue itself, with those words, and no turn of its person's own
//! conversation is spent on waking it.
//!
//! An automation of one person never tells a task given for another to go on, whatever id that
//! task's wait names.

use tasks::issue_list::{IssueList, issue_list};

use crate::workspaces::Workspaces;

/// The app that keeps the workspace's issues.
const TASKS: &str = "so.tilo.tasks";
/// The most of what an automation hands over that a task is told as what lets it go on, in
/// characters.
const MOST_CAUSE_CHARS: usize = 400;

/// Tell every task of `workspace` that waits for `person`'s automation `automation` to go on,
/// with `words` (what the automation hands over) as what lets it. Whether one was told: none
/// waits for most automations, and then whoever takes instructions takes this one.
pub(super) fn told(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    automation: &str,
    words: &str,
) -> bool {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    // The thread that asks is driving the automation and cannot wait on the runtime a second
    // time: the issues are read and written on a thread of this step's own.
    let told = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("task-wait".into())
            .spawn_scoped(scope, || {
                runtime.block_on(go_on(workspaces, workspace, person, automation, words))
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

/// Whether `document` is a part of the tracker's issues.
fn keeps_issues(document: &str) -> bool {
    ui::resource::DocumentId::parse(document)
        .and_then(|id| id.chain_part())
        .is_some_and(|(chain, _)| chain == tasks::issue_list::chain())
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
    workspace: &str,
    person: &str,
    automation: &str,
    words: &str,
) -> Result<usize, String> {
    let cause = cause_of(words);
    let routes = workspaces
        .document_routes(workspace)
        .await
        .map_err(|error| error.to_string())?;
    let mut told: usize = 0;
    for (app, document) in &routes {
        if app != TASKS || !keeps_issues(document) {
            continue;
        }
        let Some(bytes) = workspaces
            .resource_document(workspace, TASKS, document)
            .await
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        // Nothing of the part is kept while a task is told: an issue's rich text does not travel
        // between threads.
        let waiting = {
            let part = super::opened::<IssueList>(&mut doc)?;
            tasks_model::waiting_for(part.issues.iter(), person, automation)
        };
        for issue in waiting {
            match resume(workspaces, (workspace, document), person, &issue, &cause).await {
                Ok(()) => told = told.saturating_add(1),
                Err(error) => {
                    tracing::warn!(%workspace, %issue, %error, "a task that waits for an automation is not told to go on");
                }
            }
        }
    }
    Ok(told)
}

/// Write Tasks' `resume` on `issue` for `person`: Tasks' own rules still decide, so an issue
/// that was closed meanwhile, or whose budget is used up, is not told to go on.
async fn resume(
    workspaces: &Workspaces,
    (workspace, document): (&str, &str),
    person: &str,
    issue: &tilo_resource::Id,
    cause: &str,
) -> Result<(), String> {
    let cause = tilo_resource::NonBlank::parse(cause).ok_or("nothing says what lets it go on")?;
    let (person, issue) = (person.to_owned(), issue.clone());
    // The moment it is told is the server's own: what asks the task's agent again is read by it.
    let now_ms = tilo_clock::unix_now_ms_i64();
    workspaces
        .author_application(
            workspace,
            workspaces.governor(),
            &tilo_agent_session::AppCatalogue::native(),
            TASKS,
            document,
            move |doc| {
                let mut tracker =
                    tilo_resource::Replica::<IssueList>::from_doc(std::mem::take(doc))
                        .as_author(&person)
                        .in_app(TASKS, "Tasks");
                tracker.set_now_ms(now_ms);
                let applied = tracker
                    .apply(IssueList::RESUME.with(issue_list::Resume { issue, cause }))
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, tracker.doc_mut());
                applied
            },
        )
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::cause_of;

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
