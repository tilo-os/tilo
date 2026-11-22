//! A listing of a tracker's issues over the API costs each page its rows:
//! the same page at two thousand issues costs what it does at a hundred, so
//! an agent listing a company's tracker pays for the rows it reads.
//!
//! The times are printed for reading, and the growth asserted is a bound, not
//! a duration: on a loaded machine and in an unoptimized build they vary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_api::Server;

/// The sizes listed: a small company's tracker, and a big one's.
const SIZES: [usize; 2] = [100, 2_000];

/// The issues one `create-many` makes: a list stays one part's worth, and one
/// action's payload stays under what one action may carry.
const LIST: usize = 100;

/// The page sizes a caller lists with: the CLI's, and the largest a listing
/// answers (a larger `limit` is clamped to it).
const PAGES: [usize; 2] = [40, 200];

/// The most a listing of two thousand issues may take in an unoptimized
/// build on a shared machine: a bound on the whole, not a duration to keep.
/// The most listing two thousand issues may take in an unoptimized build on
/// a shared machine: a bound on the whole, not a duration to keep. It was
/// minutes before a page cost its rows.
const LISTING_CEILING: Duration = Duration::from_secs(90);

/// A key's calls of the API.
pub(super) struct V1<'a> {
    pub(super) api: &'a Arc<Server>,
    pub(super) token: String,
}

impl V1<'_> {
    fn headers(&self) -> hyper::HeaderMap {
        let bearer = format!("Bearer {}", self.token);
        headers(&[
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ])
    }

    pub(super) async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &[u8],
    ) -> (u16, Value) {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let reply = Box::pin(
            self.api
                .dispatch(&method, path, query, &self.headers(), body),
        )
        .await;
        json_of(reply).await
    }

    pub(super) async fn get(&self, path: &str) -> (u16, Value) {
        self.send(hyper::Method::GET, path, b"").await
    }

    pub(super) async fn act(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        args: &Value,
    ) -> Value {
        let (mut status, mut reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args.to_string().as_bytes(),
            )
            .await;
        // A call that outlasts the request's wait answers its invocation, which is followed
        // until it settles, as a caller does on a loaded machine.
        while status == 202 && reply["status"] == "pending" {
            let invocation = reply["invocation"].as_str().unwrap().to_owned();
            (status, reply) = self
                .get(&format!("{base}/invocations/{invocation}?wait=60"))
                .await;
        }
        assert_eq!(status, 200, "{resource}#{action}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action}: {reply}");
        reply["result"].clone()
    }
}

/// Fill the tracker with `size` issues, two hundred to a call as an import
/// makes them, a sixth of each call sub-issues of earlier ones in it.
/// An issue's description, as long as one written in a tracker: a few
/// kilobytes, so a part holds a couple of hundred and a tracker of thousands
/// is kept in many, as a moved-in one is.
fn description(at: usize) -> String {
    format!(
        "What issue {at} is about, as it was written there. {}",
        "The service keeps its work beside its peers. ".repeat(50)
    )
}

async fn fill(agent: &V1<'_>, base: &str, size: usize) {
    for start in (0..size).step_by(LIST) {
        let end = (start + LIST).min(size);
        let list: Vec<Value> = (start..end)
            .map(|at| {
                let mut entry = json!({
                    "key": format!("k{}", at - start + 1),
                    "title": format!("Issue {at}"),
                    "body": description(at),
                    "status": "todo",
                    "labels": ["Migration"],
                });
                if at % 6 == 5
                    && at > start
                    && let Some(fields) = entry.as_object_mut()
                {
                    fields.insert("under".into(), json!(format!("k{}", at - start)));
                }
                entry
            })
            .collect();
        agent
            .act(
                base,
                "tasks/tasks.issues",
                "create-many",
                &json!({"issues": list}),
            )
            .await;
    }
}

pub(super) fn quantile(took: &[Duration], quantile: f64) -> Duration {
    let mut sorted = took.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// Every row the listing at `path` holds, a page at a time as the cursor each
/// page answers leads on, and how long each page took.
pub(super) async fn every_page(
    agent: &V1<'_>,
    base: &str,
    limit: usize,
) -> (Vec<Value>, Vec<Duration>) {
    let mut rows = Vec::new();
    let mut took = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let at = cursor
            .as_deref()
            .map(|cursor| format!("&cursor={cursor}"))
            .unwrap_or_default();
        let began = Instant::now();
        let (status, page) = agent
            .get(&format!(
                "{base}/resources?path=tasks/Issues&limit={limit}{at}"
            ))
            .await;
        took.push(began.elapsed());
        assert_eq!(status, 200, "{page}");
        let mut got = page["rows"].as_array().cloned().unwrap_or_default();
        let next = page["next_cursor"].as_str().map(str::to_owned);
        // A page can be short of its limit and still go on: a resource offers
        // many rows, so a page ends where the rows do. One with nothing on it
        // and a cursor would lead nowhere.
        assert!(
            !got.is_empty() || next.is_none(),
            "a page with no rows and a cursor leads nowhere: {page}"
        );
        rows.append(&mut got);
        match next {
            Some(next) => cursor = Some(next),
            None => return (rows, took),
        }
        assert!(took.len() < 10_000, "the cursor leads somewhere");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listing_a_tracker_costs_each_page_its_rows_at_any_size() {
    let on_server = api_on_server(super::api::ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = V1 {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Mover").await,
    };
    let mut per_row = Vec::new();
    for size in SIZES {
        let (status, workspace) = agent
            .send(
                hyper::Method::POST,
                "/v1/workspaces",
                json!({"name": format!("Company of {size}")})
                    .to_string()
                    .as_bytes(),
            )
            .await;
        assert_eq!(status, 201, "{workspace}");
        let base = format!("/v1/workspaces/{}", workspace["id"].as_str().unwrap());
        agent.act(&base, "tasks", "open", &json!({})).await;
        fill(&agent, &base, size).await;
        // The first page builds what the listing reads; every page after it
        // pays its own rows.
        let (status, warm) = agent
            .get(&format!("{base}/resources?path=tasks/Issues&limit=40"))
            .await;
        assert_eq!(status, 200, "{warm}");
        // What one call of the API costs before it reads a row: a listing of
        // the app's own page, whose rows do not grow with the tracker.
        let mut base_took = Vec::new();
        for _ in 0..5 {
            let began = Instant::now();
            let (status, _) = agent
                .get(&format!("{base}/resources?path=tasks&limit=40"))
                .await;
            base_took.push(began.elapsed());
            assert_eq!(status, 200);
        }
        eprintln!(
            "{size} issues: a call of the app's own page takes p50 {:?}",
            quantile(&base_took, 0.5)
        );
        for limit in PAGES {
            let began = Instant::now();
            let (rows, took) = every_page(&agent, &base, limit).await;
            let total = began.elapsed();
            assert_eq!(
                rows.len(),
                size,
                "every issue is listed once at {size} issues, {limit} a page"
            );
            let (first, last) = (
                took.first().copied().unwrap_or_default(),
                took.last().copied().unwrap_or_default(),
            );
            let slowest = took.iter().max().copied().unwrap_or_default();
            let row = total.as_secs_f64() / size as f64;
            eprintln!(
                "{size} issues, {limit} a page: {} pages in {total:?}; first {first:?}, last \
                 {last:?}, slowest {slowest:?}, p50 {:?}, {row:.5} s per row",
                took.len(),
                quantile(&took, 0.5)
            );
            assert!(
                total < LISTING_CEILING,
                "listing {size} issues at {limit} a page took {total:?}"
            );
            assert!(
                last <= first * 4 + Duration::from_millis(100),
                "the last page at {size} issues took {last:?}, the first {first:?}"
            );
            per_row.push((size, limit, row));
        }
        if size == SIZES[1] {
            // A page answers a thousand rows (the listing's bound): a tracker this size is
            // listed in two calls, and each is still one answer's worth of memory.
            let (status, page) = agent
                .get(&format!("{base}/resources?path=tasks/Issues&limit=1000"))
                .await;
            assert_eq!(status, 200, "{page}");
            assert_eq!(
                page["rows"].as_array().map(Vec::len),
                Some(1000),
                "a page answers a thousand rows: {}",
                page["rows"].as_array().map(Vec::len).unwrap_or_default()
            );
            assert!(page["next_cursor"].is_string(), "{page}");
        }
    }
    let small = per_row
        .iter()
        .filter(|(size, _, _)| *size == SIZES[0])
        .map(|(_, _, row)| *row)
        .fold(f64::INFINITY, f64::max);
    for (size, limit, row) in &per_row {
        assert!(
            *row <= small * 3.0 + 0.002,
            "a row at {size} issues, {limit} a page costs {row:.5} s, at {} {small:.5} s",
            SIZES[0]
        );
    }
}

/// A tracker held in many parts is listed whole after its server restarts:
/// the restarted server holds only what it read since, and a listing reads
/// every part it reaches, never ending at the newest one it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tracker_is_listed_whole_after_its_server_restarts() {
    const SIZE: usize = 300;
    let on_server = api_on_server(super::api::ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &browser, "Mover").await;
    let base = {
        let agent = V1 {
            api: &on_server.api,
            token: token.clone(),
        };
        let (status, workspace) = agent
            .send(
                hyper::Method::POST,
                "/v1/workspaces",
                json!({"name": "Company"}).to_string().as_bytes(),
            )
            .await;
        assert_eq!(status, 201, "{workspace}");
        let base = format!("/v1/workspaces/{}", workspace["id"].as_str().unwrap());
        agent.act(&base, "tasks", "open", &json!({})).await;
        fill(&agent, &base, SIZE).await;
        let (before, _) = every_page(&agent, &base, 40).await;
        assert_eq!(
            before.len(),
            SIZE,
            "every issue is listed before the restart"
        );
        base
    };
    let on_server = on_server.restart(false).await;
    let agent = V1 {
        api: &on_server.api,
        token,
    };
    for limit in PAGES {
        let began = Instant::now();
        let (rows, took) = every_page(&agent, &base, limit).await;
        eprintln!(
            "after the restart, {limit} a page: {} rows in {} pages, {:?}",
            rows.len(),
            took.len(),
            began.elapsed()
        );
        let names: std::collections::HashSet<&str> =
            rows.iter().filter_map(|row| row["name"].as_str()).collect();
        assert_eq!(
            (rows.len(), names.len()),
            (SIZE, SIZE),
            "every issue is listed once after the restart, {limit} a page"
        );
    }
}

/// A person admitted into a workspace moves its governance, and with it the
/// policy every caller's access is read under: an agent listing the tracker
/// right after must be answered from the replica it already acts through,
/// at about what a listing costs any other time, not after every part of
/// the tracker was taken in again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_right_after_a_person_is_admitted_costs_what_any_listing_does() {
    const SIZE: usize = 600;
    let on_server = api_on_server(super::api::ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = V1 {
        api: &on_server.api,
        token: approved_key(&on_server, &ada, &browser, "Mover").await,
    };
    let (status, workspace) = agent
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            json!({"name": "Company"}).to_string().as_bytes(),
        )
        .await;
    assert_eq!(status, 201, "{workspace}");
    let id = workspace["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{id}");
    agent.act(&base, "tasks", "open", &json!({})).await;
    fill(&agent, &base, SIZE).await;
    let listing = format!("{base}/resources?path=tasks/Issues&limit=40");
    let list = || async {
        let began = Instant::now();
        let (status, page) = agent.get(&listing).await;
        assert_eq!(status, 200, "{page}");
        assert!(
            page["rows"].as_array().is_some_and(|rows| !rows.is_empty()),
            "{page}"
        );
        began.elapsed()
    };
    list().await;
    let mut usual = Vec::new();
    for _ in 0..5 {
        usual.push(list().await);
    }
    let usual = quantile(&usual, 0.5);

    let (status, invited) = agent
        .send(
            hyper::Method::POST,
            &format!("{base}/resources/workspace/invitations/actions/invite"),
            json!({"role": "editor"}).to_string().as_bytes(),
        )
        .await;
    assert_eq!(status, 200, "{invited}");
    let link = invited["result"]["link"]
        .as_str()
        .expect("an invitation's link");
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa", 1);
    let (_rosa_page, joined) =
        super::api_on_server::accept_invitation(&on_server, link, &rosa, "Rosa").await;
    assert_eq!(joined["workspace"], id.as_str(), "{joined}");

    let after = list().await;
    let next = list().await;
    eprintln!(
        "{SIZE} issues: a listing takes {usual:?}; right after an admission {after:?}, then \
         {next:?}"
    );
    assert!(
        after <= usual * 4 + Duration::from_millis(250),
        "the listing right after an admission took {after:?}, any other {usual:?}"
    );
}

/// A listing of another app, asked while a tracker of thousands of issues is first listed after
/// its server restarts, answers within a few times what it costs on its own and far within
/// what the tracker takes. The tracker's parts were read and handed to its app on the thread
/// serving the workspace, and the other app's listing waited behind all of them: half a
/// minute here, beside a listing of its own that takes tens of milliseconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_apps_listing_waits_for_no_tracker_taking_in_its_parts_after_a_restart() {
    let on_server = api_on_server(super::api::ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &browser, "Mover").await;
    let base = {
        let agent = V1 {
            api: &on_server.api,
            token: token.clone(),
        };
        let (status, workspace) = agent
            .send(
                hyper::Method::POST,
                "/v1/workspaces",
                json!({"name": "Company"}).to_string().as_bytes(),
            )
            .await;
        assert_eq!(status, 201, "{workspace}");
        let base = format!("/v1/workspaces/{}", workspace["id"].as_str().unwrap());
        agent.act(&base, "tasks", "open", &json!({})).await;
        agent.act(&base, "contacts", "open", &json!({})).await;
        fill(&agent, &base, SIZES[1]).await;
        base
    };
    let on_server = on_server.restart(false).await;
    let agent = V1 {
        api: &on_server.api,
        token,
    };
    let contacts = format!("{base}/resources?path=contacts&limit=40");
    let list_contacts = || async {
        let began = Instant::now();
        let (status, page) = agent.get(&contacts).await;
        assert_eq!(status, 200, "{page}");
        began.elapsed()
    };
    // The first listing after the restart joins the workspace.
    list_contacts().await;
    let mut alone = Vec::new();
    for _ in 0..5 {
        alone.push(list_contacts().await);
    }
    let alone = quantile(&alone, 0.5);
    let issues = format!("{base}/resources?path=tasks/Issues&limit=40");
    let tracker = async {
        let began = Instant::now();
        let (status, page) = agent.get(&issues).await;
        assert_eq!(status, 200, "{page}");
        began.elapsed()
    };
    let beside = async {
        // The tracker's listing starts taking in its parts first.
        tokio::time::sleep(Duration::from_millis(300)).await;
        list_contacts().await
    };
    let (tracker, beside) = tokio::join!(tracker, beside);
    eprintln!(
        "the tracker's first listing after a restart took {tracker:?}; the contacts listed beside it in {beside:?}, alone in {alone:?}"
    );
    assert!(
        beside <= alone * 8 + Duration::from_secs(2),
        "the contacts listed in {beside:?} beside the tracker's first listing ({tracker:?}), alone in {alone:?}"
    );
}

/// A new workspace through the API, as `agent`: its API base.
async fn workspace_named(agent: &V1<'_>, name: &str) -> String {
    let body = json!({ "name": name }).to_string();
    let (status, workspace) = agent
        .send(hyper::Method::POST, "/v1/workspaces", body.as_bytes())
        .await;
    assert_eq!(status, 201, "{workspace}");
    format!("/v1/workspaces/{}", workspace["id"].as_str().unwrap())
}

/// The service thread that serves the workspace at `path`.
fn thread_of(path: &str) -> usize {
    let id = path.rsplit('/').next().unwrap();
    tilo_agent_session::workspace_actions::jobs::thread_of(
        tilo_server_protocol::WorkspaceId::parse(id).unwrap(),
    )
}

/// Where the time goes, as the service logs it, when `RUST_LOG` asks.
fn trace_service() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .try_init();
    }
}

/// A tracker kept in many parts, listed first after its server restarts, answers that listing,
/// and a request of another workspace the same thread serves, asked while it is listed, answers
/// at once: one workspace's listing never holds the others' requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_trackers_listing_holds_no_other_request() {
    const SIZE: usize = 1_000;
    /// How long another workspace's request may wait while the tracker is listed, beyond
    /// a multiple of what it takes alone on this machine.
    const OTHER: Duration = Duration::from_secs(3);
    trace_service();
    let on_server = api_on_server(super::api::ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &browser, "Mover").await;
    let (base, other) = {
        let agent = V1 {
            api: &on_server.api,
            token: token.clone(),
        };
        let base = workspace_named(&agent, "Company").await;
        agent.act(&base, "tasks", "open", &json!({})).await;
        let began = Instant::now();
        fill(&agent, &base, SIZE).await;
        eprintln!("{SIZE} issues made in {:?}", began.elapsed());
        // Another workspace the same thread serves: one served by another thread never
        // waited for it.
        let other = loop {
            let other = workspace_named(&agent, "Elsewhere").await;
            if thread_of(&other) == thread_of(&base) {
                break other;
            }
        };
        agent.act(&other, "tasks", "open", &json!({})).await;
        (base, other)
    };
    let on_server = on_server.restart(false).await;
    let api = Arc::clone(&on_server.api);
    let agent = V1 {
        api: &api,
        token: token.clone(),
    };
    // The other workspace is joined again first: what its own join costs is not a wait. A
    // restarted server's device is not named as online yet, and its answer says to try again
    // shortly, so the join is asked for until it lands.
    let began = Instant::now();
    let (status, elsewhere) = loop {
        let (status, elsewhere) = agent
            .get(&format!("{other}/resources?path=tasks&limit=40"))
            .await;
        if status == 200 || began.elapsed() > Duration::from_secs(30) {
            break (status, elsewhere);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(status, 200, "{elsewhere}");
    // What the other workspace's request takes on its own here, so a busy machine does not
    // read as a wait.
    let mut alone = Vec::new();
    for _ in 0..3 {
        let began = Instant::now();
        let (status, page) = agent
            .get(&format!("{other}/resources?path=tasks&limit=40"))
            .await;
        assert_eq!(status, 200, "{page}");
        alone.push(began.elapsed());
    }
    let alone = quantile(&alone, 0.5);
    let listing = {
        let (api, token, base) = (Arc::clone(&api), token.clone(), base.clone());
        tokio::spawn(async move {
            let agent = V1 { api: &api, token };
            let began = Instant::now();
            let (status, page) = agent
                .get(&format!("{base}/resources?path=tasks/Issues&limit=40"))
                .await;
            (status, page, began.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let began = Instant::now();
    let (status, elsewhere) = agent
        .get(&format!("{other}/resources?path=tasks&limit=40"))
        .await;
    let other_took = began.elapsed();
    let (first_status, first, first_took) = listing.await.unwrap();
    eprintln!(
        "after the restart: the tracker's first page in {first_took:?}, another workspace's in \
         {other_took:?}, alone in {alone:?}"
    );
    assert_eq!(status, 200, "{elsewhere}");
    assert_eq!(first_status, 200, "{first}");
    assert!(
        other_took <= alone * 8 + OTHER,
        "another workspace's request waited {other_took:?} for the tracker's listing, \
         alone {alone:?}"
    );
    assert!(
        first_took < LISTING_CEILING,
        "the first listing after the restart took {first_took:?}"
    );
    assert_eq!(
        first["rows"].as_array().map_or(0, Vec::len),
        40,
        "the first page lists its issues: {first}"
    );
}
