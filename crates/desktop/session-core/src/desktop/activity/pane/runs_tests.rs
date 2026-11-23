//! An agent connection's run read as the timeline of what it did: the command lines it asked a
//! shell session to run, each with the run that session recorded, the command still running as
//! what it is doing now, and the whole run taken back through each document's own history.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};
use tilo_actions::remote::workspace_activity as wire;

use super::TERMINAL_APP;
use crate::desktop::activity::index::DetachedRecordHistory;
use crate::desktop::activity::pane::{ActivityView, WorkspaceActivityPane};
use crate::desktop::activity::testing::{TestHost, TestTree as WorkspaceTree};
use crate::desktop::activity::{ActivityHost, TaskRunDocument, TaskRunUndo};
use crate::desktop::i18n::t;

const OUTPUT_HASH: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
const OUTPUT: &[u8] = b"\x1b[1m   Compiling\x1b[0m signup v0.1.0\r\nrunning 2 tests\r\ntest signs_up ... ok\r\ntest rejects_taken_names ... FAILED\r\n\r\ntest result: FAILED. 1 passed; 1 failed\r\n";

/// The test host, with the content a session's runs keep their output in and the undos it carries
/// out for the activity.
#[derive(Default)]
struct Host {
    inner: TestHost,
    content: RefCell<HashMap<(String, String, String), Vec<u8>>>,
    undos: RefCell<Vec<TaskRunUndo>>,
    ended: RefCell<Option<Result<Vec<TaskRunDocument>, String>>>,
}

impl ActivityHost for Host {
    fn local_user(&self) -> String {
        self.inner.local_user()
    }
    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        self.inner.local_author(authors, actor)
    }
    fn document_content(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.content
            .borrow()
            .get(&(workspace.to_owned(), app.to_owned(), hash.to_owned()))
            .cloned()
    }
    fn mutations_available(&self) -> bool {
        self.inner.mutations_available()
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

/// A collection's entry as a model stores it: `{k: order key, v: value}`.
fn entry(doc: &mut AutoCommit, collection: &str, id: &str, order: &str) -> ObjId {
    let collection = match doc.get(ROOT, collection).unwrap() {
        Some((automerge::Value::Object(_), collection)) => collection,
        _ => doc.put_object(ROOT, collection, ObjType::Map).unwrap(),
    };
    let stored = doc.put_object(&collection, id, ObjType::Map).unwrap();
    doc.put(&stored, "k", order).unwrap();
    doc.put_object(&stored, "v", ObjType::Map).unwrap()
}

/// The agent's `run`: a request queued in the session's record, as one change of its run.
fn ask(
    doc: &mut AutoCommit,
    record: &str,
    id: &str,
    order: &str,
    command: &str,
    group: &str,
    at: i64,
) {
    let request = entry(doc, "requests", id, order);
    let value = doc.put_object(&request, "value", ObjType::Map).unwrap();
    doc.put(&value, "command", command).unwrap();
    doc.put(&value, "requested_by", "agent:claude-code")
        .unwrap();
    doc.put(&value, "agent", true).unwrap();
    let context = tilo_actions::ActionContext::for_resource(record, "terminal.session", "Terminal")
        .with_app(TERMINAL_APP, "Terminal")
        .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(TERMINAL_APP, "terminal.session", "run"),
            [(
                "command".to_owned(),
                tilo_actions::entry::Scalar::argument(&serde_json::json!(command)).unwrap(),
            )],
        )))
        .with_group(group, "Fix the signup tests", "agent-session");
    doc.commit_with(
        CommitOptions::default()
            .with_time(at)
            .with_message(context.change_message().unwrap()),
    );
}

/// The terminal's own record of the run the request became, and its answer to the request.
fn record_invocation(doc: &mut AutoCommit, id: &str, exit: i64, at: i64) {
    let run = entry(doc, "runs", id, "a0");
    doc.put(&run, "command", "cargo test -p signup").unwrap();
    doc.put(&run, "cwd", "/home/ada/signup").unwrap();
    doc.put(&run, "started_ms", at.saturating_mul(1000))
        .unwrap();
    doc.put(&run, "ended_ms", at.saturating_add(3).saturating_mul(1000))
        .unwrap();
    doc.put(&run, "exit", exit).unwrap();
    doc.put(
        &run,
        "output",
        format!("blake3:{OUTPUT_HASH}:{}", OUTPUT.len()),
    )
    .unwrap();
    doc.put(&run, "truncated", false).unwrap();
    let Some((_, requests)) = doc.get(ROOT, "requests").unwrap() else {
        panic!("the request was queued");
    };
    let Some((_, stored)) = doc.get(&requests, id).unwrap() else {
        panic!("the request is in the queue");
    };
    let Some((_, value)) = doc.get(&stored, "v").unwrap() else {
        panic!("the request has its value");
    };
    doc.put(
        &value,
        "settled",
        serde_json::json!({"state": "done", "result": {"invocation": id, "exit": exit}})
            .to_string(),
    )
    .unwrap();
    doc.commit_with(CommitOptions::default().with_time(at.saturating_add(3)));
}

struct Fixture {
    tree: WorkspaceTree,
    pane: WorkspaceActivityPane,
    doc: AutoCommit,
    checkout: ui::DocumentCheckout,
}

impl Fixture {
    fn new() -> Self {
        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active_workspace().expect("workspace").id();
        let record = ui::DocumentId::for_tab(7);
        let checkout =
            ui::DocumentCheckout::new(record.clone(), ui::ResourceType::new("terminal.session"));
        let mut doc = AutoCommit::new();
        ui::resource::init(&mut doc, &record, &checkout.kind, &workspace.to_string());
        doc.commit();
        let host = Host::default();
        host.content.borrow_mut().insert(
            (
                workspace.to_string(),
                TERMINAL_APP.to_owned(),
                OUTPUT_HASH.to_owned(),
            ),
            OUTPUT.to_vec(),
        );
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(Rc::new(host));
        pane.view = ActivityView::Explore;
        let mut fixture = Self {
            tree,
            pane,
            doc,
            checkout,
        };
        fixture
            .tree
            .desktop
            .workspace_records_held
            .entry(workspace)
            .or_default()
            .insert(record.to_string(), "terminal.session".to_owned());
        fixture
    }

    fn record(&self) -> String {
        self.checkout.id.to_string()
    }

    fn as_agent(&mut self) {
        self.doc
            .set_actor(ActorId::from(b"claude-code-agent".as_slice()));
    }

    fn as_terminal(&mut self) {
        self.doc.set_actor(ActorId::from(b"ada-laptop".as_slice()));
    }

    fn sessions(&mut self) -> wire::Sessions {
        self.pane.refresh(&mut self.tree);
        self.pane.refresh_detached_records(
            &mut self.tree,
            vec![DetachedRecordHistory {
                app: TERMINAL_APP.to_owned(),
                checkout: self.checkout.clone(),
                tree_chunks: Vec::new(),
                document: Some(self.doc.clone()),
            }],
        );
        self.pane
            .serve_runs(&wire::SessionsQuery::default(), &self.tree)
    }
}

tilo_test_env::scenario! {
    fn agent_and_connection_runs_are_listed_and_undone_from_the_activity;

    phase an_agent_runs_commands_show_their_run_records_and_the_one_running_now;
    // An MCP session's run that asked a shell to run a command shows that command as the shell
    // recorded it: its directory, how it ended, and the last lines it printed, with the whole output
    // one read away. A command still running is what the run is doing now.
    {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        fixture.as_agent();
        ask(
            &mut fixture.doc,
            &record,
            "req-1",
            "a0",
            "cargo test -p signup",
            "mcp:session-2",
            10,
        );
        fixture.as_terminal();
        record_invocation(&mut fixture.doc, "req-1", 101, 11);
        fixture.as_agent();
        ask(
            &mut fixture.doc,
            &record,
            "req-2",
            "a1",
            "cargo test -p signup -- rejects_taken_names",
            "mcp:session-2",
            20,
        );

        let served = fixture.sessions();
        let run = served
            .sessions
            .iter()
            .find(|run| run.id == "mcp:session-2")
            .unwrap_or_else(|| panic!("the session's work is one run: {served:#?}"));
        assert_eq!(run.instruction, "Fix the signup tests", "listed by what it was asked");
        assert_eq!(run.agent.relation, wire::Relation::Software);
        assert_eq!(run.actions.len(), 2, "{:#?}", run.actions);

        let wire::ActionView::Command {
            command,
            cwd,
            output,
            exit,
            started_ms,
            ended_ms,
            execution: recorded,
            ..
        } = &run.actions[0].view
        else {
            panic!(
                "a command it ran opens as that command: {:?}",
                run.actions[0]
            );
        };
        assert_eq!(command, "cargo test -p signup");
        assert_eq!(cwd, "/home/ada/signup");
        assert_eq!(*exit, Some(101));
        assert_eq!((*started_ms, *ended_ms), (Some(11_000), Some(14_000)));
        assert_eq!(
            output.last().map(String::as_str),
            Some("test result: FAILED. 1 passed; 1 failed")
        );
        assert!(
            output.iter().all(|line| !line.contains('\u{1b}')),
            "{output:?}"
        );
        let recorded = recorded.clone().expect("the run record is named");
        assert_eq!(
            (
                recorded.app_id.as_str(),
                recorded.record.as_str(),
                recorded.execution.as_str()
            ),
            (TERMINAL_APP, record.as_str(), "req-1")
        );
        assert!(!run.actions[0].view.is_running());

        assert!(run.actions[1].view.is_running(), "{:?}", run.actions[1]);
        assert_eq!(
            run.now,
            Some(wire::Now::Running {
                command: "cargo test -p signup -- rejects_taken_names".to_owned(),
                since_ms: run.actions[1].at_ms,
            })
        );

        let whole = fixture.pane.serve_run_output(&recorded, &fixture.tree);
        assert!(!whole.unavailable && !whole.running);
        assert_eq!(
            whole.lines.first().map(String::as_str),
            Some("   Compiling signup v0.1.0")
        );
        assert_eq!(whole.lines.len(), 6, "{:?}", whole.lines);

        // The second command ends: the run did that last, and is running nothing.
        fixture.as_terminal();
        record_invocation(&mut fixture.doc, "req-2", 0, 30);
        let served = fixture.sessions();
        let run = served
            .sessions
            .iter()
            .find(|run| run.id == "mcp:session-2")
            .expect("the run is still there");
        assert!(run.actions.iter().all(|action| !action.view.is_running()));
        assert!(
            matches!(run.now, Some(wire::Now::Ran { .. })),
            "{:?}",
            run.now
        );
    }

    phase a_connections_run_is_undone_whole_while_its_documents_are_open;
    // An MCP session's run over the documents it changed is undone whole: each document's own
    // history takes back what it grouped under the connection, while a command it asked a shell to
    // run is no document's to take back. A record that is not open here is what blocks it.
    {
        let mut connection = Connection::new(&["release.note", "budget.note"]);
        connection.in_a_shell("cargo test -p signup", 30);
        let run = connection.run();
        assert_eq!(run.actions.len(), 3, "two documents and one command");
        assert_eq!(run.undo_blocked, None, "every document it changed is open");
        let offered = |run: &wire::AgentSession| {
            run.actions
                .iter()
                .filter_map(|action| action.undo.as_ref().map(|undo| undo.action))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            offered(&run),
            vec![wire::DetailAction::UndoEntry; 2],
            "each document undoes on its own; the command leaves no history"
        );
        let detail = connection.pane.serve_detail(
            &wire::DetailQuery {
                range: run.range,
                compare: false,
            },
            &mut connection.tree,
        );
        assert!(!detail.gone);
        assert!(
            detail.offers.iter().any(|offer| {
                offer.action == wire::DetailAction::UndoSession
                    && offer.label == t("activity-detail-undo-run")
            }),
            "the run is offered whole under its own name, not a task's: {:?}",
            detail.offers
        );

        // The budget note is busy elsewhere: the run cannot be taken back whole until it is open.
        connection.closed.push(Connection::record("budget.note"));
        let run = connection.run();
        assert_eq!(
            run.undo_blocked.as_deref(),
            Some(t("activity-undo-run-needs-open").as_str())
        );
        assert_eq!(
            offered(&run),
            vec![wire::DetailAction::UndoEntry],
            "the document still open undoes on its own"
        );
    }

    phase undoing_a_connections_run_whole_names_its_group_in_every_document;
    // Undoing an MCP session's run whole asks the host for the connection's group in each document
    // it changed, and its outcome is reported beside the run the way a task's is.
    {
        let mut connection = Connection::new(&["release.note", "budget.note"]);
        connection.in_a_shell("cargo test -p signup", 30);
        let run = connection.run();
        let query = serde_json::to_value(&run.range)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap();
        let outcome = connection
            .pane
            .serve_action(wire::UNDO_SESSION, &query, &mut connection.tree)
            .expect("the run is in the history");
        assert!(outcome.handled);
        let undo = connection.undo();
        assert_eq!(undo.task_id, GROUP);
        assert_eq!(
            undo.steps,
            vec![
                (NOTES_APP.to_owned(), Connection::record("release.note")),
                (NOTES_APP.to_owned(), Connection::record("budget.note")),
            ],
            "each document it changed, and no shell session's record"
        );
        assert_eq!(
            undo.workspace,
            connection
                .tree
                .active_workspace()
                .expect("workspace")
                .id()
                .to_string()
        );

        // One undo at a time: asking again while it runs asks nothing more, and the run says so.
        connection
            .pane
            .serve_action(wire::UNDO_SESSION, &query, &mut connection.tree)
            .expect("the run is still in the history");
        assert_eq!(connection.host.undos.borrow().len(), 1);
        assert_eq!(
            connection.run().undo_blocked.as_deref(),
            Some(t("activity-undo-run-in-progress").as_str())
        );

        let ended = undo
            .steps
            .iter()
            .map(|(app, record)| TaskRunDocument {
                app: app.clone(),
                record: record.clone(),
            })
            .collect();
        connection.host.ended.replace(Some(Ok(ended)));
        connection.refresh();
        assert!(connection.pane.undo_pending.is_none(), "it ended");
        assert_eq!(
            connection
                .pane
                .agent_undo_notice
                .as_ref()
                .map(|(run, notice)| (run.as_str(), notice.as_str())),
            Some((GROUP, t("activity-undo-task-done").as_str()))
        );
        assert!(connection.pane.unreverted.is_empty());
    }

    phase one_document_of_a_connections_run_undoes_alone;
    // One document of an MCP session's run undoes on its own: the host is asked for the connection's
    // group in that one record, exactly as the run view's Undo this step asks.
    {
        let mut connection = Connection::new(&["release.note", "budget.note"]);
        let run = connection.run();
        let action = &run.actions[0];
        assert_eq!(
            action.undo.as_ref().map(|undo| undo.action),
            Some(wire::DetailAction::UndoEntry)
        );
        let query = wire::ResourceQuery {
            range: run.range.clone(),
            task: None,
            app_id: action.app_id.clone(),
            id: action.record.clone(),
        };
        let arguments = serde_json::to_value(&query)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap();
        let outcome = connection
            .pane
            .serve_action(wire::UNDO_RECORD, &arguments, &mut connection.tree)
            .expect("the run is in the history");
        assert!(outcome.handled);
        let undo = connection.undo();
        assert_eq!(undo.task_id, GROUP);
        assert_eq!(
            undo.steps,
            vec![(NOTES_APP.to_owned(), Connection::record("release.note"))],
            "only the record the person picked is undone"
        );
    }

    phase an_agents_run_in_a_record_the_session_keeps_is_listed_and_undone_from_the_activity;
    // The workspace record the session keeps holds a root per package it runs, and each app's record
    // is admitted over that one document. The app's own steps are its entry's and no other's, and an
    // agent's run there is listed — and taken back from the Activity — though no app's process holds
    // the record and no marker holds the run.
    {
        const NOTES: &str = "so.tilo.notes";
        const NOTES_KIND: &str = "tl.so.tilo.notes.Notebook";
        const NOTES_ROOT: &str = "tl:so.tilo.notes:notebook";
        const CHAT_KIND: &str = "tl.so.tilo.chat.Channel";
        const CHAT_ROOT: &str = "tl:so.tilo.chat:channel";
        // The run an agent admitted at this desktop's API works in: one run of a task.
        const RUN: &str =
            "so.tilo.session/agent-session/243b7256ef6c400fa43d111a4988b8ca@ad46de5895801c1c";
        // The person names a session-kept record as the app's own record of the workspace.
        let primary = ui::DocumentId::package_primary().as_str().to_owned();

        let tree = WorkspaceTree::bootstrap_demo();
        let host = Rc::new(Host::default());
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(Rc::clone(&host) as Rc<dyn ActivityHost>);
        pane.view = ActivityView::Explore;

        // One document, two apps' records: the agent renamed a note in one and wrote in the other.
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(b"claude-code-agent".as_slice()));
        let note = doc.put_object(ROOT, "note", ObjType::Text).unwrap();
        doc.splice_text(&note, 0, 0, "Shopping").unwrap();
        let context = tilo_actions::ActionContext::for_resource(NOTES_ROOT, NOTES_KIND, "Shopping")
            .with_group(RUN, "Claude Code", ui::history_source::steps::AGENT_SESSION);
        doc.commit_with(
            CommitOptions::default()
                .with_time(10)
                .with_message(context.change_message().unwrap()),
        );
        let channel = doc.put_object(ROOT, "channel", ObjType::Text).unwrap();
        doc.splice_text(&channel, 0, 0, "Morning").unwrap();
        let other = tilo_actions::ActionContext::for_resource(CHAT_ROOT, CHAT_KIND, "Lounge")
            .with_group(RUN, "Claude Code", ui::history_source::steps::AGENT_SESSION);
        doc.commit_with(
            CommitOptions::default()
                .with_time(20)
                .with_message(other.change_message().unwrap()),
        );

        let mut tree = tree;
        pane.refresh(&mut tree);
        pane.refresh_detached_records(
            &mut tree,
            vec![DetachedRecordHistory {
                app: NOTES.to_owned(),
                checkout: ui::DocumentCheckout::package_primary(ui::ResourceType::new(NOTES_KIND)),
                tree_chunks: Vec::new(),
                document: Some(doc),
            }],
        );
        let served = pane.serve_runs(&wire::SessionsQuery::default(), &tree);
        let run = served
            .sessions
            .iter()
            .find(|run| run.id == RUN)
            .unwrap_or_else(|| panic!("the agent's run is listed: {served:#?}"));
        assert_eq!(
            run.undo_blocked, None,
            "the run is undone from the Activity: {run:#?}"
        );
        let [action] = run.actions.as_slice() else {
            panic!("one action, in the note: {run:#?}");
        };
        assert_eq!(
            (action.app_id.as_str(), action.record.as_str()),
            (NOTES, primary.as_str()),
            "the notes' record, and not the channel the same document holds"
        );

        // The person presses Undo run: the host is asked for the run in that record.
        let arguments = serde_json::to_value(&run.range)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap();
        let outcome = pane
            .serve_action(wire::UNDO_SESSION, &arguments, &mut tree)
            .expect("the run is in the history");
        assert!(outcome.handled);
        let undo = host.undos.borrow().first().cloned().expect("an undo");
        assert_eq!(undo.task_id, RUN);
        assert_eq!(undo.steps, vec![(NOTES.to_owned(), primary)]);
    }
}

/// The app whose records the run changed.
const NOTES_APP: &str = "so.tilo.notes";
/// The MCP session whose work the run is.
const GROUP: &str = "mcp:8c1d-4f2a";

/// A workspace with the records an agent connection's run changed, as their apps hold them.
struct Connection {
    tree: WorkspaceTree,
    pane: WorkspaceActivityPane,
    host: Rc<Host>,
    /// Each record: its app, its checkout, and its document as its app last wrote it.
    open: Vec<(String, ui::DocumentCheckout, AutoCommit)>,
    /// The records whose document this device does not hold right now.
    closed: Vec<String>,
}

impl Connection {
    /// The notes `files` open, each already written by the agent connection: one change a record,
    /// in the order the files are named.
    fn new(files: &[&str]) -> Self {
        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active_workspace().expect("workspace").id();
        let host = Rc::new(Host::default());
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(Rc::clone(&host) as Rc<dyn ActivityHost>);
        pane.view = ActivityView::Explore;
        let mut open = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let record = ui::DocumentId::for_file(file);
            let checkout = ui::DocumentCheckout::new(record.clone(), ui::ResourceType::new("note"));
            let mut doc = AutoCommit::new();
            ui::resource::init(&mut doc, &record, &checkout.kind, &workspace.to_string());
            doc.commit();
            doc.set_actor(ActorId::from(b"claude-code-agent".as_slice()));
            doc.put(ROOT, "title", *file).unwrap();
            let context = tilo_actions::ActionContext::for_resource(record.as_str(), "note", *file)
                .with_app(NOTES_APP, "Notes")
                .with_described(crate::desktop::activity::testing::verb("Write the note"))
                .with_group(
                    GROUP,
                    "Write the notes",
                    ui::history_source::steps::AGENT_SESSION,
                );
            let at = 10_i64.saturating_add(i64::try_from(index).unwrap_or(0));
            doc.commit_with(
                CommitOptions::default()
                    .with_time(at)
                    .with_message(context.change_message().unwrap()),
            );
            open.push((NOTES_APP.to_owned(), checkout, doc));
        }
        Self {
            tree,
            pane,
            host,
            open,
            closed: Vec::new(),
        }
    }

    /// The record id `file` names, as the history lists its changes.
    fn record(file: &str) -> String {
        ui::DocumentId::for_file(file).to_string()
    }

    /// The session's record, with the command line the agent asked it to run as one of its changes.
    fn in_a_shell(&mut self, command: &str, at: i64) {
        let workspace = self
            .tree
            .desktop
            .active_workspace()
            .expect("workspace")
            .id();
        let record = ui::DocumentId::for_tab(3);
        let checkout =
            ui::DocumentCheckout::new(record.clone(), ui::ResourceType::new("terminal.session"));
        let mut doc = AutoCommit::new();
        ui::resource::init(&mut doc, &record, &checkout.kind, &workspace.to_string());
        doc.commit();
        doc.set_actor(ActorId::from(b"claude-code-agent".as_slice()));
        ask(
            &mut doc,
            &record.to_string(),
            "req-1",
            "a0",
            command,
            GROUP,
            at,
        );
        self.open.push((TERMINAL_APP.to_owned(), checkout, doc));
    }

    /// The records as this device admits them now: `closed` ones without their document.
    fn refresh(&mut self) {
        self.pane.refresh(&mut self.tree);
        let records = self
            .open
            .iter()
            .map(|(app, checkout, doc)| DetachedRecordHistory {
                app: app.clone(),
                checkout: checkout.clone(),
                tree_chunks: Vec::new(),
                document: (!self
                    .closed
                    .iter()
                    .any(|closed| closed == checkout.id.as_str()))
                .then(|| doc.clone()),
            })
            .collect();
        self.pane.refresh_detached_records(&mut self.tree, records);
    }

    /// The connection's run, as the activity serves it now.
    fn run(&mut self) -> wire::AgentSession {
        self.refresh();
        let served = self
            .pane
            .serve_runs(&wire::SessionsQuery::default(), &self.tree);
        let total = served.total;
        served
            .sessions
            .into_iter()
            .find(|run| run.id == GROUP)
            .unwrap_or_else(|| panic!("the connection's work is one of {total} runs"))
    }

    /// What the host was last asked to undo.
    fn undo(&self) -> TaskRunUndo {
        self.host
            .undos
            .borrow()
            .last()
            .cloned()
            .expect("the host was asked to undo the run")
    }
}
