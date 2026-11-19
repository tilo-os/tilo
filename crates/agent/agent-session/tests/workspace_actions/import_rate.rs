//! A tracker brought in through the API as `tilo migrate` brings one, lists of
//! 200 issues to a `create-many`, while the person goes on working beside it:
//! listing the tracker as `tilo ls` does and making an issue of their own.
//!
//! Every list lands in about the time the issues it brings take to write, as
//! fast late in the import as early, and the person's calls answer within a few
//! of the import's lists, as fast late as early: no list waits for a part of
//! the tracker to arrive that nobody holds, no listing fails because the import
//! wrote between two of its pages, and nothing grows with what the tracker
//! already holds.
//!
//! A listing reads several pages one after another. It waits for at most the
//! one list the import is writing when it starts, never for a list begun
//! between two of its pages: a listing takes about as long as one of the
//! import's lists, where it took one list per page.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tilo_api::Server;

const LIST: usize = 200;
const LISTS: usize = 10;

/// The fewest issues a second the import lands at. A list that waited for a
/// part of the tracker nobody held took thirty seconds.
const RATE_FLOOR: f64 = 50.0;

/// The longest one of the person's calls takes while the import goes on.
const CALL_CEILING: Duration = Duration::from_secs(10);

/// How many of the import's lists a listing beside it takes at most, in the
/// middle: about one, the list it began beside. Waiting for a list at each of
/// its pages took three to four.
const LISTING_IN_LISTS: f64 = 1.75;

/// One key's calls of the API, held by whoever makes them.
#[derive(Clone)]
struct Caller {
    api: Arc<Server>,
    token: String,
}

impl Caller {
    async fn send(&self, method: hyper::Method, path: &str, body: &Value) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let pairs = [
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let body = if body.is_null() {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        // Boxed as any sendable future: the call's own type nests too deep
        // for the spawned loop that makes it to be proven sendable.
        let headers = headers(&pairs);
        let reply: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send + '_>> =
            Box::pin(self.api.dispatch(&method, path, query, &headers, &body));
        let reply = reply.await;
        json_of(reply).await
    }

    async fn act(&self, base: &str, resource: &str, action: &str, args: &Value) -> Value {
        let (status, reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args,
            )
            .await;
        assert_eq!(status, 200, "{resource}#{action}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action}: {reply}");
        reply["result"].clone()
    }

    /// What `tilo ls` of the tracker reads.
    async fn list(&self, base: &str) {
        let (status, listed) = self
            .send(
                hyper::Method::GET,
                &format!("{base}/resources?path=tasks"),
                &Value::Null,
            )
            .await;
        assert_eq!(status, 200, "ls tasks: {listed}");
    }
}

fn origin(at: usize) -> Value {
    json!({
        "author": "Grace Hopper",
        "handle": "grace@example.com",
        "source": "Linear",
        "url": format!("https://linear.example/acme/issue/ENG-{at}"),
        "reference": format!("ENG-{at}"),
        "created_ms": 1_756_720_800_000_i64,
        "updated_ms": null,
    })
}

fn listed(first: usize) -> Value {
    let issues: Vec<Value> = (first..first + LIST)
        .map(|at| {
            json!({
                "title": format!("ENG-{at} Speed up the list"),
                "body": format!("## ENG-{at}\n\nWhat the issue says there, with a step or two.\n"),
                "status": "todo",
                "labels": ["Migration"],
                "origin": origin(at),
            })
        })
        .collect();
    json!({ "issues": issues })
}

/// The middle of `took`.
fn median(took: &mut [Duration]) -> Duration {
    took.sort_unstable();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_keeps_its_pace_while_the_person_works_beside_it() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let importer = Caller {
        api: Arc::clone(&on_server.api),
        token: approved_key(&on_server, &ada, &browser, "Import").await,
    };
    let (status, made) = importer
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            &json!({ "name": "Team" }),
        )
        .await;
    assert_eq!(status, 201, "{made}");
    let base = format!("/v1/workspaces/{}", made["id"].as_str().unwrap());
    importer.act(&base, "tasks", "open", &json!({})).await;
    let issues = "tasks%2Ftasks.issues";
    // The tracker takes writes once the workspace serves the key's session, as
    // `tilo migrate` waits for before it sends its first list.
    let ready = Instant::now();
    loop {
        let (status, reply) = importer
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{issues}/actions/create"),
                &json!({ "title": "Before the import" }),
            )
            .await;
        if status == 200 && reply["status"] == "ok" {
            break;
        }
        assert!(
            ready.elapsed() < Duration::from_secs(30),
            "the tracker takes no write: {status} {reply}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The person's calls, one after another for as long as the import goes on,
    // each with how many lists had landed when it began.
    let person = importer.clone();
    let landed = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let beside = {
        let (base, landed, done) = (base.clone(), Arc::clone(&landed), Arc::clone(&done));
        tokio::spawn(async move {
            let mut calls: Vec<(usize, &'static str, Duration)> = Vec::new();
            let mut made = 0;
            while !done.load(Ordering::Acquire) {
                let at = landed.load(Ordering::Acquire);
                let began = Instant::now();
                person.list(&base).await;
                calls.push((at, "ls", began.elapsed()));
                let at = landed.load(Ordering::Acquire);
                let began = Instant::now();
                person
                    .act(
                        &base,
                        issues,
                        "create",
                        &json!({ "title": format!("The person's own {made}") }),
                    )
                    .await;
                calls.push((at, "create", began.elapsed()));
                made += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            calls
        })
    };

    let mut lists = Vec::with_capacity(LISTS);
    for list in 0..LISTS {
        let began = Instant::now();
        let answer = importer
            .act(&base, issues, "create-many", &listed(list * LIST))
            .await;
        lists.push(began.elapsed());
        let made = answer["made"]
            .as_array()
            .or_else(|| answer["value"]["made"].as_array())
            .map_or(0, Vec::len);
        assert_eq!(made, LIST, "every issue of a list is made: {answer}");
        landed.store(list + 1, Ordering::Release);
    }
    done.store(true, Ordering::Release);
    let calls = beside.await.unwrap();

    let took: Duration = lists.iter().sum();
    let rate = (LIST * LISTS) as f64 / took.as_secs_f64();
    println!("lists of {LIST} issues took {lists:?}: {rate:.0} issues a second");
    for (at, call, took) in &calls {
        println!("{call:>6} with {at:>2} lists landed: {took:?}");
    }
    assert!(
        rate >= RATE_FLOOR,
        "{} issues landed at {rate:.0} a second, under {RATE_FLOOR}",
        LIST * LISTS
    );

    // The later lists land about as fast as the first, into a tracker ten times
    // as big.
    let (mut first, mut last) = (lists[..LISTS / 2].to_vec(), lists[LISTS / 2..].to_vec());
    let (first, last) = (median(&mut first), median(&mut last));
    assert!(
        last <= first * 3 + Duration::from_millis(200),
        "the import's lists took {last:?} late, {first:?} early"
    );

    // Every call answered within a few lists' time, and the later half of the
    // import answered the person as fast as the first.
    assert!(
        calls.len() >= 4,
        "the person's calls ran beside the import: {calls:?}"
    );
    let slowest = calls.iter().map(|(_, _, took)| *took).max().unwrap();
    assert!(
        slowest <= CALL_CEILING,
        "a call beside the import took {slowest:?}, over {CALL_CEILING:?}"
    );
    // A listing beside the import waits for the list it began beside, not
    // for one list per page.
    let mut listings: Vec<Duration> = calls
        .iter()
        .filter(|(at, call, _)| *call == "ls" && *at < LISTS)
        .map(|(_, _, took)| *took)
        .collect();
    let listing = median(&mut listings);
    let list = median(&mut lists.clone());
    assert!(
        listing.as_secs_f64() <= list.as_secs_f64() * LISTING_IN_LISTS,
        "a listing beside the import took {listing:?} in the middle, a list {list:?}: {listings:?}"
    );

    let half = LISTS / 2;
    let mut early: Vec<Duration> = calls
        .iter()
        .filter(|(at, _, _)| *at < half)
        .map(|(_, _, took)| *took)
        .collect();
    let mut late: Vec<Duration> = calls
        .iter()
        .filter(|(at, _, _)| *at >= half)
        .map(|(_, _, took)| *took)
        .collect();
    if !early.is_empty() && !late.is_empty() {
        let (early, late) = (median(&mut early), median(&mut late));
        assert!(
            late <= early * 3 + Duration::from_millis(50),
            "the person's calls took {late:?} late in the import, {early:?} early"
        );
    }
}
