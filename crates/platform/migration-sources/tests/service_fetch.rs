//! What fetching from a service costs: a repository's pages and one request per issue for what
//! the forge keeps per issue, with those requests in flight together. A slow local forge counts
//! the requests and times them.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    reason = "a fixture's own counts and the test's own failures"
)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use migration_sources::service::forgejo;
use serde_json::{Value, json};
use test_http::{Reply, Responder};

const PAGE: usize = 50;
const REPOSITORY: &str = "/api/v1/repos/acme/app";

/// A local forge holding one repository.
struct Forge {
    server: Responder,
    base: String,
}

impl Forge {
    /// A forge of `issues` issues, each with `comments` comments, answering after `delay`, the
    /// first `balk` requests told to wait.
    fn start(issues: usize, comments: usize, delay: Duration, balk: usize) -> Self {
        let waited = Arc::new(AtomicUsize::new(0));
        let server = Responder::serve(move |peer| {
            let mut target = peer.request().target.clone();
            loop {
                let reply = if waited.fetch_add(1, Ordering::SeqCst) < balk {
                    Some(Reply::status(429).header("Retry-After", "0"))
                } else {
                    std::thread::sleep(delay);
                    answer(&target, issues, comments)
                };
                let Some(reply) = reply else {
                    return;
                };
                peer.reply(&reply.keep_alive());
                let Some(next) = peer.next_request() else {
                    return;
                };
                target = next.target;
            }
        });
        Self {
            base: server.url(),
            server,
        }
    }

    /// What of the repository is reviewed, as a person would type its address.
    fn scope(&self) -> forgejo::Scope {
        forgejo::scope(&format!("{}/acme/app", self.base)).expect("the forge's address")
    }

    /// Every request the forge was asked, once the fetch is done.
    fn asked(&self) -> Vec<test_http::Request> {
        self.server.drain_requests()
    }
}

fn answer(target: &str, issues: usize, comments: usize) -> Option<Reply> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let page = query
        .split('&')
        .find_map(|part| part.strip_prefix("page="))
        .and_then(|page| page.parse::<usize>().ok())
        .unwrap_or(1);
    if path == "/api/v1/user" {
        return Some(plain(&json!({ "login": "someone" })));
    }
    if path == REPOSITORY {
        return Some(plain(&json!({
            "id": 1,
            "full_name": "acme/app",
            "has_issues": true,
        })));
    }
    if path == format!("{REPOSITORY}/issues") {
        let all: Vec<Value> = (1..=issues).map(|number| issue(number, comments)).collect();
        return Some(plain(&json!(paged(&all, page))));
    }
    if path == format!("{REPOSITORY}/issues/comments") {
        let all: Vec<Value> = (1..=issues)
            .flat_map(|number| (0..comments).map(move |at| comment(number, at)))
            .collect();
        return Some(plain(&json!(paged(&all, page))));
    }
    // One issue's comments, the shape a fetch asks them in when it asks per issue.
    if let Some(number) = path
        .strip_prefix(&format!("{REPOSITORY}/issues/"))
        .and_then(|rest| rest.strip_suffix("/comments"))
        .and_then(|number| number.parse::<usize>().ok())
    {
        let all: Vec<Value> = (0..comments).map(|at| comment(number, at)).collect();
        return Some(plain(&json!(paged(&all, page))));
    }
    if path.ends_with("/blocks") || path.ends_with("/dependencies") {
        return Some(plain(&json!([])));
    }
    None
}

fn paged(entries: &[Value], page: usize) -> Vec<Value> {
    entries
        .iter()
        .skip((page.saturating_sub(1)) * PAGE)
        .take(PAGE)
        .cloned()
        .collect()
}

fn issue(number: usize, comments: usize) -> Value {
    json!({
        "id": 1000 + number,
        "number": number,
        "title": format!("Issue {number}"),
        "body": "",
        "state": "open",
        "html_url": format!("https://forge.example/acme/app/issues/{number}"),
        "created_at": "2024-01-01T00:00:00Z",
        "updated_at": "2024-01-01T00:00:00Z",
        "user": { "login": "someone", "full_name": "Some One" },
        "comments": comments,
    })
}

fn comment(number: usize, at: usize) -> Value {
    json!({
        "id": number * 100 + at,
        "issue_url": format!("https://forge.example/api/v1/repos/acme/app/issues/{number}"),
        "body": format!("A word on issue {number}"),
        "created_at": "2024-01-02T00:00:00Z",
        "updated_at": "2024-01-02T00:00:00Z",
        "html_url": format!("https://forge.example/acme/app/issues/{number}#comment"),
        "user": { "login": "someone", "full_name": "Some One" },
    })
}

fn plain(body: &Value) -> Reply {
    Reply::status(200)
        .header("content-type", "application/json")
        .body(body.to_string())
}

fn path_of(target: &str) -> &str {
    target.split_once('?').map_or(target, |(path, _)| path)
}

/// A comment asked one issue at a time, the shape this fetch left behind.
fn per_issue_comment(target: &str) -> bool {
    let path = path_of(target);
    path.ends_with("/comments")
        && path
            .rsplit('/')
            .nth(1)
            .is_some_and(|part| part.parse::<u64>().is_ok())
}

/// Blocks or dependencies, which a forge only holds per issue.
fn relation(target: &str) -> bool {
    let path = path_of(target);
    path.ends_with("/blocks") || path.ends_with("/dependencies")
}

fn chosen(issues: usize) -> BTreeSet<String> {
    (1..=issues)
        .map(|number| (1000 + number).to_string())
        .collect()
}

/// A fetch of `issues` issues: its request count, and the requests that are neither blocks nor
/// dependencies, which cost a page of the repository's issues or comments each.
fn fetched(issues: usize, comments: usize, delay: Duration) -> (Duration, usize, usize) {
    let forge = Forge::start(issues, comments, delay, 0);
    let started = Instant::now();
    let landed = forgejo::issues(
        &forge.scope(),
        "key",
        &chosen(issues),
        &AtomicBool::new(false),
    )
    .expect("the forge answers");
    let elapsed = started.elapsed();
    assert_eq!(landed.issues.len(), issues);
    assert_eq!(
        landed
            .issues
            .iter()
            .map(|issue| issue.comments.len())
            .sum::<usize>(),
        issues * comments
    );
    let asked = forge.asked();
    assert_eq!(
        asked
            .iter()
            .filter(|request| per_issue_comment(&request.target))
            .count(),
        0,
        "a comment is asked for issue by issue again"
    );
    let relations = asked
        .iter()
        .filter(|request| relation(&request.target))
        .count();
    (elapsed, asked.len(), relations)
}

#[test]
fn a_page_of_issues_costs_its_pages_and_its_relations() {
    let issues = 100;
    let delay = Duration::from_millis(50);
    let (elapsed, asked, relations) = fetched(issues, 1, delay);
    // Blocks and dependencies are asked for every issue, and nothing else per issue is.
    assert_eq!(relations, 2 * issues);
    let pages = asked - relations;
    assert!(
        pages <= 12,
        "{pages} requests besides blocks and dependencies for {issues} issues"
    );
    // One at a time these would take the count times the delay; together they take its share.
    let mut sequential = delay * u32::try_from(asked).expect("a request count");
    sequential /= 2;
    assert!(
        elapsed < sequential,
        "{asked} requests took {elapsed:?}, no faster than one at a time ({sequential:?})"
    );
}

#[test]
fn a_large_fetch_stays_a_page_of_requests() {
    let issues = 2000;
    let delay = Duration::from_millis(2);
    let (elapsed, asked, relations) = fetched(issues, 1, delay);
    assert_eq!(relations, 2 * issues);
    let pages = asked - relations;
    assert!(
        pages <= 100,
        "{pages} requests besides blocks and dependencies for {issues} issues"
    );
    let mut sequential = delay * u32::try_from(asked).expect("a request count");
    sequential /= 2;
    assert!(
        elapsed < sequential,
        "{asked} requests took {elapsed:?}, no faster than one at a time ({sequential:?})"
    );
}

#[test]
fn a_service_that_asks_to_wait_is_asked_again() {
    let forge = Forge::start(0, 0, Duration::from_millis(1), 1);
    let title = forgejo::check(&forge.scope(), "key").expect("the retried check answers");
    assert!(title.starts_with("Forgejo:"), "{title}");
    assert!(
        forge.asked().len() > 2,
        "the request the forge asked to wait for was not asked again"
    );
}
