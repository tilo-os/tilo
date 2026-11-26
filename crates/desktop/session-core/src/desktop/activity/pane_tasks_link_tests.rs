use super::*;
use crate::desktop::activity::TaskRunDocument;
use crate::desktop::activity::testing::TestTree as WorkspaceTree;
use ui::resource::ResourceStore;
#[derive(Default)]
pub(super) struct UndoHost {
    pub(super) undos: RefCell<Vec<TaskRunUndo>>,
    ended: RefCell<Option<Result<Vec<TaskRunDocument>, String>>>,
}
impl ActivityHost for UndoHost {
    fn local_user(&self) -> String {
        "local".into()
    }
    fn mutations_available(&self) -> bool {
        true
    }
    fn undo_task_run(&self, undo: TaskRunUndo) -> Result<(), String> {
        self.undos.borrow_mut().push(undo);
        Ok(())
    }
    fn poll_task_run_undo(
        &self,
        _invocation: uuid::Uuid,
    ) -> Option<Result<Vec<TaskRunDocument>, String>> {
        self.ended.borrow_mut().take()
    }
}
fn fixture() -> (WorkspaceTree, WorkspaceActivityPane, Rc<UndoHost>) {
    let mut tree = WorkspaceTree::bootstrap_default();
    let host = Rc::new(UndoHost::default());
    let mut pane = WorkspaceActivityPane::new();
    pane.set_host(host.clone());
    pane.refresh(&mut tree);
    pane.view = ActivityView::Explore;
    (tree, pane, host)
}

/// Once an agent's run of a task recorded what it changed under the task, undoing it asks the
/// host to undo the run in each changed document, and tells how that ended.
#[test]
fn an_agent_sessions_task_is_undone_by_the_host_as_one() {
    let (mut tree, mut pane, host) = fixture();
    let change = ui::history_source::entry_id_from_hash(&"ab".repeat(32));
    let doc = tree.document_mut();
    let key = ui::record_invocation_with_details(
        doc,
        "Review release notes",
        Some("agent:scribe"),
        Some("local"),
        None,
        None,
        &[ui::InvocationStep {
            app: "so.tilo.notes".into(),
            record: "file:/notes/release.note".into(),
            range: ui::ChangeRange::new(change, change),
            what: Vec::new(),
        }],
    )
    .unwrap();
    assert!(ui::history_source::invocations::set_invocation_task(
        doc, &key, "task:1"
    ));
    doc.commit();
    pane.refresh_records(&mut tree, &ResourceStore::in_memory());

    assert!(pane.undo_task_run("task:1"));
    let undo = host.undos.borrow().last().cloned().unwrap();
    assert_eq!(undo.task_id, "task:1");
    assert_eq!(
        undo.workspace,
        tree.active_workspace().unwrap().id().to_string()
    );
    assert_eq!(
        undo.steps,
        vec![(
            "so.tilo.notes".to_owned(),
            "file:/notes/release.note".to_owned()
        )]
    );
    assert!(pane.undo_task_run("task:1"));
    assert_eq!(host.undos.borrow().len(), 1, "one undo at a time");

    host.ended.replace(Some(Ok(vec![TaskRunDocument {
        app: "so.tilo.notes".into(),
        record: "file:/notes/release.note".into(),
    }])));
    pane.refresh(&mut tree);
    assert!(pane.undo_pending.is_none());
    assert_eq!(
        pane.agent_undo_notice
            .as_ref()
            .map(|(_, notice)| notice.clone()),
        Some(t("activity-undo-task-done"))
    );
    assert!(pane.unreverted.is_empty(), "every record came back");
}

/// Each record of the run can be undone on its own from the task view: the host is asked for
/// the task, in that one document only.
#[test]
fn one_record_of_a_run_is_undone_alone() {
    let (mut tree, mut pane, host) = fixture();
    let change = ui::history_source::entry_id_from_hash(&"ab".repeat(32));
    let doc = tree.document_mut();
    let key = ui::record_invocation_with_details(
        doc,
        "Review release notes",
        Some("agent:scribe"),
        Some("local"),
        None,
        None,
        &[
            ui::InvocationStep {
                app: "so.tilo.notes".into(),
                record: "file:/notes/release.note".into(),
                range: ui::ChangeRange::new(change, change),
                what: Vec::new(),
            },
            ui::InvocationStep {
                app: "so.tilo.sheets".into(),
                record: "file:/sheets/budget.tilo-sheet".into(),
                range: ui::ChangeRange::new(change, change),
                what: Vec::new(),
            },
        ],
    )
    .unwrap();
    assert!(ui::history_source::invocations::set_invocation_task(
        doc, &key, "task:1"
    ));
    doc.commit();
    pane.refresh_records(&mut tree, &ResourceStore::in_memory());

    assert!(pane.undo_task_record(
        "task:1",
        "so.tilo.sheets",
        "file:/sheets/budget.tilo-sheet",
        None
    ));
    let undo = host.undos.borrow().last().cloned().unwrap();
    assert_eq!(undo.task_id, "task:1");
    assert_eq!(
        undo.steps,
        vec![(
            "so.tilo.sheets".to_owned(),
            "file:/sheets/budget.tilo-sheet".to_owned()
        )],
        "only the record the person picked is undone"
    );
}

/// The agent composer follows the task, not the history: it names the run by its task, and the
/// same one record is undone, nothing else; a task that changed nothing is refused.
#[test]
fn one_record_of_a_run_is_undone_by_its_task() {
    use crate::desktop::activity::vocabulary as wire;
    let (mut tree, mut pane, host) = fixture();
    let change = ui::history_source::entry_id_from_hash(&"ef".repeat(32));
    let doc = tree.document_mut();
    let key = ui::record_invocation_with_details(
        doc,
        "Move the card to Done",
        Some("agent:scribe"),
        Some("local"),
        None,
        None,
        &[
            ui::InvocationStep {
                app: "so.tilo.notes".into(),
                record: "file:/notes/release.note".into(),
                range: ui::ChangeRange::new(change, change),
                what: Vec::new(),
            },
            ui::InvocationStep {
                app: "so.tilo.lang.board".into(),
                record: String::new(),
                range: ui::ChangeRange::new(change, change),
                what: Vec::new(),
            },
        ],
    )
    .unwrap();
    assert!(ui::history_source::invocations::set_invocation_task(
        doc,
        &key,
        "tasks.tracker/issues/release"
    ));
    doc.commit();
    pane.refresh_records(&mut tree, &ResourceStore::in_memory());

    let query = |task: &str| {
        serde_json::to_value(wire::ResourceQuery {
            task: Some(task.into()),
            app_id: "so.tilo.lang.board".into(),
            ..wire::ResourceQuery::default()
        })
        .unwrap()
        .as_object()
        .cloned()
        .unwrap()
    };
    let outcome = pane
        .serve_action(
            wire::UNDO_RECORD,
            &query("tasks.tracker/issues/release"),
            &mut tree,
        )
        .unwrap();
    assert!(outcome.handled);
    let undo = host.undos.borrow().last().cloned().unwrap();
    assert_eq!(undo.task_id, "tasks.tracker/issues/release");
    assert_eq!(
        undo.steps,
        vec![("so.tilo.lang.board".to_owned(), String::new())],
        "only that app's record of the task is undone"
    );
    assert!(
        pane.serve_action(
            wire::UNDO_RECORD,
            &query("tasks.tracker/issues/other"),
            &mut tree
        )
        .is_err()
    );
}

/// A record the host did not undo is named, not dropped: the task view says what did not come
/// back.
#[test]
fn a_record_undo_did_not_reach_is_reported() {
    let (mut tree, mut pane, host) = fixture();
    let change = ui::history_source::entry_id_from_hash(&"cd".repeat(32));
    let doc = tree.document_mut();
    let key = ui::record_invocation_with_details(
        doc,
        "Review release notes",
        Some("agent:scribe"),
        Some("local"),
        None,
        None,
        &[
            ui::InvocationStep {
                app: "so.tilo.notes".into(),
                record: "file:/notes/release.note".into(),
                range: ui::ChangeRange::new(change, change),
                what: Vec::new(),
            },
            ui::InvocationStep {
                app: "so.tilo.sheets".into(),
                record: "file:/sheets/budget.tilo-sheet".into(),
                range: ui::ChangeRange::new(change, change),
                what: Vec::new(),
            },
        ],
    )
    .unwrap();
    assert!(ui::history_source::invocations::set_invocation_task(
        doc, &key, "task:1"
    ));
    doc.commit();
    pane.refresh_records(&mut tree, &ResourceStore::in_memory());

    assert!(pane.undo_task_run("task:1"));
    // The host reached only the notes document.
    host.ended.replace(Some(Ok(vec![TaskRunDocument {
        app: "so.tilo.notes".into(),
        record: "file:/notes/release.note".into(),
    }])));
    pane.poll_undo();
    assert_eq!(
        pane.agent_undo_notice
            .as_ref()
            .map(|(_, notice)| notice.clone()),
        Some(t("activity-undo-task-partial-records"))
    );
    assert_eq!(pane.unreverted.len(), 1, "the sheets record is named");
    assert!(
        pane.unreverted[0].contains("so.tilo.sheets"),
        "the record's app is said: {:?}",
        pane.unreverted
    );
}

/// A run the host cannot undo says so, in the host's words, and reports no record as come
/// back.
#[test]
fn a_run_the_host_cannot_undo_is_reported_as_refused() {
    let (mut tree, mut pane, host) = fixture();
    let change = ui::history_source::entry_id_from_hash(&"12".repeat(32));
    let doc = tree.document_mut();
    let key = ui::record_invocation_with_details(
        doc,
        "Review release notes",
        Some("agent:scribe"),
        Some("local"),
        None,
        None,
        &[ui::InvocationStep {
            app: "so.tilo.notes".into(),
            record: "file:/notes/release.note".into(),
            range: ui::ChangeRange::new(change, change),
            what: Vec::new(),
        }],
    )
    .unwrap();
    assert!(ui::history_source::invocations::set_invocation_task(
        doc, &key, "task:1"
    ));
    doc.commit();
    pane.refresh_records(&mut tree, &ResourceStore::in_memory());

    assert!(pane.undo_task_run("task:1"));
    let why = "open the documents this task changed to undo it";
    host.ended.replace(Some(Err(why.into())));
    pane.poll_undo();
    assert_eq!(
        pane.agent_undo_notice
            .as_ref()
            .map(|(_, notice)| notice.clone()),
        Some(crate::desktop::i18n::t_args(
            "activity-undo-task-refused",
            &[("detail", why.into())]
        ))
    );
    assert!(pane.unreverted.is_empty(), "nothing came back");
}
