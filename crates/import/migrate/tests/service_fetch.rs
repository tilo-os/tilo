//! What fetching from a service costs: a repository's pages and one request per issue for what
//! the forge keeps per issue, with those requests in flight together. A local forge counts the
//! requests and holds them a page at a time.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    reason = "a fixture's own counts and the test's own failures"
)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use tilo_migrate::service::{IN_FLIGHT, forgejo};
use serde_json::{Value, json};
use test_http::{Reply, Responder};

const PAGE: usize = 50;
const REPOSITORY: &str = "/api/v1/repos/acme/app";
/// How many per-issue requests a fetch asks alone: one issue's blocks and its dependencies, to
/// learn whether the forge keeps relations at all.
const PROBE: usize = 2;

/// A local forge holding one repository.
struct Forge {
    server: Responder,
    base: String,
    /// How many requests were being answered at the same time, at most.
    peak: Arc<AtomicUsize>,
}

impl Forge {
    /// A forge of `issues` issues, each with `comments` comments, the first `balk` requests told
    /// to wait.
    ///
    /// Its per-issue requests, after the probe, are held a page at a time: each waits until
    /// [`IN_FLIGHT`] of them are waiting, then all are answered. A fetch that asks one after
    /// another never fills a page and gives up.
    fn start(issues: usize, comments: usize, balk: usize) -> Self {
        let waited = Arc::new(AtomicUsize::new(0));
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let page = Arc::new(Page::new(IN_FLIGHT));
        // Blocks and dependencies, one request each per issue. Whole pages of those after the
        // probe are held: a fetch may reach the last requests with less than a page to ask.
        let held = (2 * issues).saturating_sub(PROBE) / IN_FLIGHT * IN_FLIGHT;
        let asked = Arc::new(AtomicUsize::new(0));
        let server = Responder::serve({
            let (inflight, peak) = (Arc::clone(&inflight), Arc::clone(&peak));
            let (page, asked) = (Arc::clone(&page), Arc::clone(&asked));
            move |peer| {
                let mut target = peer.request().target.clone();
                loop {
                    let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    let reply = if waited.fetch_add(1, Ordering::SeqCst) < balk {
                        Some(Reply::status(429).header("Retry-After", "0"))
                    } else {
                        if relation(&target)
                            && (PROBE..PROBE + held).contains(&asked.fetch_add(1, Ordering::SeqCst))
                        {
                            page.hold();
                        }
                        answer(&target, issues, comments)
                    };
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    let Some(reply) = reply else {
                        return;
                    };
                    peer.reply(&reply.keep_alive());
                    let Some(next) = peer.next_request() else {
                        return;
                    };
                    target = next.target;
                }
            }
        });
        Self {
            base: server.url(),
            server,
            peak,
        }
    }

    /// The most requests the forge was answering at the same time.
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
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

/// A page of requests held together: each waits until a whole page of them are waiting, then
/// all of them go.
struct Page {
    width: usize,
    /// How many of the page are waiting, and how many pages have gone.
    state: Mutex<(usize, u64)>,
    full: Condvar,
}

impl Page {
    fn new(width: usize) -> Self {
        Self {
            width,
            state: Mutex::new((0, 0)),
            full: Condvar::new(),
        }
    }

    /// Wait for the page to fill, however long the requests still to come take.
    fn hold(&self) {
        let mut state = self.state.lock().expect("the page's own lock");
        let mine = state.1;
        state.0 += 1;
        if state.0 >= self.width {
            state.0 = 0;
            state.1 += 1;
            self.full.notify_all();
        } else {
            while state.1 == mine {
                state = self.full.wait(state).expect("the page's own lock");
            }
        }
        drop(state);
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
        return Some(plain(&json!(page_of(issues, page, |at| {
            issue(at + 1, comments)
        }))));
    }
    if path == format!("{REPOSITORY}/issues/comments") {
        let per = comments.max(1);
        return Some(plain(&json!(page_of(issues * comments, page, |at| {
            comment(at / per + 1, at % per)
        }))));
    }
    // One issue's comments, the shape a fetch asks them in when it asks per issue.
    if let Some(number) = path
        .strip_prefix(&format!("{REPOSITORY}/issues/"))
        .and_then(|rest| rest.strip_suffix("/comments"))
        .and_then(|number| number.parse::<usize>().ok())
    {
        return Some(plain(&json!(page_of(comments, page, |at| {
            comment(number, at)
        }))));
    }
    if path.ends_with("/blocks") || path.ends_with("/dependencies") {
        return Some(plain(&json!([])));
    }
    None
}

/// One page of `count` entries, `entry` making the `at`th (from zero): what the forge does
/// for a request, rather than making every entry and dropping all but the page.
fn page_of(count: usize, page: usize, entry: impl Fn(usize) -> Value) -> Vec<Value> {
    let first = page.saturating_sub(1).saturating_mul(PAGE);
    (first..first.saturating_add(PAGE).min(count))
        .map(entry)
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

/// A fetch of `issues` issues: its request count, the requests that are neither blocks nor
/// dependencies, which cost a page of the repository's issues or comments each, and the most
/// requests the forge answered at once.
fn fetched(issues: usize, comments: usize) -> (usize, usize, usize) {
    let forge = Forge::start(issues, comments, 0);
    let landed = forgejo::issues(
        &forge.scope(),
        "key",
        &chosen(issues),
        &AtomicBool::new(false),
    )
    .expect("the forge answers");
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
    let peak = forge.peak();
    assert_eq!(
        peak, IN_FLIGHT,
        "the forge answered {peak} requests at once where a fetch keeps {IN_FLIGHT} in flight"
    );
    let relations = asked
        .iter()
        .filter(|request| relation(&request.target))
        .count();
    (asked.len(), relations, peak)
}

#[test]
fn a_page_of_issues_costs_its_pages_and_its_relations() {
    let issues = 100;
    let (asked, relations, _) = fetched(issues, 1);
    // Blocks and dependencies are asked for every issue, and nothing else per issue is.
    assert_eq!(relations, 2 * issues);
    let pages = asked - relations;
    assert!(
        pages <= 12,
        "{pages} requests besides blocks and dependencies for {issues} issues"
    );
}

#[test]
fn a_large_fetch_stays_a_page_of_requests() {
    let issues = 2000;
    let (asked, relations, _) = fetched(issues, 1);
    assert_eq!(relations, 2 * issues);
    let pages = asked - relations;
    assert!(
        pages <= 100,
        "{pages} requests besides blocks and dependencies for {issues} issues"
    );
}

#[test]
fn a_service_that_asks_to_wait_is_asked_again() {
    let forge = Forge::start(0, 0, 1);
    let title = forgejo::check(&forge.scope(), "key").expect("the retried check answers");
    assert!(title.starts_with("Forgejo:"), "{title}");
    assert!(
        forge.asked().len() > 2,
        "the request the forge asked to wait for was not asked again"
    );
}
