//! How fast a workspace takes what people and agents make in it, and whether
//! that speed holds as the workspace fills: the workspace server's own action and
//! upload paths, driven through the API as an agent or `tilo` drives them.
//!
//! For each workspace size (`TILO_THROUGHPUT_SIZES`, items already there per
//! kind) it times, for Tasks issues, Notes pages, chat messages and Files:
//! - one action at a time: its p50 and p99;
//! - a bulk call (`group`, as the SDK's `*_many` sends it): records a second;
//! - a second device of the same person: how long after the action answers
//!   the change reaches it.
//!
//! and once: file bytes in (`uploads`) for one large file, and many small
//! files put one after another.
//!
//! The targets: a single action is local work plus at most one round trip,
//! so its time does not grow with what the workspace holds; a bulk call
//! moves at least a thousand records a second; a file goes in at disk or
//! network speed. With `TILO_THROUGHPUT_ASSERT=1` the test fails when a
//! target does not hold; either way it prints the table, and writes it as
//! JSON to `TILO_THROUGHPUT_OUT` when set.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;
use tilo_api::Server;

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

    /// `action` once for each of `items`, as the SDK's bulk call sends it:
    /// `group` calls of at most [`GROUP_MEMBERS`] members. A resource that
    /// takes no group is answered by why.
    async fn many(
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
                return Err(format!("{status} {}", reply["error"]["message"]));
            }
        }
        Ok(())
    }

    /// [`Self::many`], or one call after another where the resource takes
    /// no group: whether it went as one.
    async fn many_or_each(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        items: &[Value],
    ) -> Option<String> {
        let refused = self.many(base, resource, action, items).await.err()?;
        for args in items {
            self.act(base, resource, action, args).await;
        }
        Some(refused)
    }

    /// `bytes` into the workspace in one request: their `tilo.content:` name.
    async fn upload(&self, base: &str, bytes: &[u8]) -> String {
        let (status, reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/uploads"),
                bytes,
                "application/octet-stream",
            )
            .await;
        assert!(status == 200 || status == 201, "upload: {status} {reply}");
        let text = reply.to_string();
        let at = text
            .find("tilo.content:")
            .unwrap_or_else(|| panic!("an upload names its content: {text}"));
        text[at..].split('"').next().unwrap_or_default().to_owned()
    }
}

/// The most members one `group` call carries, as the SDK sends them.
const GROUP_MEMBERS: usize = 500;

/// One kind of record the benchmark makes: where it lives, the action that
/// makes one and that action's arguments for the `index`th.
struct Kind {
    name: &'static str,
    /// The app whose `open` founds its records.
    app: &'static str,
    /// The app id the second device hears its changes under.
    app_id: &'static str,
    /// The kind of the resource whose action makes one.
    listed_as: &'static str,
    action: &'static str,
    args: fn(usize) -> Value,
}

fn issue(index: usize) -> Value {
    json!({"title": format!("Issue {index}"), "body": format!("What issue {index} is about, in a line or two.")})
}

fn note(index: usize) -> Value {
    json!({"title": format!("note-{index}"), "markdown": format!("# Note {index}\n\nA paragraph of what note {index} keeps.\n")})
}

fn message(index: usize) -> Value {
    json!({"channel": CHANNEL.lock().unwrap().clone(), "text": format!("message {index}: a line of chat")})
}

/// The channel chat messages go to, in the workspace being measured.
static CHANNEL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A channel of the workspace's chat for the messages, by its id.
async fn channel(agent: &Agent<'_>, base: &str) -> String {
    let chat = resource_of(agent, base, "chat", "chat.workspace", "create-channel").await;
    let read = agent.get(&format!("{base}/resources/{chat}")).await;
    let id = find_channel(&read).unwrap_or_else(|| {
        panic!("the chat has a channel: {read}");
    });
    CHANNEL.lock().unwrap().clone_from(&id);
    id
}

/// The id of a channel `value` names: a `c_` id.
fn find_channel(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            if let Some(id) = object.get("id").and_then(Value::as_str)
                && id.starts_with("c_")
            {
                return Some(id.to_owned());
            }
            object.values().find_map(find_channel)
        }
        Value::Array(items) => items.iter().find_map(find_channel),
        _ => None,
    }
}

const KINDS: &[Kind] = &[
    Kind {
        name: "tasks issue",
        app: "tasks",
        app_id: "so.tilo.tasks",
        listed_as: "tasks.issues",
        action: "create",
        args: issue,
    },
    Kind {
        name: "notes page",
        app: "notes",
        app_id: "so.tilo.notes",
        listed_as: "notes",
        action: "create-page",
        args: note,
    },
    Kind {
        name: "chat message",
        app: "chat",
        app_id: "so.tilo.chat",
        listed_as: "chat.messages",
        action: "post",
        args: message,
    },
];

/// The path of the resource of `kind` among the app's records, as a
/// listing names it.
async fn resource_of(agent: &Agent<'_>, base: &str, app: &str, kind: &str, action: &str) -> String {
    let listed = agent
        .get(&format!("{base}/resources?path={app}&limit=200"))
        .await;
    let rows = listed["rows"].as_array().cloned().unwrap_or_default();
    let found = rows.iter().find(|row| {
        row["kind"].as_str() == Some(kind)
            || row["actions"]
                .as_array()
                .is_some_and(|actions| actions.iter().any(|named| named == action))
    });
    if let Some(row) = found {
        eprintln!("{app}: {kind} at {} offers {}", row["path"], row["actions"]);
    }
    found.and_then(|row| row["path"].as_str()).map_or_else(
        || panic!("no {kind} with {action} under {app}: {listed}"),
        str::to_owned,
    )
}

fn quantile(durations: &[Duration], quantile: f64) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

fn sizes() -> Vec<usize> {
    std::env::var("TILO_THROUGHPUT_SIZES").ok().map_or_else(
        || vec![0, 1000],
        |sizes| {
            sizes
                .split(',')
                .filter_map(|size| size.trim().parse().ok())
                .collect()
        },
    )
}

fn count(env: &str, default: usize) -> usize {
    std::env::var(env)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// One measured row of the table.
#[derive(serde::Serialize)]
struct Row {
    kind: String,
    size: usize,
    single_p50_ms: f64,
    single_p99_ms: f64,
    seed_per_s: f64,
    bulk_per_s: f64,
    second_device_p50_ms: Option<f64>,
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
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

/// `device` joining the workspace `id` beside the key's calls, as a
/// person's other device does.
async fn second_device(
    on_server: &super::api_on_server::OnServer,
    device: &crate::support::Device,
    id: &str,
) -> crate::harness::page::Page {
    use crate::support::{admitted_to, ready, start_peer};
    let workspace = tilo_server_protocol::WorkspaceId::parse(id).unwrap();
    let peer = start_peer(
        tilo_server_client::Host::at(on_server.hosted.server.node_addr()),
        device.cert.clone(),
        on_server.host.cert.clone(),
        device.key.clone(),
    );
    let events = peer.events();
    ready(&[&peer]).await;
    let answer = peer
        .ask_workspaces(tilo_server_protocol::workspaces::WorkspacesRequest::Enter {
            workspace: id.into(),
        })
        .unwrap();
    let answer = tilo_test_env::within("the device enters the workspace", answer)
        .await
        .unwrap();
    eprintln!("the second device entering: {answer:?}");
    peer.send(tilo_session_client::Cmd::Join(workspace))
        .unwrap();
    admitted_to(&peer, workspace).await;
    // The workspace's file tree, as a computer that mounts the workspace's
    // folder opens it: its own document and the first chunks it lists.
    for chunk in 0..4 {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: fs_protocol::tree::FILES_APP.to_owned(),
            cluster_id: id.to_owned(),
            document: if chunk == 0 {
                tilo_fs::tree_document::document_id()
            } else {
                fs_protocol::tree::chunk_document_id(chunk)
            },
        };
        peer.send(tilo_session_client::Cmd::OpenDocumentAt(
            workspace,
            route.clone(),
        ))
        .unwrap();
        peer.send(tilo_session_client::Cmd::OpenRoute(route))
            .unwrap();
    }
    crate::harness::page::Page::joined((peer, events), device, &on_server.host, id)
}

/// How long after now a write of `app_id` by someone else reaches `page`.
async fn heard_by(
    page: &crate::harness::page::Page,
    app_id: &str,
    since: Instant,
) -> Option<Duration> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let own = page.peer.engine().identity().device_cert.device_public_key;
    while Instant::now() < deadline {
        for event in page.events.drain() {
            if let tilo_session_client::Event::Landed { route, author } = event
                && route.app_id == app_id
                && author != Some(own)
            {
                return Some(since.elapsed());
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "fills workspaces to thousands of records and times every path; run with \
            --ignored in a release build (`TILO_THROUGHPUT_SIZES=0,1000,25000`)"]
async fn a_workspace_takes_records_and_files_at_the_same_speed_however_full() {
    // Where the time goes, as the host logs it, when `RUST_LOG` asks.
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let singles = count("TILO_THROUGHPUT_SINGLES", 40);
    let bulk = count("TILO_THROUGHPUT_BULK", 1000);
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Agent {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Bench").await,
    };
    let mut table: Vec<Row> = Vec::new();

    for size in sizes() {
        let (base, id) = workspace(&agent, &format!("Bench {size}")).await;
        for kind in KINDS {
            agent.act(&base, kind.app, "open", &json!({})).await;
        }
        channel(&agent, &base).await;
        let page = second_device(&on_server, &browser, &id).await;

        // The calls an agent makes over MCP, as a key reaches them: listing a
        // notebook, reading it, and making a note. The target is the same as a
        // person's: local work and at most one round trip, so under 300 ms.
        let mut mcp = Mcp {
            api: &api,
            path: format!("/w/{id}/mcp"),
            token: agent.token.clone(),
            session: None,
        };
        assert_eq!(mcp.initialize().await.0, 200);
        let calls = 3;
        let mut took = Vec::with_capacity(calls);
        for _ in 0..calls {
            let began = Instant::now();
            let wrote = mcp
                .tilo_do(json!({"path": "notes/Notes#new-note",
                                 "args": {"title": format!("mcp-{size}")}}))
                .await;
            took.push(began.elapsed());
            assert_eq!(wrote["result"]["isError"], false, "{wrote}");
        }
        let wrote = Row {
            kind: "mcp write".into(),
            size,
            single_p50_ms: ms(quantile(&took, 0.5)),
            single_p99_ms: ms(quantile(&took, 0.99)),
            seed_per_s: 0.0,
            bulk_per_s: 0.0,
            second_device_p50_ms: None,
        };
        for path in ["notes/Notes", "notes"] {
            let mut took = Vec::with_capacity(calls);
            for _ in 0..calls {
                let began = Instant::now();
                let (status, listed) = mcp
                    .post(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                 "params": {"name": "tilo_ls", "arguments": {"path": path},
                                            "_meta": {"so.tilo.mcp": {"structured": true}}}}))
                    .await;
                took.push(began.elapsed());
                assert_eq!(status, 200, "{listed}");
            }
            table.push(Row {
                kind: format!("mcp ls {path}"),
                size,
                single_p50_ms: ms(quantile(&took, 0.5)),
                single_p99_ms: ms(quantile(&took, 0.99)),
                seed_per_s: 0.0,
                bulk_per_s: 0.0,
                second_device_p50_ms: None,
            });
        }
        let mut took = Vec::with_capacity(calls);
        for _ in 0..calls {
            let began = Instant::now();
            let read = mcp.tilo_read(json!({"path": "notes/Notes"})).await;
            took.push(began.elapsed());
            assert_eq!(read["result"]["isError"], false, "{read}");
        }
        table.push(Row {
            kind: "mcp read".into(),
            size,
            single_p50_ms: ms(quantile(&took, 0.5)),
            single_p99_ms: ms(quantile(&took, 0.99)),
            seed_per_s: 0.0,
            bulk_per_s: 0.0,
            second_device_p50_ms: None,
        });
        eprintln!(
            "{:>14} at {size:>6}: single p50 {:7.1} ms p99 {:7.1} ms",
            "mcp write", wrote.single_p50_ms, wrote.single_p99_ms
        );
        table.push(wrote);

        for kind in KINDS {
            let resource = resource_of(&agent, &base, kind.app, kind.listed_as, kind.action).await;
            // The workspace filled to `size` of them, in bulk.
            let seed: Vec<Value> = (0..size).map(kind.args).collect();
            let began = Instant::now();
            if let Some(refused) = agent
                .many_or_each(&base, &resource, kind.action, &seed)
                .await
            {
                eprintln!(
                    "{}: no bulk call ({refused}); seeded one at a time",
                    kind.name
                );
            }
            let seeded = began.elapsed();
            let seed_per_s = if size == 0 {
                0.0
            } else {
                size as f64 / seeded.as_secs_f64()
            };

            // One at a time.
            let mut took = Vec::with_capacity(singles);
            let mut heard = Vec::with_capacity(singles);
            for index in 0..singles {
                let args = (kind.args)(size + index);
                drop(page.events.drain());
                let began = Instant::now();
                agent.act(&base, &resource, kind.action, &args).await;
                took.push(began.elapsed());
                if let Some(arrived) = heard_by(&page, kind.app_id, began).await {
                    heard.push(arrived);
                }
            }

            // In bulk.
            let more: Vec<Value> = (0..bulk)
                .map(|index| (kind.args)(size + singles + index))
                .collect();
            let began = Instant::now();
            let bulk_per_s = match agent.many(&base, &resource, kind.action, &more).await {
                Ok(()) => bulk as f64 / began.elapsed().as_secs_f64(),
                Err(refused) => {
                    eprintln!("{}: no bulk call: {refused}", kind.name);
                    0.0
                }
            };

            let row = Row {
                kind: kind.name.to_owned(),
                size,
                single_p50_ms: ms(quantile(&took, 0.5)),
                single_p99_ms: ms(quantile(&took, 0.99)),
                seed_per_s,
                bulk_per_s,
                second_device_p50_ms: (!heard.is_empty()).then(|| ms(quantile(&heard, 0.5))),
            };
            eprintln!(
                "{:>14} at {size:>6}: single p50 {:7.1} ms p99 {:7.1} ms | seed {:8.0}/s | bulk {:8.0}/s | second device p50 {:?} ms ({} of {singles} heard)",
                row.kind,
                row.single_p50_ms,
                row.single_p99_ms,
                row.seed_per_s,
                row.bulk_per_s,
                row.second_device_p50_ms,
                heard.len()
            );
            table.push(row);
        }

        // Files: one put after another (upload, then `files#open`), as
        // `tilo files put` sends them today.
        let files = count("TILO_THROUGHPUT_FILES", 200);
        let mut took = Vec::with_capacity(files);
        let mut heard = Vec::new();
        let mut waiting = Duration::ZERO;
        let began = Instant::now();
        for index in 0..files {
            let one = Instant::now();
            let body = format!("fn file_{index}() {{}}\n").repeat(40);
            let content = agent.upload(&base, body.as_bytes()).await;
            drop(page.events.drain());
            agent
                .act(
                    &base,
                    "files",
                    "open",
                    &json!({"content": content, "name": format!("f{index}.rs"), "dir": format!("src/m{}", index / 50)}),
                )
                .await;
            took.push(one.elapsed());
            if index % 20 == 0 {
                let listening = Instant::now();
                if let Some(arrived) = heard_by(&page, fs_protocol::tree::FILES_APP, one).await {
                    heard.push(arrived);
                }
                waiting += listening.elapsed();
            }
        }
        let files_per_s = files as f64 / began.elapsed().saturating_sub(waiting).as_secs_f64();
        eprintln!(
            "{:>14} at {size:>6}: one put p50 {:7.1} ms p99 {:7.1} ms | {files_per_s:8.1} files/s | second device p50 {:?}",
            "file put",
            ms(quantile(&took, 0.5)),
            ms(quantile(&took, 0.99)),
            (!heard.is_empty()).then(|| ms(quantile(&heard, 0.5)))
        );
        table.push(Row {
            kind: "file put".into(),
            size,
            single_p50_ms: ms(quantile(&took, 0.5)),
            single_p99_ms: ms(quantile(&took, 0.99)),
            seed_per_s: 0.0,
            bulk_per_s: files_per_s,
            second_device_p50_ms: (!heard.is_empty()).then(|| ms(quantile(&heard, 0.5))),
        });
    }

    // One large file's bytes in.
    let large = count("TILO_THROUGHPUT_LARGE_MB", 64);
    if large > 0 {
        let (base, _) = workspace(&agent, "Bench large").await;
        let piece = 64 * 1024 * 1024;
        let bytes: Vec<u8> = (0..large.min(64) * 1024 * 1024)
            .map(|index| (index as u32).wrapping_mul(2_654_435_761).to_le_bytes()[0])
            .collect();
        let began = Instant::now();
        let content = if large * 1024 * 1024 <= piece {
            agent.upload(&base, &bytes).await
        } else {
            // In pieces, as `tilo put` sends a file larger than one request.
            let size = large * 1024 * 1024;
            let (status, opened) = agent
                .send(
                    hyper::Method::POST,
                    &format!("{base}/uploads/bench-large-upload"),
                    json!({ "size": size }).to_string().as_bytes(),
                    "application/json",
                )
                .await;
            assert!(status < 300, "{opened}");
            let mut sent = Value::Null;
            for _ in 0..large.div_ceil(64) {
                let (status, answer) = agent
                    .send(
                        hyper::Method::PUT,
                        &format!("{base}/uploads/bench-large-upload"),
                        &bytes,
                        "application/octet-stream",
                    )
                    .await;
                assert!(status < 300, "{answer}");
                sent = answer;
            }
            let text = sent.to_string();
            let at = text
                .find("tilo.content:")
                .unwrap_or_else(|| panic!("the last piece names the content: {text}"));
            text[at..].split('"').next().unwrap_or_default().to_owned()
        };
        let uploaded = began.elapsed();
        // Into the workspace's folder, as `tilo files put` places it.
        let placing = Instant::now();
        agent
            .act(
                &base,
                "files",
                "open",
                &json!({"content": content, "name": "large.bin", "dir": "big"}),
            )
            .await;
        eprintln!(
            "upload of {large} MB: {uploaded:?}; placed in the folder in {:?}",
            placing.elapsed()
        );
        let took = began.elapsed();
        let rate = large as f64 / took.as_secs_f64();
        eprintln!("upload of {large} MB: {took:?}, {rate:.1} MB/s");
        table.push(Row {
            kind: format!("upload {large} MB (MB/s)"),
            size: 0,
            single_p50_ms: ms(took),
            single_p99_ms: ms(took),
            seed_per_s: 0.0,
            bulk_per_s: rate,
            second_device_p50_ms: None,
        });
    }

    if let Ok(out) = std::env::var("TILO_THROUGHPUT_OUT") {
        std::fs::write(&out, serde_json::to_vec_pretty(&table).unwrap()).unwrap();
    }

    if std::env::var("TILO_THROUGHPUT_ASSERT").is_ok_and(|value| value == "1") {
        let mut failed = String::new();
        let smallest = table.iter().map(|row| row.size).min().unwrap_or(0);
        for row in &table {
            if row.kind.starts_with("upload") {
                continue;
            }
            if row.kind.starts_with("mcp ") && row.single_p50_ms > 300.0 {
                let _ = writeln!(
                    failed,
                    "{} at {}: single p50 {:.1} ms over 300 ms",
                    row.kind, row.size, row.single_p50_ms
                );
            }
            if !row.kind.starts_with("mcp ") && row.kind != "file put" && row.bulk_per_s < 1000.0 {
                let _ = writeln!(
                    failed,
                    "{} at {}: bulk {:.0}/s < 1000/s",
                    row.kind, row.size, row.bulk_per_s
                );
            }
            if let Some(first) = table
                .iter()
                .find(|other| other.kind == row.kind && other.size == smallest)
                && row.single_p50_ms > first.single_p50_ms * 2.0 + 20.0
            {
                let _ = writeln!(
                    failed,
                    "{} at {}: single p50 {:.1} ms grew from {:.1} ms",
                    row.kind, row.size, row.single_p50_ms, first.single_p50_ms
                );
            }
        }
        assert!(failed.is_empty(), "targets missed:\n{failed}");
    }
}
