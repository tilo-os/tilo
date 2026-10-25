//! Undoing an agent's run of a workspace task as one, from Activity.
//!
//! An agent working a task stamps each change with the task's group, so every
//! document it changed can take those changes back by naming the task: its
//! history's `undo` with `{"run": task}`. That goes through the document's own
//! history rather than reverting it from outside, so a change that queued
//! something beyond the document (a file renamed on disk) is undone by
//! queueing its compensation, and edits others made since are kept.
//!
//! This asks each document the run changed to do that, through the session's
//! action door as the person at the screen, and waits for the answers. It
//! blocks, so the host runs it off the shell's thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHost, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, InvokeArgument,
    InvokeRequest, MAX_CATALOGUE_PAGE, ReceiptStatus, RemoteActionTarget,
};

/// How long one run's undo may take altogether.
pub(crate) const UNDO_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a running undo is looked at again.
const POLL: Duration = Duration::from_millis(100);

/// The history verb each document publishes, and the argument naming a run.
const UNDO: &str = "undo";
const RUN: &str = "run";

/// Undo task `task`'s run in each document `steps` names (app and record),
/// through `host` as `caller`.
///
/// Answers one [`DocumentUndo`] per document the run changed, in the order
/// `steps` names them, so the task's view can say which records came back
/// and which did not — a record nobody reverted is reported, never dropped.
///
/// # Errors
/// Nothing was undone: no document the run changed is open, or each refused
/// (it had nothing of the run left in effect, or the person may not change
/// it). A run undone in some documents but not others is reported as done
/// with the ones that were not; their records stay, so they can be asked
/// again.
pub(crate) fn undo_task_run(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    task: &str,
    steps: &[(String, String)],
    timeout: Duration,
) -> Result<Vec<DocumentUndo>, String> {
    let now = Instant::now();
    let deadline = now.checked_add(timeout).unwrap_or(now);
    let providers = ask(
        host,
        caller,
        CatalogueQuery {
            max_results: 0,
            ..CatalogueQuery::default()
        },
        deadline,
    )
    .ok_or("the action door did not answer")?
    .providers;
    let targets = undo_targets(&providers, caller.room().unwrap_or_default(), steps);
    if targets.is_empty() {
        return Err("open the documents this task changed to undo it".into());
    }
    let mut undone: usize = 0;
    let mut refused = Vec::new();
    for target in &targets {
        let Some(address) = undo_address(host, caller, &target.provider.provider, deadline) else {
            refused.push(format!("{} has no history to undo", target.provider.app_id));
            continue;
        };
        match invoke(
            host,
            caller,
            &target.provider.provider,
            address,
            task,
            deadline,
        ) {
            Ok(()) => undone = undone.saturating_add(1),
            Err(error) => refused.push(format!("{}: {error}", target.provider.app_id)),
        }
    }
    if undone == 0 {
        return Err(refused.join("; "));
    }
    if !refused.is_empty() {
        tracing::warn!(task, refused = %refused.join("; "), "a task's run was undone only in part");
    }
    Ok(targets.into_iter().map(|target| target.outcome).collect())
}

/// One document the task's run changed, and the publisher that served it.
struct UndoTarget {
    provider: CatalogueProvider,
    /// The step as the run recorded it, which the outcome is reported under.
    outcome: DocumentUndo,
}

/// What undoing a task did to one of the records it changed.
pub(crate) type DocumentUndo = tilo_desktop::activity::TaskRunDocument;

/// The live publishers of the documents `steps` names in `room`: a step's
/// record where its app publishes it, else every open document of that app
/// (undo by run changes nothing in a document the run did not touch).
fn undo_targets(
    providers: &[CatalogueProvider],
    room: &str,
    steps: &[(String, String)],
) -> Vec<UndoTarget> {
    let live = |provider: &&CatalogueProvider| {
        provider.room == room && provider.provider.parse::<u64>().is_ok()
    };
    let mut targets: Vec<UndoTarget> = Vec::new();
    for (app, record) in steps {
        let of_app: Vec<&CatalogueProvider> = providers
            .iter()
            .filter(live)
            .filter(|provider| &provider.app_id == app)
            .collect();
        let exact: Vec<&CatalogueProvider> = of_app
            .iter()
            .copied()
            .filter(|provider| !record.is_empty() && &provider.record_id == record)
            .collect();
        for provider in if exact.is_empty() { of_app } else { exact } {
            if targets
                .iter()
                .any(|known| known.provider.provider == provider.provider)
            {
                continue;
            }
            targets.push(UndoTarget {
                provider: provider.clone(),
                outcome: DocumentUndo {
                    app: app.clone(),
                    record: record.clone(),
                },
            });
        }
    }
    targets
}

/// The address of the history `undo` provider `provider` publishes.
fn undo_address(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    provider: &str,
    deadline: Instant,
) -> Option<ActionAddress> {
    let page = ask(
        host,
        caller,
        CatalogueQuery {
            provider: Some(provider.to_owned()),
            needle: UNDO.to_owned(),
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        },
        deadline,
    )?;
    page.rows
        .into_iter()
        .filter_map(|row| row.action)
        .find(|action| {
            action.address.action == UNDO && action.params.iter().any(|param| param.name == RUN)
        })
        .map(|action| action.address)
}

fn invoke(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    provider: &str,
    address: ActionAddress,
    task: &str,
    deadline: Instant,
) -> Result<(), String> {
    let mut args = serde_json::Map::new();
    args.insert(RUN.into(), task.into());
    let invocation = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address,
        },
        argument: InvokeArgument::Json(args),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let mut status = match host.invoke(caller, request).wait_timeout(left.max(POLL)) {
        Ok(Some(Ok(receipt))) => receipt.status,
        Ok(Some(Err(error))) => return Err(error.to_string()),
        _ => return Err("the document did not answer".into()),
    };
    while matches!(status, ReceiptStatus::Accepted) {
        if Instant::now() >= deadline {
            return Err("the document did not finish undoing in time".into());
        }
        std::thread::sleep(POLL);
        if let Some(state) = host.observe(caller, &invocation) {
            status = state.receipt().status;
        }
    }
    match status {
        ReceiptStatus::Completed { .. } => Ok(()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Cancelled { .. } => Err("cancelled".into()),
        ReceiptStatus::Accepted => Err("still running".into()),
    }
}

fn ask(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    query: CatalogueQuery,
    deadline: Instant,
) -> Option<CataloguePage> {
    let left = deadline.saturating_duration_since(Instant::now());
    host.query(caller, query)
        .wait_timeout(left.max(POLL))
        .ok()
        .flatten()?
        .ok()
}

/// Undoes run by run on a thread of its own, so Activity's request
/// returns at once and its outcome is polled like any review command's.
#[derive(Clone, Default)]
pub(crate) struct TaskUndos {
    results: Arc<std::sync::Mutex<Outcomes>>,
}

/// Each undo by its request: `None` while it runs, then how it ended.
type Outcomes = std::collections::BTreeMap<uuid::Uuid, Option<Result<Vec<DocumentUndo>, String>>>;

/// Where one undo is.
pub(crate) enum UndoState {
    Running,
    Ended(Result<Vec<DocumentUndo>, String>),
}

/// How many undo outcomes are kept for the pane to poll.
const MAX_KEPT: usize = 32;

impl TaskUndos {
    /// Start undoing `task`'s run in `room` as the person at the screen,
    /// through `broker`. Everything that waits, admitting the person's
    /// session included, runs on the undo's own thread: the caller may hold
    /// the workspace that admission reads.
    pub(crate) fn start(
        &self,
        broker: crate::shell::collab_broker::AppActionBroker,
        room: String,
        invocation: uuid::Uuid,
        task: String,
        steps: Vec<(String, String)>,
    ) -> Result<(), String> {
        {
            let mut results = self
                .results
                .lock()
                .map_err(|_| "task undo is unavailable")?;
            if results.contains_key(&invocation) {
                return Ok(());
            }
            while results.len() >= MAX_KEPT {
                let Some(oldest) = results
                    .iter()
                    .find(|(_, result)| result.is_some())
                    .map(|(id, _)| *id)
                else {
                    return Err("too many undos are running".into());
                };
                results.remove(&oldest);
            }
            results.insert(invocation, None);
        }
        let results = Arc::clone(&self.results);
        std::thread::Builder::new()
            .name("tilo-task-undo".into())
            .spawn(move || {
                let outcome = crate::shell::certified_action_session::CertifiedActionSession::open(
                    broker, &room, None,
                )
                .and_then(|session| {
                    let (host, caller) = session.door();
                    undo_task_run(host.as_ref(), &caller, &task, &steps, UNDO_TIMEOUT)
                })
                .inspect(|undone| {
                    tracing::info!(task, documents = undone.len(), "undid a task's run");
                });
                if let Ok(mut results) = results.lock() {
                    results.insert(invocation, Some(outcome));
                }
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Where undo `invocation` is; `None` when it is not one of these.
    pub(crate) fn poll(&self, invocation: uuid::Uuid) -> Option<UndoState> {
        let results = self.results.lock().ok()?;
        Some(match results.get(&invocation)? {
            None => UndoState::Running,
            Some(outcome) => UndoState::Ended(outcome.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(token: &str, app: &str, record: &str) -> CatalogueProvider {
        CatalogueProvider {
            provider: token.into(),
            app_id: app.into(),
            room: "room".into(),
            record_id: record.into(),
            browsable: true,
        }
    }

    #[test]
    fn a_run_is_undone_in_the_documents_it_changed() {
        let providers = [
            provider("1", "so.tilo.notes", "note-a"),
            provider("2", "so.tilo.notes", "note-b"),
            provider("3", "so.tilo.file-explorer", "files"),
            provider("so.tilo.sheets", "so.tilo.sheets", ""),
            CatalogueProvider {
                room: "elsewhere".into(),
                ..provider("4", "so.tilo.file-explorer", "files")
            },
        ];
        let steps = [
            ("so.tilo.notes".to_owned(), "note-b".to_owned()),
            (
                "so.tilo.file-explorer".to_owned(),
                "file:/home/a".to_owned(),
            ),
            ("so.tilo.sheets".to_owned(), String::new()),
        ];
        let tokens: Vec<String> = undo_targets(&providers, "room", &steps)
            .into_iter()
            .map(|target| target.provider.provider)
            .collect();
        // The note it changed only; Files, whose record is named
        // differently, by its open document in this room; Sheets is not
        // running, so nothing of it is asked.
        assert_eq!(tokens, ["2", "3"]);
    }

    /// Each document the run changed is reported under the step it was
    /// asked for, so the task view can say what came back and what did not.
    #[test]
    fn the_documents_undone_are_reported_by_their_steps() {
        let providers = [
            provider("1", "so.tilo.notes", "note-a"),
            provider("2", "so.tilo.sheets", "budget"),
        ];
        let steps = [
            ("so.tilo.notes".to_owned(), "note-a".to_owned()),
            ("so.tilo.sheets".to_owned(), "budget".to_owned()),
        ];
        let reported: Vec<(String, String)> = undo_targets(&providers, "room", &steps)
            .into_iter()
            .map(|target| (target.outcome.app, target.outcome.record))
            .collect();
        assert_eq!(reported, steps);
    }
}
