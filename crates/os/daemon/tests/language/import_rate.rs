//! A tracker brought into the workspace through the API as `tilo migrate` brings one, lists of
//! 200 issues to `new_tasks`, while the person goes on working beside it: listing the tracker as
//! `tilo ls` does and making an issue of their own.
//!
//! Every list lands in about the time the issues it brings take to write, as fast late in the
//! import as early, and the person's calls answer within a few of the import's lists, as fast
//! late as early: no listing fails because the import wrote between two of its pages, and
//! nothing grows with what the tracker already keeps.
//!
//! A listing reads several pages one after another. It waits for at most the one list the import
//! is writing when it starts, never for a list begun between two of its pages: a listing takes
//! about as long as one of the import's lists.
//!
//! What the import and the person's calls cost is counted on every run: the bytes each list reads
//! and writes, late as early, and the lists that land while one listing reads. How fast they are
//! is kept to its floors by the throughput check, which runs the timed test alone.

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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::language::host;

const LIST: usize = 200;
const LISTS: usize = 10;

/// The fewest issues a second the import lands at. A list that waited for a
/// part of the tracker nobody kept took thirty seconds.
const RATE_FLOOR: f64 = 50.0;

/// The longest one of the person's calls takes while the import goes on.
const CALL_CEILING: Duration = Duration::from_secs(10);

/// How many of the import's lists a listing beside it takes at most, in the
/// middle: about one, the list it began beside. Waiting for a list at each of
/// its pages took three to four.
const LISTING_IN_LISTS: f64 = 1.75;

/// The app and the path its issues are listed at.
const TASKS: &str = "so.tilo.tasks";
/// The app the tracker's actions are addressed through, and where its issues are listed.
const TRACKER_APP: &str = "so.tilo.tasks";
const ISSUES_AT: &str = "so.tilo.tasks/Tasks";

/// The bytes of history this process read and wrote so far, as the engine
/// counts them. Each test runs in a process of its own.
fn bytes_moved() -> u64 {
    let work = tilo_peer_state::work::now();
    work.bytes_read.saturating_add(work.bytes_written)
}

/// One of the import's lists: how long it took, and the bytes the process
/// moved meanwhile.
#[derive(Clone, Copy, Debug)]
struct List {
    took: Duration,
    moved: u64,
}

/// One of the person's calls: how many of the import's lists had landed when
/// it began and when it answered, and how long it took.
#[derive(Clone, Copy, Debug)]
struct Call {
    kind: &'static str,
    began: usize,
    answered: usize,
    took: Duration,
}

/// `app` started in the workspace with no window, as the workspace's own start does: its records'
/// actions are answered there from then on.
async fn open_app(host: &host::Host, app: &str) {
    host::start_app(host, app)
        .await
        .unwrap_or_else(|refused| panic!("the workspace starts {app}: {refused:?}"));
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

/// What `tilo ls` of the tracker reads.
async fn list(key: &host::Key, workspace: &str) {
    let (status, listed) = key
        .get(&format!(
            "/v1/workspaces/{workspace}/resources?path={ISSUES_AT}&limit=40"
        ))
        .await;
    assert_eq!(status, 200, "ls tasks: {listed}");
}

fn titles(first: usize) -> Vec<String> {
    (first..first + LIST)
        .map(|at| format!("ENG-{at} Speed up the list"))
        .collect()
}

/// The middle of `took`.
fn median(took: &mut [Duration]) -> Duration {
    took.sort_unstable();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

/// The import, each list of it waited for at most the hang bound, beside the
/// person's calls.
async fn import_beside_the_person(host: &host::Host) -> (Vec<List>, Vec<Call>) {
    let workspace = host.workspace().to_owned();
    let key = host.page_key("Import");
    open_app(host, TASKS).await;
    // The device's own agent, what makes and reads what the rows time.
    let agent = host.agent();
    // The tracker takes writes once the workspace serves the key's session, as `tilo migrate`
    // waits for before it sends its first list.
    let ready = Instant::now();
    loop {
        let reply = agent
            .tool_structured(
                "tilo_do",
                &json!({"path": "so.tilo.tasks#new_task",
                        "args": {"title": "Before the import"}}),
            )
            .await;
        if reply["isError"] != true {
            break;
        }
        assert!(
            ready.elapsed() < Duration::from_secs(30),
            "the tracker takes no write: {reply}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The person's calls, one after another for as long as the import goes on,
    // each with how many lists had landed when it began.
    let landed = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let beside = {
        let (landed, done) = (Arc::clone(&landed), Arc::clone(&done));
        let key = &key;
        let agent = &agent;
        let workspace = workspace.as_str();
        async move {
            let mut calls: Vec<Call> = Vec::new();
            let mut made = 0;
            let call = |kind, began, started: Instant| Call {
                kind,
                began,
                answered: landed.load(Ordering::Acquire),
                took: started.elapsed(),
            };
            while !done.load(Ordering::Acquire) {
                let (at, started) = (landed.load(Ordering::Acquire), Instant::now());
                tilo_test_env::within(
                    "the person's listing beside the import",
                    list(key, workspace),
                )
                .await;
                calls.push(call("ls", at, started));
                let (at, started) = (landed.load(Ordering::Acquire), Instant::now());
                tilo_test_env::within(
                    "the person's own issue beside the import",
                    act(
                        agent,
                        TRACKER_APP,
                        "new_task",
                        &json!({"title": format!("The person's own {made}")}),
                    ),
                )
                .await;
                calls.push(call("create", at, started));
                made += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            calls
        }
    };

    let import = {
        let (landed, done) = (Arc::clone(&landed), Arc::clone(&done));
        let agent = &agent;
        async move {
            let mut lists = Vec::with_capacity(LISTS);
            for list in 0..LISTS {
                let (began, before) = (Instant::now(), bytes_moved());
                let answer = tilo_test_env::within(
                    format!("list {list} of the import"),
                    act(
                        agent,
                        TRACKER_APP,
                        "new_tasks",
                        &json!({ "titles": titles(list * LIST) }),
                    ),
                )
                .await;
                lists.push(List {
                    took: began.elapsed(),
                    moved: bytes_moved().saturating_sub(before),
                });
                let made = answer["made"]
                    .as_array()
                    .or_else(|| answer["value"]["made"].as_array())
                    .map_or(0, Vec::len);
                assert_eq!(made, LIST, "every issue of a list is made: {answer}");
                landed.store(list + 1, Ordering::Release);
            }
            done.store(true, Ordering::Release);
            lists
        }
    };

    let (calls, lists) = tokio::join!(beside, import);
    for list in &lists {
        println!("a list of {LIST} issues: {list:?}");
    }
    for call in &calls {
        println!("{call:?}");
    }
    assert!(
        calls.len() >= 4,
        "the person's calls ran beside the import: {calls:?}"
    );
    (lists, calls)
}

/// The middle of `counts`.
fn middle(counts: &mut [u64]) -> u64 {
    counts.sort_unstable();
    counts.get(counts.len() / 2).copied().unwrap_or_default()
}

/// The later lists move about what the first did, into a tracker ten times
/// as big, and a listing beside the import waits for the list it began
/// beside, never for one list per page: at most the list under way and the
/// next land while it reads.
#[test]
fn an_import_keeps_its_pace_while_the_person_works_beside_it() {
    host::run_with_packages(&["tasks"], &[], |host| async move {
        let (lists, calls) = import_beside_the_person(&host).await;
        let moved =
            |lists: &[List]| middle(&mut lists.iter().map(|list| list.moved).collect::<Vec<_>>());
        let (first, last) = (moved(&lists[..LISTS / 2]), moved(&lists[LISTS / 2..]));
        assert!(
            last.saturating_mul(4) <= first.saturating_mul(5),
            "the import's lists moved {last} bytes late, {first} early"
        );
        let mut landed_meanwhile: Vec<u64> = calls
            .iter()
            .filter(|call| call.kind == "ls" && call.began < LISTS)
            .map(|call| u64::try_from(call.answered.saturating_sub(call.began)).unwrap())
            .collect();
        let meanwhile = middle(&mut landed_meanwhile);
        assert!(
            meanwhile <= 2,
            "{meanwhile} of the import's lists landed while a listing beside it read, in the middle: \
             {landed_meanwhile:?}"
        );
    });
}

/// The import lands at its floor, and the person's calls answer within a few
/// of its lists, as fast late as early. Timed, so the throughput check runs it
/// alone.
#[test]
#[ignore = "timed: the throughput check runs it alone"]
fn an_import_lands_at_its_floor_while_the_person_works_beside_it() {
    host::run_with_packages(&["tasks"], &[], |host| async move {
        let (lists, calls) = import_beside_the_person(&host).await;
        let mut lists: Vec<Duration> = lists.iter().map(|list| list.took).collect();
        let calls: Vec<(usize, &str, Duration)> = calls
            .iter()
            .map(|call| (call.began, call.kind, call.took))
            .collect();
        let took: Duration = lists.iter().sum();
        let rate = (LIST * LISTS) as f64 / took.as_secs_f64();
        println!("lists of {LIST} issues took {lists:?}: {rate:.0} issues a second");
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
        let list = median(&mut lists);
        assert!(
            listing.as_secs_f64() <= list.as_secs_f64() * LISTING_IN_LISTS,
            "a listing beside the import took {listing:?} in the middle, a list {list:?}: \
             {listings:?}"
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
    });
}
