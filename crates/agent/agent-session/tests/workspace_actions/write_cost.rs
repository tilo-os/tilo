//! A write through the API costs the same however full the tracker it lands in: an agent
//! bringing issues one at a time into a tracker of thousands waits for each as long as it would
//! in an empty one, and a listing asked meanwhile answers every issue the tracker holds.
//!
//! Each size gets a workspace of its own, filled the way an import fills one (a `create-many`
//! of two hundred issues per call), then takes single `create`s while another caller lists the
//! whole tracker page by page. The test fails when a single write at the larger size takes
//! clearly longer than at the empty one, or when a listing beside the writes leaves out an issue.
//!
//! `TILO_WRITE_COST_SIZE` sets the larger size (default 2,000).

#![allow(
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_api::Server;
use ui::tilo_resource::Resource as _;

/// The issues one `create-many` makes, as an import sends them.
const LIST: usize = 200;

/// The single writes timed at each size.
const SINGLES: usize = 30;

/// How much longer a single write at the larger size may take than in an empty tracker: enough
/// for a loaded machine, far below what a cost that grows with the tracker adds.
const SLACK: Duration = Duration::from_millis(40);

/// A key's calls of the API.
struct V1 {
    api: Arc<Server>,
    token: String,
}

impl V1 {
    async fn send(&self, method: hyper::Method, path: &str, body: &[u8]) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let pairs = [
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        // Boxed as any sendable future: the call's own type nests too deep for the spawned
        // listing that makes it to be proven sendable.
        let headers = headers(&pairs);
        let reply: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send + '_>> =
            Box::pin(self.api.dispatch(&method, path, query, &headers, body));
        json_of(reply.await).await
    }

    async fn act(&self, base: &str, action: &str, args: &Value) -> Value {
        let (mut status, mut reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/tasks/Issues/actions/{action}"),
                args.to_string().as_bytes(),
            )
            .await;
        while status == 202 && reply["status"] == "pending" {
            let invocation = reply["invocation"].as_str().unwrap().to_owned();
            (status, reply) = self
                .send(
                    hyper::Method::GET,
                    &format!("{base}/invocations/{invocation}?wait=60"),
                    b"",
                )
                .await;
        }
        assert_eq!(status, 200, "tasks/Issues#{action}: {reply}");
        assert_eq!(reply["status"], "ok", "tasks/Issues#{action}: {reply}");
        reply["result"].clone()
    }

    /// Every issue the tracker lists, page after page as `tilo ls` reads them: each row's
    /// path and name.
    async fn every_issue(&self, base: &str) -> Result<Vec<(String, String)>, String> {
        let (mut rows, mut cursor) = (Vec::new(), None::<String>);
        for _ in 0..10_000 {
            let at = cursor
                .as_deref()
                .map(|cursor| format!("&cursor={cursor}"))
                .unwrap_or_default();
            let (status, page) = self
                .send(
                    hyper::Method::GET,
                    &format!("{base}/resources?path=tasks/Issues&limit=200{at}"),
                    b"",
                )
                .await;
            if status != 200 {
                return Err(format!("{status} {page}"));
            }
            rows.extend(page["rows"].as_array().into_iter().flatten().map(|row| {
                let text = |field: &str| row[field].as_str().unwrap_or_default().to_owned();
                (text("path"), text("name"))
            }));
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok(rows),
            }
        }
        Err("the cursor leads nowhere".into())
    }
}

/// What a listing answered: how many rows, how many issues it listed more than once, and how
/// many of those the tracker held before the writes beside it began it left out.
#[derive(Debug, PartialEq, Eq)]
struct Listed {
    rows: usize,
    twice: usize,
    missing: usize,
}

impl Listed {
    /// `listed` against the issues `held` names.
    fn of(
        listed: Result<Vec<(String, String)>, String>,
        held: &HashSet<String>,
    ) -> Result<Self, String> {
        let listed = listed?;
        let paths: HashSet<&str> = listed.iter().map(|(path, _)| path.as_str()).collect();
        let names: HashSet<&str> = listed.iter().map(|(_, name)| name.as_str()).collect();
        Ok(Self {
            rows: listed.len(),
            twice: listed.len() - paths.len(),
            missing: held
                .iter()
                .filter(|name| !names.contains(name.as_str()))
                .count(),
        })
    }
}

/// What the service logs, where `RUST_LOG` asks for it: each request's stages, to read where a
/// write spends its time.
fn traced() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
}

fn median(mut took: Vec<Duration>) -> Duration {
    took.sort();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

/// What one size measured: the median single write, the bulk rate it was filled at, the
/// listings asked beside the writes, and the one after them.
struct Measured {
    alone: Duration,
    single: Duration,
    filled_per_s: f64,
    /// What a listing after the writes answered.
    after: Result<Listed, String>,
    /// What each listing beside the writes answered.
    listings: Vec<Result<Listed, String>>,
}

async fn measure(agent: &Arc<V1>, size: usize) -> Measured {
    let (status, made) = agent
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            json!({ "name": format!("Tracker of {size}") })
                .to_string()
                .as_bytes(),
        )
        .await;
    assert_eq!(status, 201, "{made}");
    let base = format!("/v1/workspaces/{}", made["id"].as_str().unwrap());
    let began = Instant::now();
    for start in (0..size).step_by(LIST) {
        let issues: Vec<Value> = (start..(start + LIST).min(size))
            .map(|at| json!({ "title": format!("Moved in {at}") }))
            .collect();
        agent
            .act(&base, "create-many", &json!({ "issues": issues }))
            .await;
    }
    let filled_per_s = size as f64 / began.elapsed().as_secs_f64().max(f64::EPSILON);
    // One write first, so what an empty tracker sets up once is not counted as its cost.
    agent
        .act(&base, "create", &json!({ "title": "First" }))
        .await;

    // Writes alone, then beside a caller listing the whole tracker over and over.
    let mut alone = Vec::with_capacity(SINGLES);
    tracing::info!(size, "write cost: writes alone begin");
    for at in 0..SINGLES {
        let began = Instant::now();
        agent
            .act(
                &base,
                "create",
                &json!({ "title": format!("Brought in alone {at}") }),
            )
            .await;
        alone.push(began.elapsed());
    }
    tracing::info!(size, "write cost: writes alone end");
    // What the tracker holds before the writes beside a listing: what each listing answers.
    let mut held: HashSet<String> = (0..size).map(|at| format!("Moved in {at}")).collect();
    held.insert("First".to_owned());
    held.extend((0..SINGLES).map(|at| format!("Brought in alone {at}")));

    let writing = Arc::new(AtomicBool::new(true));
    let lister = {
        let (agent, base, writing) = (Arc::clone(agent), base.clone(), Arc::clone(&writing));
        tokio::spawn(async move {
            let mut listings = Vec::new();
            while writing.load(Ordering::Relaxed) {
                listings.push(agent.every_issue(&base).await);
            }
            listings
        })
    };
    let mut took = Vec::with_capacity(SINGLES);
    for at in 0..SINGLES {
        let began = Instant::now();
        agent
            .act(
                &base,
                "create",
                &json!({ "title": format!("Brought in {at}") }),
            )
            .await;
        took.push(began.elapsed());
    }
    writing.store(false, Ordering::Relaxed);
    let listings = lister.await.unwrap();
    // One listing after the writes, so there is one however fast they went.
    let after = Listed::of(agent.every_issue(&base).await, &held);
    Measured {
        alone: median(alone),
        single: median(took),
        filled_per_s,
        after,
        listings: listings
            .into_iter()
            .map(|listed| Listed::of(listed, &held))
            .collect(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_costs_the_same_however_full_the_tracker() {
    traced();
    let size = std::env::var("TILO_WRITE_COST_SIZE")
        .ok()
        .and_then(|size| size.parse().ok())
        .unwrap_or(2_000);
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Arc::new(V1 {
        api: Arc::clone(&on_server.api),
        token: approved_key(&on_server, &ada, &browser, "Mover").await,
    });

    let empty = measure(&agent, 0).await;
    let full = measure(&agent, size).await;
    eprintln!(
        "a single write: {:?} in an empty tracker and {:?} in one of {size} issues; beside a \
         listing {:?} and {:?}; filled at {:.0} issues a second; {} listings beside the writes; \
         listed {:?} after them",
        empty.alone,
        full.alone,
        empty.single,
        full.single,
        full.filled_per_s,
        full.listings.len(),
        full.after
    );
    for (size, measured) in [(0, &empty), (size, &full)] {
        assert_eq!(
            measured.after,
            Ok(Listed {
                rows: size + 1 + 2 * SINGLES,
                twice: 0,
                missing: 0
            }),
            "the tracker lists every issue it holds, each once"
        );
    }
    for listed in empty.listings.iter().chain(&full.listings) {
        let listed = listed
            .as_ref()
            .expect("a listing beside the writes answers");
        assert_eq!(
            (listed.twice, listed.missing),
            (0, 0),
            "a listing beside the writes answers every issue the tracker held before them, each \
             once: {listed:?}"
        );
    }
    assert!(
        full.alone <= empty.alone * 2 + SLACK,
        "a single write takes {:?} in a tracker of {size} issues and {:?} in an empty one",
        full.alone,
        empty.alone
    );
    assert!(
        full.single <= empty.single * 2 + SLACK,
        "a single write beside a listing takes {:?} in a tracker of {size} issues and {:?} in an \
         empty one",
        full.single,
        empty.single
    );
}

/// The issues a `create-many` answered it made, by id.
fn made(result: &Value) -> Vec<String> {
    let made = result["made"]
        .as_array()
        .or_else(|| result["value"]["made"].as_array())
        .unwrap_or_else(|| panic!("a create-many answers what it made: {result}"));
    made.iter()
        .map(|made| made["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Where an issue moved in from.
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

/// A tracker moved in as `tilo migrate` moves one, by two movers at once while another caller
/// lists it ([`move_in`]).
/// Every call is taken, and the tracker holds every issue and every comment afterwards: a write
/// is never refused for what a write beside it changed, and nothing it wrote goes missing.
///
/// `TILO_MIGRATION_SIZE` sets how many issues (default 2,000).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_in_tracker_lands_whole_beside_a_listing() {
    const COMMENTS: usize = 2;
    traced();
    let size: usize = std::env::var("TILO_MIGRATION_SIZE")
        .ok()
        .and_then(|size| size.parse().ok())
        .unwrap_or(2_000);
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Arc::new(V1 {
        api: Arc::clone(&on_server.api),
        token: approved_key(&on_server, &ada, &browser, "Mover").await,
    });
    let (status, workspace) = agent
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            json!({ "name": "Moved in" }).to_string().as_bytes(),
        )
        .await;
    assert_eq!(status, 201, "{workspace}");
    let id = workspace["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{id}");

    let moving = Arc::new(AtomicBool::new(true));
    let lister = {
        let (agent, base, moving) = (Arc::clone(&agent), base.clone(), Arc::clone(&moving));
        tokio::spawn(async move {
            let mut listings = 0_usize;
            while moving.load(Ordering::Relaxed) {
                let _ = agent.every_issue(&base).await;
                listings += 1;
            }
            listings
        })
    };
    // Two movers at once, as an import split over several workers moves a tracker in: each
    // takes the other's writes to the parts they share while writing its own.
    let second = Arc::new(V1 {
        api: Arc::clone(&on_server.api),
        token: approved_key(&on_server, &ada, &browser, "Second mover").await,
    });
    let began = Instant::now();
    let half = (size / 2).next_multiple_of(LIST).min(size);
    tokio::join!(
        move_in(&agent, &base, (0, half), COMMENTS),
        move_in(&second, &base, (half, size), COMMENTS),
    );
    let took = began.elapsed();
    moving.store(false, Ordering::Relaxed);
    let listings = lister.await.unwrap();

    // What the workspace holds, part by part.
    let workspaces = on_server.hosted.server.workspaces().unwrap().clone();
    let (mut issues, mut comments) = (0, 0);
    for index in 0..1_024 {
        let part = tasks::issue_list::chain().part(index);
        let Some(bytes) = workspaces
            .resource_document(&id, "so.tilo.tasks", part.as_str())
            .await
            .unwrap()
        else {
            continue;
        };
        let list =
            tasks::issue_list::IssueList::read(&automerge::AutoCommit::load(&bytes).unwrap())
                .unwrap();
        issues += list.issues.len();
        comments += list
            .issues
            .values()
            .map(|issue| issue.comments.len())
            .sum::<usize>();
    }
    eprintln!(
        "{size} issues with their comments and links moved in in {took:?} beside {listings} \
         listings; the tracker holds {issues} issues and {comments} comments"
    );
    assert_eq!(issues, size, "every issue moved in is held");
    assert_eq!(comments, size * COMMENTS, "every comment moved in is held");
}

/// Issues `from..to` moved in by `agent` as `tilo migrate` moves them: two hundred to a
/// `create-many`, then their comments as one group and their links as another.
async fn move_in(agent: &V1, base: &str, (from, to): (usize, usize), comments_each: usize) {
    for first in (from..to).step_by(LIST) {
        let issues: Vec<Value> = (first..(first + LIST).min(to))
            .map(|at| {
                json!({
                    "title": format!("Issue {at}"),
                    "body": format!("What issue {at} is about, as it was written there."),
                    "status": "todo",
                    "labels": ["Migration"],
                    "origin": origin(at),
                })
            })
            .collect();
        let ids = made(
            &agent
                .act(base, "create-many", &json!({ "issues": issues }))
                .await,
        );
        assert_eq!(ids.len(), issues.len(), "every issue made is answered");
        let comments: Vec<Value> = ids
            .iter()
            .flat_map(|issue| {
                (0..comments_each).map(move |n| {
                    json!({ "action": "comment", "args": {
                        "issue": issue,
                        "text": format!("Comment {n}, moved in with its author and date."),
                        "origin": origin(n),
                    }})
                })
            })
            .collect();
        agent
            .act(base, "group", &json!({ "actions": comments }))
            .await;
        let links: Vec<Value> = ids
            .iter()
            .step_by(10)
            .zip(ids.iter().skip(1).step_by(10))
            .map(|(from, to)| {
                json!({ "action": "link", "args": {
                    "issue": from, "app": "so.tilo.tasks", "kind": "tasks.tracker.issues",
                    "resource": to, "label": "Blocks",
                }})
            })
            .collect();
        agent.act(base, "group", &json!({ "actions": links })).await;
    }
}
