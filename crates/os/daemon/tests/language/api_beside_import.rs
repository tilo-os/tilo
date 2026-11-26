//! A key's calls while an import goes on in the same workspace: a person's computer posts into
//! the workspace's chat as fast as it can, one message after another, and her backend's key
//! lists her tasks, her devices and her folder all the while, through the API server as
//! `tilo ls -w` asks it. The messages are another app's writes: every listing answers whole, and
//! a folder of more files than a page keeps lists whole, never "kept changing" for writes to
//! Chat.
//!
//! What a listing asks is counted, not timed: each listing asks the host for the pages it asks
//! with nothing being written, no more and no other, so it reads nothing again for what the
//! import moved. The request kinds the old workspace-actions queue counted as waits are gone
//! with it; what the one dispatch keeps is that a listing asks the same pages and answers whole
//! beside the import. A shared machine's times vary with its load while what a listing asks does
//! not; the times are printed for reading.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::large_futures,
    reason = "in a test these are the assertion, not a hazard; one scenario in order, its future \
              spanning the daemon it starts"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::language::host;
use crate::language::host::act;
use crate::language::host::made_address;
use crate::language::host::open_app;

/// The app the import writes into, Chat, another app than the ones listed, whose conversations
/// the chat's actions name.
const CHAT_APP: &str = "so.tilo.chat";
/// The app whose `open` founds the tracker.
const TASKS_APP: &str = "so.tilo.tasks";

/// More files than one page of a listing keeps, so the folder is read over several pages.
const FILES: usize = 300;

/// How often the key lists while the import goes on.
const ROUNDS: usize = 8;

/// The listings of a round, in the order the key asks them.
const LISTINGS: [&str; 3] = ["tasks", "devices", "files"];

/// The pages the key's listings asked the host for, in the order asked: the provider,
/// the resource and the rows of each, as the agent surface names them when its asks are traced.
static ASKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Keeps every page the agent surface asks the host for in [`ASKED`].
struct Counted;

/// The `message` field of a traced event, as the line it was written as.
#[derive(Default)]
struct Message {
    text: Option<String>,
}

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.text = Some(format!("{value:?}"));
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Counted {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "tilo_mcp::timing" {
            return;
        }
        let mut message = Message::default();
        event.record(&mut message);
        // `TIMING ask.catalogue <µs> provider=… parent=… rows=…`: what was asked, not how long.
        let Some(line) = message.text else { return };
        let Some(rest) = line.strip_prefix("TIMING ask.catalogue ") else {
            return;
        };
        if let Some((_, asked)) = rest.split_once(' ') {
            ASKED.lock().unwrap().push(asked.to_owned());
        }
    }
}

/// Count the agent surface's asks into [`ASKED`], once in the process.
fn count() {
    static TRACED: std::sync::Once = std::sync::Once::new();
    TRACED.call_once(|| {
        host::also_trace(Box::new(Counted));
    });
}

/// What `call` asked the host for, how long it took, and what it answered. The agent
/// surface notes an ask once its answer is back, before the listing answers, so every ask of
/// the call is in.
async fn asked<T>(call: impl std::future::Future<Output = T>) -> (Vec<String>, Duration, T) {
    let from = ASKED.lock().unwrap().len();
    let started = Instant::now();
    let answer = call.await;
    let took = started.elapsed();
    (ASKED.lock().unwrap()[from..].to_vec(), took, answer)
}

/// Every row `path` lists, page after page as `tilo ls` reads them, with what the key was answered
/// when a page was refused.
async fn listed_whole(key: &host::Key, path: &str) -> Result<usize, String> {
    let (mut rows, mut cursor) = (0, None::<String>);
    loop {
        let page = match &cursor {
            Some(cursor) => format!("{path}&cursor={cursor}"),
            None => path.to_owned(),
        };
        let (status, listed) = key.get(&page).await;
        if status != 200 {
            return Err(format!("{status} {listed}"));
        }
        rows += listed["rows"].as_array().map_or(0, Vec::len);
        match listed["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(rows),
        }
    }
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
    reply["content"]
        .as_str()
        .unwrap_or_else(|| panic!("an upload names its content: {reply}"))
        .to_owned()
}

#[test]
fn a_keys_listings_answer_whole_beside_an_import_into_another_app() {
    host::run_with_packages(&["tasks", "chat"], &[], |host| async move {
        beside_an_import(&host).await;
    });
}

/// The same, alone on its machine for the throughput check, whose log keeps the times it prints.
#[test]
#[ignore = "the throughput check runs it alone, for the times it prints"]
fn an_import_into_another_app_never_slows_a_keys_listings() {
    host::run_with_packages(&["tasks", "chat"], &[], |host| async move {
        beside_an_import(&host).await;
    });
}

/// The key's listings, round after round beside the import. Every listing answers whole, asks
/// the host for what it asks with nothing being written, and every round finds the import
/// writing.
async fn beside_an_import(host: &host::Host) {
    count();
    let key = host.page_key("Ada's backend");
    let computer = host.page_key("Ada's computer");
    let workspace = host.workspace().to_owned();
    let base = format!("/v1/workspaces/{workspace}/resources");

    // Her computer keeps the workspace's apps, her tasks, her devices and her folder.
    open_app(host, TASKS_APP).await;
    open_app(host, CHAT_APP).await;
    // The device's own agent, what the import writes through, and the channel it writes into,
    // made once as a person makes one.
    let agent = host.agent();
    let channel = {
        let made = act(
            &agent,
            "so.tilo.chat",
            "new_channel",
            &json!({"name": "import"}),
        )
        .await;
        made_address(&made, CHAT_APP)
    };
    for index in 0..FILES {
        let body = format!("file {index:03}: a line or two.\n");
        let content = upload(&computer, &workspace, body.as_bytes()).await;
        act(
            &agent,
            "files",
            "open",
            &json!({"files": [{"content": content, "name": format!("f{index:03}.txt")}]}),
        )
        .await;
    }
    let tasks = format!("{base}?path=so.tilo.tasks/Tasks&limit=40");
    let devices = format!("{base}?path=devices&limit=40");
    let files = format!("{base}?path=files&limit=100");
    let deadline = tilo_test_env::hang_deadline();
    let files_listed = loop {
        let listed = listed_whole(&key, &files).await;
        if let Ok(rows) = listed
            && rows >= FILES
        {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "the folder never listed whole: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    // What each listing asks with nothing being written, once what a first listing finds out
    // (the workspace's apps, its forms) is known.
    let lists = Lists {
        key: &key,
        paths: [tasks, devices, files],
        files: files_listed,
    };
    lists.round("before the import").await;
    let quiet = lists.round("with nothing written").await;
    for (call, (asks, _)) in LISTINGS.iter().zip(&quiet) {
        assert!(
            !asks.is_empty(),
            "listing {call} asked the host for nothing"
        );
    }

    // It imports messages: one posted into the chat after another, as fast as it can.
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicUsize::new(0));
    let import = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        let agent = &agent;
        let channel = channel.as_str();
        async move {
            while !stop.load(Ordering::Relaxed) {
                act(agent, channel, "send", &json!({"body": "an imported line"})).await;
                written.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    };
    let rounds = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        let lists = &lists;
        async move {
            let deadline = tilo_test_env::hang_deadline();
            while written.load(Ordering::Relaxed) < 20 {
                assert!(Instant::now() < deadline, "the import never wrote");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            // The key lists beside it, each listing asking for what it asked with nothing
            // written.
            let mut during_rounds: Vec<usize> = Vec::with_capacity(ROUNDS);
            for round in 0..ROUNDS {
                let before = written.load(Ordering::Relaxed);
                let listed = lists.round(&format!("round {round}")).await;
                let during = written.load(Ordering::Relaxed) - before;
                eprintln!("  {during} messages written meanwhile");
                for ((call, (asks, _)), (alone, _)) in LISTINGS.iter().zip(&listed).zip(&quiet) {
                    assert_eq!(
                        asks, alone,
                        "listing {call} in round {round} asked the host for other pages beside \
                         the import than with nothing written"
                    );
                }
                during_rounds.push(during);
            }
            stop.store(true, Ordering::Relaxed);
            during_rounds
        }
    };
    let (during_rounds, ()) = tokio::join!(rounds, import);
    let written = written.load(Ordering::Relaxed);
    eprintln!("{written} messages written in all");
    // The import writes one message after another as fast as it can, so how many it wrote in
    // all measures how long the rounds took, not whether it kept up: what the listings were
    // beside is an import that never stopped.
    for (round, during) in during_rounds.iter().enumerate() {
        assert!(
            *during > 0,
            "the import wrote nothing while round {round} listed: {during_rounds:?}"
        );
    }
}

/// The key's listings of one workspace: its tasks, its devices and its folder.
struct Lists<'a> {
    key: &'a host::Key,
    paths: [String; 3],
    /// The rows the folder lists whole.
    files: usize,
}

impl Lists<'_> {
    /// Each listing once, in [`LISTINGS`]' order, with what it asked the host for and how long
    /// it took, printed under `name`.
    async fn round(&self, name: &str) -> Vec<(Vec<String>, Duration)> {
        let deadline = tilo_test_env::hang_deadline();
        let mut listed = Vec::with_capacity(LISTINGS.len());
        for (call, path) in LISTINGS.iter().zip(&self.paths) {
            let listing = async {
                if *call == "files" {
                    // The folder over its pages: each read on from the last one's cursor.
                    let rows = listed_whole(self.key, path).await;
                    assert_eq!(
                        rows,
                        Ok(self.files),
                        "the folder lists whole beside Sheets' writes, {name}"
                    );
                } else {
                    let (status, answer) = self.key.get(path).await;
                    assert_eq!(status, 200, "listing {call}, {name}: {answer}");
                }
            };
            let (asks, took, ()) = tokio::time::timeout_at(deadline.into(), asked(listing))
                .await
                .unwrap_or_else(|_| panic!("listing {call} never answered, {name}"));
            listed.push((asks, took));
        }
        let said: Vec<String> = LISTINGS
            .iter()
            .zip(&listed)
            .map(|(call, (asks, took))| format!("{call} {took:?} ({} asks)", asks.len()))
            .collect();
        eprintln!("{name}: {}", said.join(", "));
        listed
    }
}
