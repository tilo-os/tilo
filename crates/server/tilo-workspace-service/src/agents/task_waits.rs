//! An automation a task waits for tells that task to go on.
//!
//! A task that waits names what can end its wait early: one of its person's automations, by
//! its id ([`tasks_model::WaitFor::event`]). When that automation hands its instruction to its
//! person's assistant, the turn it asks for looks first whether a task of the workspace waits
//! for it ([`tasks_model::waiting_for`]): an open issue with an agent, given for the
//! automation's own person. It then writes Tasks' `resume` on that issue for the person, with
//! what the automation handed over as what lets it go on, and the turn ends there. The task's
//! agent is asked again from the issue itself, with those words, and no model of its person's
//! own conversation is spent on waking it.
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

/// Whether `document` is a part of the tracker's issues.
fn keeps_issues(document: &str) -> bool {
    collab_types::document::DocumentId::parse(document)
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

/// Tell every task of `workspace` that waits for `person`'s automation `automation` to go on,
/// with `words` (what the automation handed over) as what lets it: how many were told. None
/// waits for most automations.
///
/// # Errors
/// The workspace's issues could not be read.
pub(super) async fn go_on(
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
            match resume(
                workspaces,
                (workspace, document),
                person,
                automation,
                &issue,
                &cause,
            )
            .await
            {
                Ok(resumed) => told = told.saturating_add(usize::from(resumed)),
                Err(error) => {
                    tracing::warn!(%workspace, %issue, %error, "a task that waits for an automation is not told to go on");
                }
            }
        }
    }
    Ok(told)
}

/// Write Tasks' `resume` on `issue` for `person` if it still waits for `automation`.
/// Tasks' own rules also decide, so an issue whose budget is used up is not told to go on.
async fn resume(
    workspaces: &Workspaces,
    (workspace, document): (&str, &str),
    person: &str,
    automation: &str,
    issue: &tilo_resource::Id,
    cause: &str,
) -> Result<bool, String> {
    let cause = tilo_resource::NonBlank::parse(cause).ok_or("nothing says what lets it go on")?;
    let (person, issue) = (person.to_owned(), issue.clone());
    // The moment it is told is the server's own: what asks the task's agent again is read by it.
    let now_ms = tilo_clock::unix_now_ms_i64();
    workspaces
        .author_application(
            workspace,
            workspaces.device_cert(),
            &tilo_agent_session::AppCatalogue::native(),
            TASKS,
            document,
            move |doc| {
                let current = super::opened::<IssueList>(doc)?;
                if !current
                    .issues
                    .get(&issue)
                    .is_some_and(|issue| issue.waits_for_automation(&person, automation))
                {
                    return Ok(false);
                }
                let mut tracker =
                    tilo_resource::Replica::<IssueList>::from_doc(std::mem::take(doc))
                        .as_author(&person)
                        .in_app(TASKS, "Tasks");
                tracker.set_now_ms(now_ms);
                let applied = tracker
                    .apply(IssueList::RESUME.with(issue_list::Resume { issue, cause }))
                    .map(|_| true)
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
    use super::*;
    use tilo_agent_session::{AppCatalogue, Replica, ReplicaActor, Seed};

    #[test]
    fn what_lets_a_task_go_on_is_one_line_of_what_the_automation_hands_over() {
        assert_eq!(
            cause_of("  The venue\nconfirmed   the date. "),
            "The venue confirmed the date."
        );
        assert_eq!(cause_of(" \n "), "the automation it waited for came");
        assert_eq!(cause_of(&"word ".repeat(200)).chars().count(), 400);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_automation_selected_before_the_wait_changed_cannot_end_the_new_wait() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::workspaces::fixtures::governor();
        // The native apps name their surfaces as the host first runs them.
        let catalogue = AppCatalogue::native();
        crate::surfaces::decide_receipt(&scratch.path().join("surfaces"));
        let server = crate::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Launch").await.unwrap().workspace;
        let person = hex::encode(cert.user_public_key);
        let chain = tasks::issue_list::chain();
        let part = chain.part(0);
        let document = part.as_str();
        let tracker = super::super::workspace_resource(&catalogue, TASKS);
        workspaces
            .prepare_application(&workspace, &cert, &catalogue, TASKS, &tracker)
            .await
            .unwrap();
        let first_part = {
            let mut page = Replica::open(
                &catalogue,
                TASKS,
                &workspace,
                &tilo_actions::MemberId::person(&person),
                ReplicaActor::Owner,
                if tracker.is_empty() {
                    Seed::New
                } else {
                    Seed::NewAt(collab_types::document::DocumentId::parse(&tracker).unwrap())
                },
            )
            .unwrap();
            page.invoke(
                &tilo_actions::api::ActionAddress::new(
                    collab_types::document::chain_resource_id(&chain),
                    "create",
                ),
                &serde_json::Map::from_iter([
                    ("title".to_owned(), "Book the venue".into()),
                    ("assignee".to_owned(), "agent:scout".into()),
                ]),
                false,
            )
            .unwrap();
            page.take_document_mutations()
                .unwrap()
                .into_iter()
                .find_map(|(id, mutation)| match mutation {
                    collab_types::DocumentMutation::Snapshot(bytes) if id == part => Some(bytes),
                    _ => None,
                })
                .unwrap()
        };
        let issue = workspaces
            .author_workspace_document(&workspace, &cert, (TASKS, document), |doc| {
                tilo_automerge::ApplyIncoming::load_incoming(doc, &first_part)
                    .map_err(|error| error.to_string())?;
                let mut tracker =
                    tilo_resource::Replica::<IssueList>::from_doc(std::mem::take(doc))
                        .as_author(&person)
                        .in_app(TASKS, "Tasks");
                let state = tracker.state().unwrap();
                let issue = state.issues.iter().next().unwrap().0.clone();
                tracker
                    .apply(IssueList::WAIT.with(issue_list::Wait {
                        issue: issue.clone(),
                        waits: vec![tasks_model::WaitFor {
                            kind: tasks_model::WaitKind::Reply,
                            what: "the new venue's answer".to_owned(),
                            event: Some("new-venue".to_owned()),
                            ..Default::default()
                        }],
                        until_ms: tilo_clock::unix_now_ms_i64().saturating_add(60_000),
                    }))
                    .unwrap();
                std::mem::swap(doc, tracker.doc_mut());
                Ok(issue)
            })
            .await
            .unwrap();

        let resume_for = |automation| {
            resume(
                workspaces,
                (&workspace, document),
                &person,
                automation,
                &issue,
                "The venue replied",
            )
        };
        let task_bytes = || workspaces.resource_document(&workspace, TASKS, document);
        let before = task_bytes().await.unwrap().unwrap();
        assert!(!resume_for("old-venue").await.unwrap());
        assert_eq!(task_bytes().await.unwrap().unwrap(), before);
        assert!(resume_for("new-venue").await.unwrap());
        let bytes = task_bytes().await.unwrap().unwrap();
        let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
        let saved = super::super::opened::<IssueList>(&mut doc).unwrap();
        let work = saved.issues.get(&issue).unwrap().work.as_ref().unwrap();
        assert_eq!(work.resumed.as_ref().unwrap().cause, "The venue replied");
        assert!(!work.waiting());
        drop(server);
    }
}
