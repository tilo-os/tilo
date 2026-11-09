//! An agent connection's run read as the timeline of what it did: the command lines it asked a
//! shell session to run, each with the run that session recorded, and the command still running
//! as what it is doing now.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};
use tilo_actions::remote::workspace_activity as wire;

use super::TERMINAL_APP;
use crate::activity::ActivityHost;
use crate::activity::index::DetachedRecordHistory;
use crate::activity::pane::{ActivityView, WorkspaceActivityPane};
use crate::activity::testing::{TestHost, TestTree as WorkspaceTree};

const OUTPUT_HASH: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
const OUTPUT: &[u8] = b"\x1b[1m   Compiling\x1b[0m signup v0.1.0\r\nrunning 2 tests\r\ntest signs_up ... ok\r\ntest rejects_taken_names ... FAILED\r\n\r\ntest result: FAILED. 1 passed; 1 failed\r\n";

/// The test host, with the content a session's runs keep their output in.
#[derive(Default)]
struct Host {
    inner: TestHost,
    content: RefCell<HashMap<(String, String, String), Vec<u8>>>,
}

impl ActivityHost for Host {
    fn local_user(&self) -> String {
        self.inner.local_user()
    }
    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        self.inner.local_author(authors, actor)
    }
    fn record_content(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.content
            .borrow()
            .get(&(workspace.to_owned(), app.to_owned(), hash.to_owned()))
            .cloned()
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
fn ask(doc: &mut AutoCommit, record: &str, id: &str, order: &str, command: &str, at: i64) {
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
        .with_group("mcp:session-2", "Claude Code #2", "agent-run");
    doc.commit_with(
        CommitOptions::default()
            .with_time(at)
            .with_message(context.change_message().unwrap()),
    );
}

/// The terminal's own record of the run the request became, and its answer to the request.
fn record_run(doc: &mut AutoCommit, id: &str, exit: i64, at: i64) {
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
        serde_json::json!({"state": "done", "result": {"run": id, "exit": exit}}).to_string(),
    )
    .unwrap();
    doc.commit_with(CommitOptions::default().with_time(at.saturating_add(3)));
}

struct Fixture {
    tree: WorkspaceTree,
    pane: WorkspaceActivityPane,
    doc: AutoCommit,
    checkout: ui::RecordCheckout,
}

impl Fixture {
    fn new() -> Self {
        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active_workspace().expect("workspace").id();
        let record = ui::RecordId::for_tab(7);
        let checkout =
            ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("terminal.session"));
        let mut doc = AutoCommit::new();
        ui::record::init(&mut doc, &record, &checkout.kind, &workspace.to_string());
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

    fn runs(&mut self) -> wire::Runs {
        self.pane.refresh(&mut self.tree);
        self.pane.refresh_detached_records(
            &mut self.tree,
            vec![DetachedRecordHistory {
                app: TERMINAL_APP.to_owned(),
                checkout: self.checkout.clone(),
                document: Some(self.doc.clone()),
            }],
        );
        self.pane
            .serve_runs(&wire::RunsQuery::default(), &self.tree)
    }
}

/// An MCP session's run that asked a shell to run a command shows that command as the shell
/// recorded it: its directory, how it ended, and the last lines it printed, with the whole output
/// one read away. A command still running is what the run is doing now.
#[test]
fn an_agent_runs_commands_show_their_run_records_and_the_one_running_now() {
    let mut fixture = Fixture::new();
    let record = fixture.record();
    fixture.as_agent();
    ask(
        &mut fixture.doc,
        &record,
        "req-1",
        "a0",
        "cargo test -p signup",
        10,
    );
    fixture.as_terminal();
    record_run(&mut fixture.doc, "req-1", 101, 11);
    fixture.as_agent();
    ask(
        &mut fixture.doc,
        &record,
        "req-2",
        "a1",
        "cargo test -p signup -- rejects_taken_names",
        20,
    );

    let served = fixture.runs();
    let run = served
        .runs
        .iter()
        .find(|run| run.id == "mcp:session-2")
        .unwrap_or_else(|| panic!("the session's work is one run: {served:#?}"));
    assert_eq!(run.agent.name, "Claude Code #2");
    assert_eq!(run.agent.relation, wire::Relation::Software);
    assert_eq!(run.actions.len(), 2, "{:#?}", run.actions);

    let wire::ActionView::Command {
        command,
        cwd,
        output,
        exit,
        started_ms,
        ended_ms,
        run: recorded,
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
            recorded.run.as_str()
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
    record_run(&mut fixture.doc, "req-2", 0, 30);
    let served = fixture.runs();
    let run = served
        .runs
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
