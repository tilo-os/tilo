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
struct V1<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl V1<'_> {
    fn headers(&self) -> hyper::HeaderMap {
        let bearer = format!("Bearer {}", self.token);
        headers(&[
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ])
    }

    async fn send(&self, method: hyper::Method, path: &str, body: &[u8]) -> (u16, Value) {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let reply = Box::pin(
            self.api
                .dispatch(&method, path, query, &self.headers(), body),
        )
        .await;
        json_of(reply).await
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        self.send(hyper::Method::GET, path, b"").await
    }

    async fn act(&self, base: &str, resource: &str, action: &str, args: &Value) -> Value {
        let (status, reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args.to_string().as_bytes(),
            )
            .await;
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

fn quantile(took: &[Duration], quantile: f64) -> Duration {
    let mut sorted = took.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// Every row the listing at `path` holds, a page at a time as the cursor each
/// page answers leads on, and how long each page took.
async fn every_page(agent: &V1<'_>, base: &str, limit: usize) -> (Vec<Value>, Vec<Duration>) {
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
