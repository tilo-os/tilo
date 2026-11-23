//! A key's calls while an import goes on in the same workspace: Ada's logged-in computer writes
//! spreadsheets into the workspace as fast as it reads them in, one new sheet after another, and
//! her backend's key lists her chat, her contacts and her folder all the while, through the API
//! server as `tilo ls -w` asks it. The sheets are another app's writes: every listing answers
//! whole, and a folder of more files than a page holds lists whole, never "kept changing" for
//! writes to Sheets.
//!
//! What a listing costs beside the import is counted, not timed: each listing asks the
//! workspace's host for the pages it asks for with nothing being written, no more and no other,
//! so it reads nothing again for what the import moved; and none of its requests waits on the
//! workspace's thread for anything to land, for a part to arrive or for the workspace to be
//! joined, so none waits for the import's next write. A shared machine's times vary with its
//! load while what a listing asks and waits for does not; the times are printed for reading.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key};
use super::key::Key;
use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SHEETS: &str = "so.tilo.sheets";

/// More files than one page of a listing holds, so the folder is read over several pages.
const FILES: usize = 300;

/// How often the key lists while the import goes on.
const ROUNDS: usize = 8;

/// The listings of a round, in the order the key asks them.
const LISTINGS: [&str; 3] = ["chat", "contacts", "files"];

/// The pages the key's listings asked the workspace's host for, in the order asked: the provider,
/// the resource and the rows of each, as the agent surface names them when its asks are traced.
static ASKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The requests the workspace's thread took up that waited there, for a change to land, for a
/// part or for the workspace to be joined ([`WAITS`]), by kind.
static WAITED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The kinds of request the workspace's thread takes up once a request waited there: a watch
/// for a change to land, parts that arrived for a request held for them, a workspace joined
/// that requests waited to be joined to.
const WAITS: [&str; 3] = ["changed", "loaded", "joined"];

/// Keeps every page the agent surface asks the workspace's host for in [`ASKED`], and every
/// request of [`WAITS`]' kinds the workspace's thread took up in [`WAITED`].
struct Counted;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Counted {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        #[derive(Default)]
        struct Message {
            text: Option<String>,
            kind: Option<String>,
        }
        impl tracing::field::Visit for Message {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "kind" {
                    self.kind = Some(value.to_owned());
                }
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.text = Some(format!("{value:?}"));
                }
            }
        }
        let mut message = Message::default();
        event.record(&mut message);
        if message.text.as_deref() == Some("workspace actions: a request ran") {
            if let Some(kind) = message.kind.filter(|kind| WAITS.contains(&kind.as_str())) {
                WAITED.lock().unwrap().push(kind);
            }
            return;
        }
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

/// Count the agent surface's asks into [`ASKED`] and the workspace thread's waits into
/// [`WAITED`], once in the process.
fn count() {
    static TRACED: std::sync::Once = std::sync::Once::new();
    TRACED.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::{Layer as _, util::SubscriberInitExt as _};
        tracing_subscriber::registry()
            .with(
                Counted.with_filter(
                    tracing_subscriber::filter::Targets::new()
                        .with_target("tilo_mcp::timing", tracing::Level::DEBUG)
                        .with_target(
                            "tilo_agent_session::workspace_actions",
                            tracing::Level::DEBUG,
                        ),
                ),
            )
            .try_init()
            .expect("the asks and waits are counted: nothing else in this process traces");
    });
}

/// What `call` asked the workspace's host for, how long it took, and what it answered. The agent
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
async fn listed_whole(key: &Key<'_>, path: &str) -> Result<usize, String> {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keys_listings_answer_whole_beside_an_import_into_another_app() {
    Box::pin(beside_an_import()).await;
}

/// The same, alone on its machine for the throughput check, whose log keeps the times it prints.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the throughput check runs it alone, for the times it prints"]
async fn an_import_into_another_app_never_slows_a_keys_listings() {
    Box::pin(beside_an_import()).await;
}

/// The key's listings, round after round beside the import. Every listing answers whole, asks
/// the host for what it asks with nothing being written, and every round finds the import
/// writing.
async fn beside_an_import() {
    count();
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let computer = enrol(&ada, "Ada's computer", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &computer, "Ada's backend").await;
    let key = Key { api: &api, token };
    let (status, made) = key
        .post(
            "/v1/resources/account/workspaces/actions/create",
            &json!({"name": "Company"}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{made}");
    let workspace = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let base = format!("/v1/workspaces/{workspace}");

    // Her computer holds the workspace, with her chat, her contacts and her folder.
    let mut page = super::resource_support::open_admitted_peer(
        &on_server.hosted,
        &computer,
        &on_server.host,
        &workspace,
    )
    .await;
    page.create("so.tilo.chat");
    page.create("so.tilo.contacts");
    let mut folder = super::resource_support::computer_folder(&computer.cert);
    for index in 0..FILES {
        folder
            .create_file(tilo_fs::ROOT_INO, format!("f{index:03}.txt"), None, 0, 1)
            .unwrap();
    }
    super::put_files::write_tree(&page, &mut folder).unwrap();
    page.durable().await;
    let chat = format!("{base}/resources?path=chat&limit=40");
    let contacts = format!("{base}/resources?path=contacts&limit=40");
    let files = format!("{base}/resources?path=files&limit=100");
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
    // (the workspace's apps, its forms) is known. A listing that waited for a write to land
    // would never answer here.
    let lists = Lists {
        key: &key,
        paths: [chat, contacts, files],
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

    // It imports spreadsheets: a new sheet written after another, as fast as it can.
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicUsize::new(0));
    let import = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        tokio::task::spawn_blocking(move || {
            while !stop.load(Ordering::Relaxed) {
                page.create(SHEETS);
                page.pump();
                written.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
            page
        })
    };
    let deadline = tilo_test_env::hang_deadline();
    while written.load(Ordering::Relaxed) < 20 {
        assert!(Instant::now() < deadline, "the import never wrote");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The key lists beside it, each listing asking for what it asked with nothing written.
    let mut during_rounds: Vec<usize> = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let before = written.load(Ordering::Relaxed);
        let listed = lists.round(&format!("round {round}")).await;
        let during = written.load(Ordering::Relaxed) - before;
        eprintln!("  {during} sheets written meanwhile");
        for ((call, (asks, _)), (alone, _)) in LISTINGS.iter().zip(&listed).zip(&quiet) {
            assert_eq!(
                asks, alone,
                "listing {call} in round {round} asked the host for other pages beside the \
                 import than with nothing written"
            );
        }
        during_rounds.push(during);
    }
    stop.store(true, Ordering::Relaxed);
    let page = import.await.unwrap();
    page.durable().await;
    let written = written.load(Ordering::Relaxed);
    eprintln!("{written} sheets written in all");
    // The import writes one sheet after another as fast as it can, so how many it wrote in
    // all measures how long the rounds took, not whether it kept up: the same test wrote 90-99
    // across eight rounds on a faster machine and 126 when the rounds ran longer. What the
    // listings were beside is an import that never stopped: every round found it writing.
    for (round, during) in during_rounds.iter().enumerate() {
        assert!(
            *during > 0,
            "the import wrote nothing while round {round} listed: {during_rounds:?}"
        );
    }
}

/// The key's listings of one workspace: its chat, its contacts and its folder.
struct Lists<'a> {
    key: &'a Key<'a>,
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
            let waited = WAITED.lock().unwrap().len();
            let (asks, took, ()) = tokio::time::timeout_at(deadline.into(), asked(listing))
                .await
                .unwrap_or_else(|_| panic!("listing {call} never answered, {name}"));
            let waits = WAITED.lock().unwrap()[waited..].to_vec();
            assert!(
                waits.is_empty(),
                "a request waited on the workspace's thread while {call} listed, {name}: {waits:?}"
            );
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
