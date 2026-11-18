//! What the refresh before every action costs, in reads: the document the
//! request acts on and what a write changed since the last refresh, never
//! every document the workspace holds.
//!
//! The work is counted, not timed: a shared machine's times vary with the
//! load, while the reads a refresh makes are the same wherever it executes. The
//! count is process-wide ([`replica_reads`]), so this test measures a delta
//! around one action; a test binary that carries out several tests at once would
//! see theirs too.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_agent_session::workspace_actions::replica_reads;
use tilo_api::Server;

/// The workspace sizes a refresh is counted at: one small enough to show a
/// per-document cost as a handful of reads, one where a per-document cost is
/// hundreds.
const SIZES: [usize; 2] = [10, 1000];

/// The most members one `group` call carries, as the SDK sends them.
const GROUP_MEMBERS: usize = 500;

/// The most reads one action may take at either size: the document acted on,
/// what changed since the last refresh, and the documents the worker holds
/// mounted, which is bounded by its own cache and never by the workspace.
const CEILING: u64 = 16;

/// A key's calls of the API.
struct Agent<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl Agent<'_> {
    async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &[u8],
        kind: &str,
    ) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let pairs = [("authorization", bearer.as_str()), ("content-type", kind)];
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let reply = Box::pin(
            self.api
                .dispatch(&method, path, query, &headers(&pairs), body),
        )
        .await;
        json_of(reply).await
    }

    async fn get(&self, path: &str) -> Value {
        let (status, value) = self
            .send(hyper::Method::GET, path, b"", "application/json")
            .await;
        assert_eq!(status, 200, "GET {path}: {value}");
        value
    }

    /// `action` of `resource` with `args`, which ran: its result.
    async fn act(&self, base: &str, resource: &str, action: &str, args: &Value) -> Value {
        let (status, reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args.to_string().as_bytes(),
                "application/json",
            )
            .await;
        assert_eq!(status, 200, "{resource}#{action} {args}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action} {args}: {reply}");
        reply["result"].clone()
    }

    /// [`Self::act`] for each of `items` in as few `group` calls as the API
    /// takes: what the SDK's bulk calls send. A resource that takes no group
    /// is given its calls one at a time.
    async fn many(&self, base: &str, resource: &str, action: &str, items: &[Value]) {
        if let Err(refused) = self.grouped(base, resource, action, items).await {
            eprintln!("{resource}#{action}: no bulk call ({refused}); one at a time");
            for args in items {
                self.act(base, resource, action, args).await;
            }
        }
    }

    async fn grouped(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        items: &[Value],
    ) -> Result<(), String> {
        for chunk in items.chunks(GROUP_MEMBERS) {
            let members: Vec<Value> = chunk
                .iter()
                .map(|args| json!({"action": action, "args": args}))
                .collect();
            let (status, reply) = self
                .send(
                    hyper::Method::POST,
                    &format!("{base}/resources/{resource}/actions/group"),
                    json!({ "actions": members }).to_string().as_bytes(),
                    "application/json",
                )
                .await;
            if status != 200 || reply["status"] != "ok" {
                return Err(format!("{status} {reply}"));
            }
        }
        Ok(())
    }
}

/// A workspace of Ada's, through the API: its base path and id.
async fn workspace(agent: &Agent<'_>, name: &str) -> (String, String) {
    let (status, made) = agent
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            json!({ "name": name }).to_string().as_bytes(),
            "application/json",
        )
        .await;
    assert_eq!(status, 201, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    (format!("/v1/workspaces/{id}"), id)
}

/// The path of the resource of the notebook's pages: the row whose actions
/// make a page.
async fn notes(agent: &Agent<'_>, base: &str) -> String {
    let listed = agent
        .get(&format!("{base}/resources?path=notes&limit=200"))
        .await;
    let rows = listed["rows"].as_array().cloned().unwrap_or_default();
    rows.iter()
        .find(|row| {
            row["actions"]
                .as_array()
                .is_some_and(|actions| actions.iter().any(|named| named == "create-page"))
        })
        .and_then(|row| row["path"].as_str())
        .map_or_else(
            || panic!("the notebook lists the pages it makes: {listed}"),
            str::to_owned,
        )
}

/// A note's Markdown, one paragraph.
fn note(index: usize) -> Value {
    json!({
        "title": format!("note-{index}"),
        "markdown": format!("# Note {index}\n\nA paragraph of what note {index} keeps.\n"),
    })
}

/// The reads one action takes, in that agent's workspace of `size` notes: the
/// least of a few, so what a settling workspace sends beside the action does
/// not decide it.
async fn reads_per_action(agent: &Agent<'_>, base: &str, size: usize) -> u64 {
    agent.act(base, "notes", "open", &json!({})).await;
    let notes = notes(agent, base).await;
    let seed: Vec<Value> = (0..size).map(note).collect();
    agent.many(base, &notes, "create-page", &seed).await;
    // The first action catches up what seeding changed; only the ones after
    // it show what a refresh costs once the workspace is where it is.
    agent.act(base, &notes, "create-page", &note(size)).await;
    let mut took = Vec::new();
    for index in 0..3 {
        let before = replica_reads();
        agent
            .act(base, &notes, "create-page", &note(size + 1 + index))
            .await;
        took.push(replica_reads().saturating_sub(before));
    }
    took.into_iter().min().unwrap_or_default()
}

/// A refresh reads the document the action names and what a write changed,
/// so an action at a thousand notes reads what one at ten does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_reads_what_the_action_names_however_full_the_workspace() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Agent {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Refresh").await,
    };
    let mut counted = Vec::new();
    for size in SIZES {
        let (base, _) = workspace(&agent, &format!("Refresh {size}")).await;
        let reads = reads_per_action(&agent, &base, size).await;
        eprintln!("{size} notes: {reads} reads for an action");
        counted.push((size, reads));
    }
    for (size, reads) in &counted {
        assert!(
            *reads <= CEILING,
            "{size} notes: an action read {reads} documents, more than the {CEILING} the \
             document it names and what changed take"
        );
    }
    let (small, large) = (counted[0].1, counted[1].1);
    assert!(
        large <= small.saturating_add(2),
        "a full workspace reads per action what a small one does: {small} reads at {} notes, \
         {large} at {}",
        counted[0].0,
        counted[1].0
    );
}
