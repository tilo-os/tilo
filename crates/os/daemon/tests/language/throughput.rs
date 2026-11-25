//! How fast a workspace takes what people and agents make in it, and whether
//! that speed keeps as the workspace fills: the device's one dispatch, its
//! store and the workspace's own folder, driven through the API's listings and
//! its tool calls as an agent or `tilo` drives them.
//!
//! For each workspace size (`TILO_THROUGHPUT_SIZES`, items already there per
//! kind) it times, for Tasks issues and chat messages:
//! - one action at a time: its p50 and p99;
//! - the app's bulk call (`new_tasks`, as the SDK's `*_many` sends it): records
//!   a second, where the app takes one;
//! - where the app takes a list in one call (Tasks' `new_tasks`): items a
//!   second sent that way;
//! - another device of the same person: how long after the action answers the
//!   change it made is read there.
//!
//! and once: file bytes in for one large file, and many small files put one
//! after another.
//!
//! The sizes fill one workspace, the one a daemon founds for its person, one
//! after another: a size seeds what the sizes before it left it short of, and
//! the calls those sizes made stay, as they stay in a workspace people use.
//!
//! The targets: a single action is local work plus at most one round trip, so
//! its time does not grow with what the workspace keeps; a bulk call moves at
//! least a thousand items a second; a file goes in at disk or network speed.
//! With `TILO_THROUGHPUT_ASSERT=1` the test fails when a target does not keep;
//! either way it prints the table, and writes it as JSON to
//! `TILO_THROUGHPUT_OUT` when set.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard, and the times are printed for reading"
)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::language::host;

/// The most one action may take, at the smallest size measured: an
/// in-process action is tens of milliseconds optimized and some hundreds
/// unoptimized, so this is far above noise and far below the seconds a wait
/// on a view or on a timeout adds.
const SINGLE_CEILING_MS: f64 = if cfg!(debug_assertions) {
    2000.0
} else {
    300.0
};

/// The most the slowest of a row's actions may take, at the smallest size
/// measured: a row's first action may merge what the rows before it wrote
/// (a few hundred milliseconds for their bulk writes), so its tail is
/// allowed three times what the median is.
const SINGLE_P99_CEILING_MS: f64 = SINGLE_CEILING_MS * 3.0;

/// The app whose issues the first kind makes, and the app whose conversations the second does.
const TASKS_APP: &str = "so.tilo.tasks";
const CHAT_APP: &str = "so.tilo.chat";
/// Where the tracker's issues are listed, as `tilo ls` names them.
const ISSUES_AT: &str = "so.tilo.tasks/Tasks";

/// One kind of thing the benchmark makes: where it lives, the action that
/// makes one and the app's own list call, where it has one.
struct Kind {
    name: &'static str,
    /// The app whose `open` founds what it keeps.
    app: &'static str,
    /// The action that makes one.
    action: &'static str,
    /// The action that makes a list of them in one call, the argument it
    /// takes the list as and the most one call takes, where the app has one.
    list: Option<(&'static str, &'static str, usize)>,
}

fn issue(index: usize) -> Value {
    json!({"title": format!("Issue {index}")})
}

/// What the `index`th of `kind` is made with.
fn args_for(kind: &Kind, index: usize) -> Value {
    match kind.name {
        "tasks issue" => issue(index),
        _ => json!({"body": format!("message {index}: a line of chat")}),
    }
}

/// One item as the kind's list call takes it, where the app takes a list, or else as the single
/// action takes it.
fn list_item(kind: &Kind, index: usize) -> Value {
    if kind.list.is_some() {
        json!(format!("Issue {index}"))
    } else {
        args_for(kind, index)
    }
}

/// What an action's argument makes that a read can look for: unique to it.
fn marker(kind: &Kind, index: usize) -> String {
    match kind.name {
        "tasks issue" => format!("Issue {index}"),
        _ => format!("message {index}: a line of chat"),
    }
}

const KINDS: &[Kind] = &[
    Kind {
        name: "tasks issue",
        app: TASKS_APP,
        action: "new_task",
        list: Some(("new_tasks", "titles", 200)),
    },
    Kind {
        name: "chat message",
        app: CHAT_APP,
        action: "send",
        list: None,
    },
];

/// The index of the tracker's kind in [`KINDS`]: the issues the MCP calls and the delete by id
/// add to and take from.
const ISSUES: usize = 0;

fn quantile(durations: &[Duration], quantile: f64) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

fn sizes() -> Vec<usize> {
    let mut sizes: Vec<usize> = std::env::var("TILO_THROUGHPUT_SIZES").ok().map_or_else(
        || vec![0, 1000],
        |sizes| {
            sizes
                .split(',')
                .filter_map(|size| size.trim().parse().ok())
                .collect()
        },
    );
    // Each size fills on from the one before it, in one workspace.
    sizes.sort_unstable();
    sizes
}

fn count(env: &str, default: usize) -> usize {
    std::env::var(env)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// One measured row of the table.
struct Row {
    kind: String,
    size: usize,
    single_p50_ms: f64,
    single_p99_ms: f64,
    seed_per_s: f64,
    bulk_per_s: f64,
    second_device_p50_ms: Option<f64>,
}

impl Row {
    fn json(&self) -> Value {
        json!({
            "kind": self.kind,
            "size": self.size,
            "single_p50_ms": self.single_p50_ms,
            "single_p99_ms": self.single_p99_ms,
            "seed_per_s": self.seed_per_s,
            "bulk_per_s": self.bulk_per_s,
            "second_device_p50_ms": self.second_device_p50_ms,
        })
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// `app` started in the workspace with no window, as the workspace's own start does: its records'
/// actions are answered there from then on.
async fn open_app(host: &host::Host, app: &str) {
    host::start_app(host, app)
        .await
        .unwrap_or_else(|refused| panic!("the workspace starts {app}: {refused:?}"));
}

/// The address of what `answer`, a call that made one thing, names, under the app `app` that
/// made it.
fn made_address(answer: &Value, app: &str) -> String {
    for field in ["resources", "made"] {
        if let Some(entry) = answer[field].as_array().and_then(|made| made.first()) {
            if let Some(address) = entry["address"].as_str().or_else(|| entry["path"].as_str()) {
                return address.to_owned();
            }
            if let Some(id) = entry.as_str().or_else(|| entry["id"].as_str()) {
                return format!("{app}/{id}");
            }
        }
    }
    panic!("the call names what it made: {answer}")
}

/// The action `resource#action` with `args`, as the device's own agent runs it through the
/// tool surface `tilo do` drives; its typed answer, which must say it ran.
async fn act(agent: &host::Agent, resource: &str, action: &str, args: &Value) -> Value {
    let answer = agent
        .tool_structured(
            "tilo_do",
            &json!({"path": format!("{resource}#{action}"), "args": args}),
        )
        .await;
    assert_ne!(
        answer["isError"], true,
        "{resource}#{action} {args}: {answer}"
    );
    answer["structuredContent"].clone()
}

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let failed = result["isError"].as_bool().unwrap_or(false);
    (text, failed)
}

/// Bytes `bytes` into `workspace` in one request: the `tilo.content:` name they answer.
async fn upload(key: &host::Key, workspace: &str, bytes: &[u8]) -> String {
    let hash = blake3::hash(bytes).to_hex().to_string();
    let (status, reply) = key
        .request(
            "PUT",
            &format!("/v1/workspaces/{workspace}/contents/{hash}"),
            &[("content-type", "application/octet-stream")],
            bytes,
        )
        .await;
    assert!(status == 200 || status == 201, "upload: {status} {reply}");
    reply
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("an upload names its content: {reply}"))
        .to_owned()
}

/// How long after `since` a read of `listed_as` from another device of the person names `marker`;
/// `None` when no read of it did within five seconds.
async fn heard(
    other: &host::Agent,
    listed_as: &str,
    marker: &str,
    since: Instant,
) -> Option<Duration> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (read, failed) = said(&other.tool("tilo_read", &json!({"path": listed_as})).await);
        if !failed && read.contains(marker) {
            return Some(since.elapsed());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// How long after `since` another device of the person lists `name` in the folder `dir`;
/// `None` when no listing of it did within five seconds.
async fn heard_file(
    other: &host::Agent,
    dir: &str,
    name: &str,
    since: Instant,
) -> Option<Duration> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (text, failed) = said(
            &other
                .tool(
                    "tilo_do",
                    &json!({"path": "files#list", "args": {"folder": dir}}),
                )
                .await,
        );
        if !failed && text.contains(name) {
            return Some(since.elapsed());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[test]
#[ignore = "fills workspaces to thousands of records and times every path in a release build; \
            the throughput check runs it"]
fn a_workspace_takes_records_and_files_at_the_same_speed_however_full() {
    host::run_with_packages(&["tasks", "chat"], &[], |host| async move {
        let singles = count("TILO_THROUGHPUT_SINGLES", 40);
        let bulk = count("TILO_THROUGHPUT_BULK", 1000);
        let key = host.page_key("Bench");
        // The device's own agent, what makes and reads what the rows time; and a second agent
        // session of the person's device, which reads what a call made as it lands.
        let agent = host.agent();
        let other = host.agent();
        // The workspace the daemon founded for its person, filled one size at a time. It is the
        // only one a key reaches: the daemon answers its person's keys in the workspace it
        // enters through.
        let workspace = host.workspace().to_owned();
        for kind in KINDS {
            open_app(&host, kind.app).await;
        }
        // The channel the messages go into, made once as a person makes one; a read of it lists
        // what was sent there.
        let channel = {
            let made = act(
                &agent,
                "so.tilo.chat",
                "new_channel",
                &json!({"name": "bench"}),
            )
            .await;
            made_address(&made, CHAT_APP)
        };
        // Where one of each kind's actions goes, and where a read of what it made names it.
        let at = [TASKS_APP.to_owned(), channel.clone()];
        let read_at = [ISSUES_AT.to_owned(), channel];
        // What of each kind the workspace already keeps, as this test added it: a size fills to
        // its own count, and what an earlier size's own calls made stays for the later ones.
        let mut present: Vec<usize> = vec![0; KINDS.len()];
        // Every title and message this test makes, numbered once: a marker a read looks for is
        // one call's alone.
        let mut made: usize = 0;
        let mut table: Vec<Row> = Vec::new();

        for size in sizes() {
            // The calls an agent makes over MCP, as a key reaches them: making an issue, listing
            // a tracker and reading it. The target is the same as a person's: local work and at
            // most one round trip, so under 300 ms.
            let calls = 3;
            let mut took = Vec::with_capacity(calls);
            for index in 0..calls {
                let began = Instant::now();
                let (text, failed) = said(
                    &agent
                        .tool(
                            "tilo_do",
                            &json!({"path": "so.tilo.tasks#new_task",
                                    "args": {"title": format!("mcp-{size}-{index}")}}),
                        )
                        .await,
                );
                took.push(began.elapsed());
                assert!(!failed, "an MCP write runs: {text}");
            }
            present[ISSUES] += calls;
            table.push(Row {
                kind: "mcp write".into(),
                size,
                single_p50_ms: ms(quantile(&took, 0.5)),
                single_p99_ms: ms(quantile(&took, 0.99)),
                seed_per_s: 0.0,
                bulk_per_s: 0.0,
                second_device_p50_ms: None,
            });
            for path in [TASKS_APP.to_owned(), at[ISSUES].clone()] {
                let mut took = Vec::with_capacity(calls);
                for _ in 0..calls {
                    let began = Instant::now();
                    let (listed, failed) =
                        said(&agent.tool("tilo_ls", &json!({"path": path})).await);
                    took.push(began.elapsed());
                    assert!(!failed, "an MCP listing runs: {listed}");
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
                let (read, failed) = said(
                    &agent
                        .tool("tilo_read", &json!({"path": read_at[ISSUES]}))
                        .await,
                );
                took.push(began.elapsed());
                assert!(!failed, "an MCP read runs: {read}");
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

            for (k, kind) in KINDS.iter().enumerate() {
                // The workspace filled to `size` of them, in bulk where the app takes one: only
                // what an earlier size left it short of.
                let missing = size.saturating_sub(present[k]);
                let mut seeded: Vec<Value> = Vec::with_capacity(missing);
                for _ in 0..missing {
                    seeded.push(list_item(kind, made));
                    made += 1;
                }
                present[k] += missing;
                let began = Instant::now();
                match kind.list {
                    Some((action, field, most)) => {
                        for chunk in seeded.chunks(most) {
                            act(&agent, TASKS_APP, action, &json!({ field: chunk })).await;
                        }
                    }
                    None => {
                        for item in &seeded {
                            act(&agent, &at[k], kind.action, item).await;
                        }
                    }
                }
                let seeded = began.elapsed();
                let seed_per_s = if missing == 0 {
                    0.0
                } else {
                    missing as f64 / seeded.as_secs_f64()
                };

                // One at a time, each heard by another device of the person.
                let mut took = Vec::with_capacity(singles);
                let mut heard_latencies = Vec::new();
                for _ in 0..singles {
                    let index = made;
                    made += 1;
                    let args = args_for(kind, index);
                    let began = Instant::now();
                    act(&agent, &at[k], kind.action, &args).await;
                    took.push(began.elapsed());
                    let marker = marker(kind, index);
                    if let Some(arrived) = heard(&other, &read_at[k], &marker, began).await {
                        heard_latencies.push(arrived);
                    }
                }
                present[k] += singles;

                // In bulk, where the app takes a list in one call.
                let bulk_per_s = match kind.list {
                    Some((action, field, most)) => {
                        let mut more: Vec<Value> = Vec::with_capacity(bulk);
                        for _ in 0..bulk {
                            more.push(list_item(kind, made));
                            made += 1;
                        }
                        let began = Instant::now();
                        for chunk in more.chunks(most) {
                            act(&agent, TASKS_APP, action, &json!({ field: chunk })).await;
                        }
                        present[k] += bulk;
                        bulk as f64 / began.elapsed().as_secs_f64()
                    }
                    None => {
                        eprintln!("{}: the app takes no list call", kind.name);
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
                    second_device_p50_ms: (!heard_latencies.is_empty())
                        .then(|| ms(quantile(&heard_latencies, 0.5))),
                };
                eprintln!(
                    "{:>14} at {size:>6}: single p50 {:7.1} ms p99 {:7.1} ms | seed {:8.0}/s | bulk {:8.0}/s | second device p50 {:?} ms ({} of {singles} heard)",
                    row.kind,
                    row.single_p50_ms,
                    row.single_p99_ms,
                    row.seed_per_s,
                    row.bulk_per_s,
                    row.second_device_p50_ms,
                    heard_latencies.len()
                );
                table.push(row);
            }

            // Issues named by their own path, as the tracker's listing names them: the address
            // names the item, so the call costs its action, not a page of the listing.
            let (status, listed) = key
                .get(&format!(
                    "/v1/workspaces/{workspace}/resources?path={ISSUES_AT}&limit=1000"
                ))
                .await;
            assert_eq!(status, 200, "{listed}");
            let named: Vec<String> = listed["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["path"].as_str().map(str::to_owned))
                .take(singles)
                .collect();
            let mut took = Vec::with_capacity(named.len());
            for path in &named {
                let began = Instant::now();
                act(&agent, path, "remove", &json!({})).await;
                took.push(began.elapsed());
            }
            present[ISSUES] = present[ISSUES].saturating_sub(named.len());
            let row = Row {
                kind: "delete by id".into(),
                size,
                single_p50_ms: ms(quantile(&took, 0.5)),
                single_p99_ms: ms(quantile(&took, 0.99)),
                seed_per_s: 0.0,
                bulk_per_s: 0.0,
                second_device_p50_ms: None,
            };
            eprintln!(
                "{:>14} at {size:>6}: single p50 {:7.1} ms p99 {:7.1} ms ({} deletes)",
                row.kind,
                row.single_p50_ms,
                row.single_p99_ms,
                took.len()
            );
            table.push(row);

            // Files: one put after another (upload, then `files#open`), as `tilo files put`
            // sends them today.
            let files = count("TILO_THROUGHPUT_FILES", 200);
            let mut took = Vec::with_capacity(files);
            let mut heard_latencies = Vec::new();
            let mut waiting = Duration::ZERO;
            let began = Instant::now();
            for index in 0..files {
                let one = Instant::now();
                let body = format!("fn file_{index}() {{}}\n").repeat(40);
                let content = upload(&key, &workspace, body.as_bytes()).await;
                let dir = format!("src/m{}", index / 50);
                let name = format!("f{index}.rs");
                act(
                    &agent,
                    "files",
                    "open",
                    &json!({"files": [{"content": content, "name": name, "dir": dir}]}),
                )
                .await;
                took.push(one.elapsed());
                if index % 20 == 0 {
                    let listening = Instant::now();
                    if let Some(arrived) = heard_file(&other, &dir, &name, one).await {
                        heard_latencies.push(arrived);
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
                (!heard_latencies.is_empty()).then(|| ms(quantile(&heard_latencies, 0.5)))
            );
            table.push(Row {
                kind: "file put".into(),
                size,
                single_p50_ms: ms(quantile(&took, 0.5)),
                single_p99_ms: ms(quantile(&took, 0.99)),
                seed_per_s: 0.0,
                bulk_per_s: files_per_s,
                second_device_p50_ms: (!heard_latencies.is_empty())
                    .then(|| ms(quantile(&heard_latencies, 0.5))),
            });
        }

        // One large file's bytes in.
        let large = count("TILO_THROUGHPUT_LARGE_MB", 64);
        if large > 0 {
            let piece = 64 * 1024 * 1024;
            let bytes: Vec<u8> = (0..large.min(64) * 1024 * 1024)
                .map(|index| (index as u32).wrapping_mul(2_654_435_761).to_le_bytes()[0])
                .collect();
            let began = Instant::now();
            let content = if bytes.len() <= piece {
                upload(&key, &workspace, &bytes).await
            } else {
                // In pieces, as a file larger than one request goes up.
                let hash = blake3::hash(&bytes).to_hex().to_string();
                let route = format!("/v1/workspaces/{workspace}/contents/{hash}");
                let mut sent = Value::Null;
                let mut at = 0usize;
                while at < bytes.len() {
                    let end = (at + piece).min(bytes.len());
                    // `bytes <from>-<to>/<size>`
                    let content_range =
                        format!("bytes {at}-{}/{size}", end - 1, size = bytes.len());
                    let (status, answer) = key
                        .request(
                            "PUT",
                            &route,
                            &[
                                ("content-type", "application/octet-stream"),
                                ("content-range", content_range.as_str()),
                            ],
                            &bytes[at..end],
                        )
                        .await;
                    assert!(status == 200 || status == 201, "{answer}");
                    sent = answer;
                    at = end;
                }
                sent["content"]
                    .as_str()
                    .unwrap_or_else(|| panic!("the last piece names the content: {sent}"))
                    .to_owned()
            };
            let uploaded = began.elapsed();
            // Into the workspace's folder, as `tilo files put` places it.
            let placing = Instant::now();
            act(
                &agent,
                "files",
                "open",
                &json!({"files": [{"content": content, "name": "large.bin", "dir": "big"}]}),
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
            let rows: Vec<Value> = table.iter().map(Row::json).collect();
            std::fs::write(&out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
        }

        // Whatever else is asked: one action in a workspace keeping nothing is local work and
        // one round trip, never a wait. An action that waited on a view, or on anything that
        // times out, takes seconds here.
        let smallest = table.iter().map(|row| row.size).min().unwrap_or(0);
        let slow: Vec<String> = table
            .iter()
            .filter(|row| !row.kind.starts_with("upload") && row.size == smallest)
            .filter(|row| {
                row.single_p50_ms > SINGLE_CEILING_MS || row.single_p99_ms > SINGLE_P99_CEILING_MS
            })
            .map(|row| {
                format!(
                    "{} at {}: single p50 {:.1} ms p99 {:.1} ms",
                    row.kind, row.size, row.single_p50_ms, row.single_p99_ms
                )
            })
            .collect();
        assert!(
            slow.is_empty(),
            "one action takes over {SINGLE_CEILING_MS} ms at the median, or {SINGLE_P99_CEILING_MS} ms at p99:\n{}",
            slow.join("\n")
        );
        if std::env::var("TILO_THROUGHPUT_ASSERT").is_ok_and(|value| value == "1") {
            let mut failed = String::new();
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
                // The kinds whose app takes no list call send nothing in bulk, and a file put and
                // a delete by id are their own paths.
                let bulk_kind = row.kind == "tasks issue";
                if bulk_kind && row.bulk_per_s < 1000.0 {
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
    });
}
