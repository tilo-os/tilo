//! Undoing an agent's run of a workspace task as one, from the activity.
//!
//! An agent working a task stamps every change with the task's group, so each document it changed
//! takes those changes back by naming the task: its history's `undo` with `{"run": task}`. That
//! goes through the document's own history, so edits others made since are kept and a step's
//! compensation runs. Each document the run changed is asked through the session's action door as
//! the person at the screen, on a thread of its own, so the activity's request answers at once.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::remote::{
    ActionHost, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, InvokeArgument,
    InvokeRequest, MAX_CATALOGUE_PAGE, ReceiptStatus, RemoteActionTarget, settle_with,
};
use tilo_desktop_surface::activity::TaskRunDocument;
use tilo_lock::MutexRecover as _;

/// How long one run's undo may take altogether.
const UNDO_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the door has to answer a request at all.
const ANSWER: Duration = Duration::from_millis(100);

/// The history verb every document publishes, and the argument naming a run.
const UNDO: &str = "undo";
const RUN: &str = "run";

/// The session's action door, as an undo asks it: the seat's own, or a test's.
pub(crate) trait UndoDoor: Send + Sync {
    /// One page of what `caller` is offered now.
    fn page(&self, caller: &Caller, query: CatalogueQuery) -> Option<CataloguePage>;

    /// Run `request` as the person at the screen, waiting until it settles.
    fn run(&self, caller: &Caller, request: InvokeRequest, until: Instant) -> Result<(), String>;
}

impl UndoDoor for crate::agent_door::Host {
    fn page(&self, caller: &Caller, query: CatalogueQuery) -> Option<CataloguePage> {
        self.query(caller, query).wait_timeout(ANSWER).ok()??.ok()
    }

    fn run(&self, caller: &Caller, request: InvokeRequest, until: Instant) -> Result<(), String> {
        let invocation = request.invocation;
        let left = until.saturating_duration_since(Instant::now());
        let receipt = match self
            .invoke_at_seat(caller, request)
            .wait_timeout(left.max(ANSWER))
        {
            Ok(Some(Ok(receipt))) => receipt,
            Ok(Some(Err(error))) => return Err(error.to_string()),
            _ => return Err("the document did not answer".into()),
        };
        if receipt.status == ReceiptStatus::Accepted {
            let host = self.clone();
            return match settle_with(&host, caller, invocation, None, until)
                .wait_timeout(left.max(ANSWER))
            {
                Ok(Some(Some(state))) => settled(&state.receipt().status),
                _ => Err("the document did not finish undoing in time".into()),
            };
        }
        settled(&receipt.status)
    }
}

/// How an invocation ended.
fn settled(status: &ReceiptStatus) -> Result<(), String> {
    match status {
        ReceiptStatus::Completed { .. } => Ok(()),
        ReceiptStatus::Failed { error } => Err(error.message.clone()),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Cancelled { .. } => Err("cancelled".into()),
        ReceiptStatus::Accepted => Err("still running".into()),
    }
}

/// Undo task `task`'s run in each document `steps` names (its app and record), through `door` as
/// `caller`: one [`TaskRunDocument`] per document the run changed, in the order `steps` names them,
/// so a record nobody reverted is reported rather than dropped.
///
/// # Errors
/// Nothing was undone: no document the run changed is open, or each refused. A run undone in some
/// documents but not others answers those that were, with the refusals logged.
pub(crate) fn undo_task_run(
    door: &dyn UndoDoor,
    caller: &Caller,
    task: &str,
    steps: &[(String, String)],
    timeout: Duration,
) -> Result<Vec<TaskRunDocument>, String> {
    let now = Instant::now();
    let deadline = now.checked_add(timeout).unwrap_or(now);
    let providers = door
        .page(
            caller,
            CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            },
        )
        .ok_or("the action door did not answer")?
        .providers;
    let targets = undo_targets(&providers, caller.workspace().unwrap_or_default(), steps);
    if targets.is_empty() {
        return Err("open the documents this task changed to undo it".into());
    }
    let mut undone: usize = 0;
    let mut refused = Vec::new();
    for target in &targets {
        let Some(address) = undo_address(door, caller, &target.provider.provider) else {
            refused.push(format!("{} has no history to undo", target.provider.app_id));
            continue;
        };
        let mut args = serde_json::Map::new();
        args.insert(RUN.into(), task.into());
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: target.provider.provider.clone(),
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
        match door.run(caller, request, deadline) {
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

/// One document the task's run changed, and the publisher that serves it.
struct UndoTarget {
    provider: CatalogueProvider,
    /// The step as the run recorded it, which the outcome is reported under.
    outcome: TaskRunDocument,
}

/// The live publishers of the documents `steps` names in `workspace`: a step's record where its
/// app publishes it, else every open document of that app — undo by run changes nothing in a
/// document the run did not touch.
fn undo_targets(
    providers: &[CatalogueProvider],
    workspace: &str,
    steps: &[(String, String)],
) -> Vec<UndoTarget> {
    let live = |provider: &&CatalogueProvider| {
        provider.workspace == workspace && provider.provider.parse::<u64>().is_ok()
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
                outcome: TaskRunDocument {
                    app: app.clone(),
                    record: record.clone(),
                },
            });
        }
    }
    targets
}

/// The address of the history `undo` that provider `provider` publishes.
fn undo_address(
    door: &dyn UndoDoor,
    caller: &Caller,
    provider: &str,
) -> Option<tilo_actions::ActionAddress> {
    door.page(
        caller,
        CatalogueQuery {
            provider: Some(provider.to_owned()),
            needle: UNDO.to_owned(),
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        },
    )?
    .rows
    .into_iter()
    .filter_map(|row| row.action)
    .find(|action| {
        action.address.action == UNDO && action.params.iter().any(|param| param.name == RUN)
    })
    .map(|action| action.address)
}

/// Undoes run by run on a thread of its own, so the activity's request answers at once and its
/// outcome is polled. An outcome is taken once.
#[derive(Clone, Default)]
pub(crate) struct Undos {
    running: Arc<Mutex<Outcomes>>,
}

/// Each undo by its request: `None` while it runs, then how it ended.
type Outcomes = BTreeMap<uuid::Uuid, Option<Result<Vec<TaskRunDocument>, String>>>;

/// Where one undo is.
pub(crate) enum UndoState {
    Running,
    Ended(Result<Vec<TaskRunDocument>, String>),
}

impl Undos {
    /// Start undoing `task`'s run in `workspace` as `person`, through `door`; `done` is told once it
    /// ended, on its thread.
    pub(crate) fn start(
        &self,
        door: Arc<dyn UndoDoor>,
        person: String,
        workspace: String,
        invocation: uuid::Uuid,
        task: String,
        steps: Vec<(String, String)>,
        done: impl FnOnce() + Send + 'static,
    ) -> Result<(), String> {
        self.running.lock_recover().insert(invocation, None);
        let running = Arc::clone(&self.running);
        std::thread::Builder::new()
            .name("tilo-task-undo".into())
            .spawn(move || {
                // Opening the door reads the workspace the pane's caller may hold, so the caller is
                // the undo's own.
                let caller = Caller::local_person(person)
                    .in_workspace(workspace)
                    .in_new_execution();
                let outcome = undo_task_run(door.as_ref(), &caller, &task, &steps, UNDO_TIMEOUT);
                running.lock_recover().insert(invocation, Some(outcome));
                done();
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Where undo `invocation` is; `None` when it is not one of these.
    pub(crate) fn poll(&self, invocation: uuid::Uuid) -> Option<UndoState> {
        let mut running = self.running.lock_recover();
        let outcome = running.get(&invocation)?;
        let Some(ended) = outcome.clone() else {
            return Some(UndoState::Running);
        };
        running.remove(&invocation);
        drop(running);
        Some(UndoState::Ended(ended))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable};
    use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc};
    use tilo_actions::remote::CatalogueRow;
    use tilo_actions::{ActionContext, ResourceSpec};
    use ui::history_source::ThreadWords;

    const APP: &str = "so.tilo.notes";
    const RECORD: &str = "file:/notes/release.note";
    const WORKSPACE: &str = "workspace:1";
    const PROVIDER: &str = "7";
    const TASK: &str = "task:1";

    /// A note document served as its app publishes it: the history verbs it offers, and the run
    /// its `undo` takes back.
    struct FakeDoor {
        doc: Mutex<AutoCommit>,
        /// Whether the app publishes its history verbs.
        history: bool,
        /// Whether the record is open here at all.
        open: bool,
    }

    impl UndoDoor for FakeDoor {
        fn page(&self, caller: &Caller, query: CatalogueQuery) -> Option<CataloguePage> {
            let mut page = CataloguePage::default();
            if query.max_results == 0 {
                if self.open && caller.workspace() == Some(WORKSPACE) {
                    page.providers = vec![CatalogueProvider {
                        provider: PROVIDER.into(),
                        app_id: APP.into(),
                        workspace: WORKSPACE.into(),
                        record_id: RECORD.into(),
                        browsable: false,
                    }];
                }
                return Some(page);
            }
            if self.history {
                let mut spec = ResourceSpec::new(RECORD, "note", "Release notes");
                spec.actions = ui::history_source::verbs::specs();
                page.rows = tilo_actions::door::survey(&[spec])
                    .into_iter()
                    .map(|action| CatalogueRow::for_action(PROVIDER, APP, action))
                    .collect();
            }
            Some(page)
        }

        fn run(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
            _until: Instant,
        ) -> Result<(), String> {
            let InvokeArgument::Json(args) = request.argument else {
                return Err("the undo names no run".into());
            };
            let edit = ui::history_source::verbs::HistoryEdit::Undo {
                steps: None,
                run: args
                    .get(RUN)
                    .and_then(|run| run.as_str())
                    .map(str::to_owned),
                by: None,
                to: None,
                step: None,
            };
            let mut doc = self.doc.lock_recover();
            edit.apply_registered(&mut doc, |_| {}, &ThreadWords)
                .map_err(|error| error.to_string())?;
            doc.commit();
            drop(doc);
            Ok(())
        }
    }

    /// A note with one sentence, and an agent's run appending to it.
    fn edited() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(b"person".as_slice()));
        let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "Shipping notes.").unwrap();
        doc.commit();
        doc.set_actor(ActorId::from(b"agent".as_slice()));
        doc.splice_text(&body, 15, 0, " Sent.").unwrap();
        let message = ActionContext::default()
            .with_group(
                TASK,
                "Ship the release",
                ui::history_source::steps::AGENT_RUN,
            )
            .change_message()
            .unwrap();
        doc.commit_with(CommitOptions::default().with_message(message));
        (doc, body)
    }

    fn door(doc: AutoCommit, history: bool, open: bool) -> FakeDoor {
        FakeDoor {
            doc: Mutex::new(doc),
            history,
            open,
        }
    }

    fn caller() -> Caller {
        Caller::local_person("person")
            .in_workspace(WORKSPACE)
            .in_new_execution()
    }

    fn undo(door: &FakeDoor) -> Result<Vec<TaskRunDocument>, String> {
        undo_task_run(
            door,
            &caller(),
            TASK,
            &[(APP.to_owned(), RECORD.to_owned())],
            Duration::from_secs(5),
        )
    }

    /// The run's change is taken back by the document's own history, and the document it was
    /// undone in is reported.
    #[test]
    fn a_tasks_run_is_undone_in_the_document_it_changed() {
        let (doc, body) = edited();
        let door = door(doc, true, true);
        assert_eq!(
            undo(&door).expect("the document publishes its history"),
            vec![TaskRunDocument {
                app: APP.into(),
                record: RECORD.into(),
            }]
        );
        assert_eq!(
            door.doc.lock_recover().text(&body).unwrap(),
            "Shipping notes."
        );
    }

    /// The host cannot undo a run it cannot reach or that publishes no history: it says so, and
    /// writes nothing.
    #[test]
    fn a_run_no_open_document_can_take_back_is_refused_by_name() {
        let (doc, _) = edited();
        let refused = undo(&door(doc.clone(), true, false)).unwrap_err();
        assert!(refused.contains("open the documents"), "{refused}");
        let refused = undo(&door(doc, false, true)).unwrap_err();
        assert!(
            refused.contains(APP) && refused.contains("history"),
            "{refused}"
        );
    }
}
