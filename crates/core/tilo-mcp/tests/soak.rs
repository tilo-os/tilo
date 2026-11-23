//! An agent's MCP server holds what it holds, however long the agent works.
//!
//! `tilo mcp` serves every agent on a computer from one process that runs for days: each Claude
//! Code window, and every subagent it starts, is a connection of its own that calls the tools
//! thousands of times and then goes. Whatever a call or a connection leaves behind therefore adds
//! up until the computer runs out of memory. This drives one server through thousands of calls
//! of every tool, and many connections that come and go, and reads what the process still holds
//! allocated: after a warm-up it does not grow.
//!
//! The count is the whole process's ([`tilo_test_env::held`]), so this binary runs the one test.
//! `TILO_MCP_SOAK_CALLS` raises the number of calls for a longer soak by hand.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_mcp::McpServer;

#[global_allocator]
static ALLOCATOR: tilo_test_env::held::Held = tilo_test_env::held::Held;

mod notes {
    use tilo_resource::{Model, resource};

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub title: String,
    }

    #[resource(
        kind = "test.note",
        catalogue = "tests/locales",
        read_content = tilo_resource::read::fields
    )]
    impl Note {
        /// Rename the note.
        #[action]
        pub fn rename(&mut self, #[param("New title")] title: String) {
            self.title = title;
        }
    }
}

/// The invocations the host still answers for: its own memory is bounded, so what grows is the
/// server's.
const HOST_KEEPS: usize = 64;

/// An app's directory of a few notes, whose renames succeed at once.
struct Notes {
    rows: Vec<CatalogueRow>,
    invoked: Mutex<VecDeque<InvocationId>>,
}

impl Notes {
    fn new() -> Self {
        Self {
            rows: (0..NOTES).map(row).collect(),
            invoked: Mutex::new(VecDeque::new()),
        }
    }
}

const NOTES: usize = 4;

fn row(index: usize) -> CatalogueRow {
    let id = format!("test.note:{index}");
    let label = format!("Note {index}");
    let resource = tilo_resource::publish::<notes::Note>(&id, &label, &notes::Note::default());
    let mut descriptor = tilo_actions::api::survey(&[resource])
        .into_iter()
        .find(|descriptor| descriptor.address.action == tilo_resource::APPLY)
        .unwrap();
    // Offered as the one action it is, as an app's directory names its actions.
    descriptor.address.action = "rename".into();
    descriptor.params = vec![tilo_actions::ActionParam::required(
        "title",
        tilo_actions::ParamType::Text,
        "New title",
    )];
    descriptor.input = tilo_actions::api::InputSpec::None;
    CatalogueRow {
        offered: None,
        provider: "71".into(),
        app_id: "so.tilo.notes".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id,
            kind: "test.note".into(),
            label,
            presentation: None,
        },
        action: Some(descriptor),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

impl ActionProvider for Notes {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let mut page = CataloguePage::from_rows("1".into(), self.rows.clone(), &query);
        page.providers = vec![CatalogueProvider {
            provider: "71".into(),
            app_id: "so.tilo.notes".into(),
            workspace: "workspace".into(),
            document_id: "notebook".into(),
            browsable: true,
        }];
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        let mut invoked = self.invoked.lock().unwrap();
        invoked.push_back(invocation);
        while invoked.len() > HOST_KEEPS {
            invoked.pop_front();
        }
        drop(invoked);
        Pending::ready(Ok(InvocationState::new(
            invocation,
            tilo_actions::InvocationUpdate::Accepted { invocation },
        )
        .receipt()))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let known = self.invoked.lock().unwrap().contains(id);
        known.then(|| {
            InvocationState::new(
                *id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: tilo_actions::ActionOutput::default(),
                },
            )
        })
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// One agent connection: the server a `tilo mcp` connection opens, as its own agent run.
fn connection(host: &std::sync::Arc<Notes>, run: usize) -> McpServer<std::sync::Arc<Notes>> {
    McpServer::for_agent(
        std::sync::Arc::clone(host),
        "workspace".into(),
        "claude-code",
        "ada",
    )
    .with_agent_session(format!("run-{run}"))
}

async fn ask(
    server: &McpServer<std::sync::Arc<Notes>>,
    id: usize,
    method: &str,
    params: Value,
) -> Value {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .await
        .expect("a request is answered");
    assert!(answer.get("error").is_none(), "{method}: {answer}");
    if std::env::var_os("TILO_MCP_SOAK_SAY").is_some() {
        eprintln!("{method} {params}: {answer}");
    }
    answer
}

/// What an agent does in one turn: open, list, read, find, act, watch and look back.
async fn turn(server: &McpServer<std::sync::Arc<Notes>>, step: usize) {
    let note = step % NOTES;
    let call = |name: &str, arguments: Value| json!({"name": name, "arguments": arguments});
    ask(server, step, "tools/call", call("tilo_ls", json!({}))).await;
    ask(
        server,
        step,
        "tools/call",
        call("tilo_ls", json!({"path": "notes"})),
    )
    .await;
    ask(
        server,
        step,
        "tools/call",
        call("tilo_read", json!({"path": "notes"})),
    )
    .await;
    ask(
        server,
        step,
        "tools/call",
        call("tilo_find", json!({"query": "note"})),
    )
    .await;
    ask(
        server,
        step,
        "tools/call",
        call(
            "tilo_do",
            json!({"path": format!("notes/Note {note}#rename"), "args": {"title": format!("Title {step}")}}),
        ),
    )
    .await;
    ask(
        server,
        step,
        "tools/call",
        call("tilo_watch", json!({"wait_ms": 0})),
    )
    .await;
    if step.is_multiple_of(50) {
        // Every invocation of the session, which it lists whole: now and then, as an agent asks.
        ask(server, step, "tools/call", call("tilo_status", json!({}))).await;
    }
}

/// `turns` turns over `connections` connections that each open, work and go.
async fn work(host: &std::sync::Arc<Notes>, connections: usize, turns: usize) {
    for run in 0..connections {
        let server = connection(host, run);
        ask(
            &server,
            0,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "2"}}),
        )
        .await;
        ask(&server, 1, "tools/list", json!({})).await;
        for step in 0..turns {
            turn(&server, step).await;
        }
    }
}

/// Settle the runtime and read what the process holds.
async fn held() -> isize {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    tilo_test_env::held::bytes()
}

#[test]
fn thousands_of_calls_and_connections_leave_nothing_behind() {
    let calls: usize = std::env::var("TILO_MCP_SOAK_CALLS")
        .ok()
        .and_then(|calls| calls.parse().ok())
        .unwrap_or(1_500);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    runtime.block_on(async {
        let host = std::sync::Arc::new(Notes::new());
        // One long connection past everything a session keeps of its latest calls, and enough
        // short ones to fill every cache the server shares, before anything is counted.
        let long = connection(&host, usize::MAX);
        for step in 0..calls {
            turn(&long, step).await;
        }
        work(&host, 20, 5).await;
        let warm = held().await;

        // As much again: the long connection works on, and agents come and go beside it.
        for step in calls..calls.saturating_mul(2) {
            turn(&long, step).await;
        }
        work(&host, 200, 5).await;
        // And a night of idle: nothing it waits for keeps anything.
        tokio::time::sleep(std::time::Duration::from_secs(8 * 3600)).await;
        let after = held().await;

        let grew = after.saturating_sub(warm);
        eprintln!("held {warm} bytes once warm, {after} after as much again ({grew} more)");
        // Six tool calls a turn: thousands of calls, hundreds of connections.
        let made = calls.saturating_mul(6).saturating_add(200 * 5 * 6);
        assert!(
            grew < 256 * 1024,
            "the server held {warm} bytes once warm and {after} after {made} more calls and 200 \
             connections: {grew} bytes more"
        );
    });
}
