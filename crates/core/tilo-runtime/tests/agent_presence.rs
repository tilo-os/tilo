//! An agent in who is here: present from the moment its session connects and waiting before it
//! asks anything, heard again by the page each time its client comes back, staying while its
//! session lasts however long it goes without acting, and gone as soon as its session ends.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider as _;

use crate::agent_fixture::{act, events, ls, watch};
use crate::host;

/// The longest a page may still list an agent after its session ended.
const LEAVES_WITHIN: Duration = Duration::from_secs(4);
/// The longest an open page may take to list a run that came back.
const COMES_BACK_WITHIN: Duration = Duration::from_secs(6);
/// How long the quiet agent says nothing: longer than a model thinks, and longer than a host
/// kept a run that took no action.
const QUIET: Duration = Duration::from_secs(35);

/// Who is here, as the agent following the workspace's presence reads it.
async fn here(agent: &host::Agent) -> Vec<Value> {
    let (watched, failed) = watch(agent, json!({"wait_ms": 500})).await;
    assert!(!failed, "the agent watches who is here: {watched}");
    listed(&watched)
}

/// Who is here, as a page of the person's reads it: a key of theirs calling the workspace's
/// MCP tools while a window of theirs is open.
async fn on_the_page(page: &host::Key) -> Vec<Value> {
    let result = page.tool("tilo_watch", &json!({"wait_ms": 500})).await;
    let (watched, failed) = crate::agent_fixture::said(&result);
    assert!(!failed, "the page watches who is here: {watched}");
    listed(&watched)
}

/// The entries of who is here a `tilo_watch` answer names.
fn listed(watched: &str) -> Vec<Value> {
    events(watched)
        .into_iter()
        .filter(|event| event["kind"] == "presence")
        .flat_map(|event| event["here"].as_array().cloned().unwrap_or_default())
        .collect()
}

/// The entries of who is here that are the person's agents.
fn agents(listed: &[Value]) -> Vec<&Value> {
    listed
        .iter()
        .filter(|entry| {
            entry["kind"]
                .as_str()
                .is_some_and(|kind| kind.contains("agent"))
        })
        .collect()
}

/// An agent is present from the moment its session connects, before it asks anything: who is
/// here names it as an agent of the person's.
#[test]
fn an_agent_is_present_waiting_as_soon_as_it_connects() {
    host::run(|host| async move {
        let agent = host.agent();
        let connected = Instant::now();
        loop {
            let listed = here(&agent).await;
            if !agents(&listed).is_empty() {
                break;
            }
            assert!(
                connected.elapsed() < COMES_BACK_WITHIN,
                "the agent is present before its first request: {listed:?}"
            );
        }
        // Its first request moves that same entry on, rather than a second one appearing.
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        let listed = here(&agent).await;
        assert_eq!(
            agents(&listed).len(),
            1,
            "the run's first request moves its entry on: {listed:?}"
        );
    });
}

/// An agent whose session closes leaves who is here as soon as its session is gone, rather than
/// once the page gave up waiting for its word.
#[test]
fn an_agent_whose_session_closes_leaves_who_is_here() {
    host::run(|host| async move {
        let watcher = host.key("A page");
        let agent = host.agent();
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        assert!(
            !agents(&on_the_page(&watcher).await).is_empty(),
            "the connected agent is here"
        );
        let ended = Instant::now();
        drop(agent);
        while !agents(&on_the_page(&watcher).await).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "the agent is still here {:?} after its session ended",
                ended.elapsed()
            );
        }
    });
}

/// The person stops their agent from its card: its session ends for good, and it leaves who is
/// here with it.
#[test]
fn a_stopped_agent_leaves_who_is_here() {
    host::run(|host| async move {
        use tilo_actions::remote::mcp_server;
        let watcher = host.key("A page");
        let agent = host.agent();
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        assert!(
            !agents(&on_the_page(&watcher).await).is_empty(),
            "the connected agent is here"
        );
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: mcp_server::target(mcp_server::STOP),
            argument: mcp_server::argument(&mcp_server::AgentSessionRef {
                grant: String::new(),
                session: "Claude Code".into(),
            }),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = host
            .dispatch()
            .invoke(&host.caller(), request)
            .await
            .expect("the dispatch answers")
            .expect("the person stops their agent");
        assert!(
            !matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Rejected { .. }
            ),
            "{receipt:?}"
        );
        let ended = Instant::now();
        while !agents(&on_the_page(&watcher).await).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "the stopped agent is still here {:?} after its session ended",
                ended.elapsed()
            );
        }
        // The stopped session stays closed: the agent is not said again.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            agents(&on_the_page(&watcher).await).is_empty(),
            "a stopped agent came back on its own"
        );
    });
}

/// One computer runs its agent again and again: the page, open the whole time, lists each run
/// once, and drops it as its session ends.
#[test]
fn an_agent_that_comes_back_is_heard_by_an_open_page_every_time() {
    host::run(|host| async move {
        let watcher = host.key("A page");
        let mut seen = std::collections::BTreeSet::new();
        for run in 0..4 {
            let agent = host.agent();
            let (_, failed) = ls(&agent, None).await;
            assert!(!failed, "run {run}: the agent lists the workspace");
            let opened = Instant::now();
            loop {
                let here_now = on_the_page(&watcher).await;
                let listed = agents(&here_now);
                if let [only] = listed.as_slice()
                    && seen.insert(only["who"].as_str().unwrap_or_default().to_owned())
                {
                    break;
                }
                assert!(
                    opened.elapsed() < COMES_BACK_WITHIN,
                    "run {run}: the open page never heard the agent come back: {listed:?}"
                );
            }
            let ended = Instant::now();
            drop(agent);
            while !agents(&on_the_page(&watcher).await).is_empty() {
                assert!(
                    ended.elapsed() < LEAVES_WITHIN,
                    "run {run}: the agent is still here {:?} after its session ended",
                    ended.elapsed()
                );
            }
        }
    });
}

/// The page that stayed open lists the run after one that ended as soon as it starts, with
/// nothing left behind by the one that ended.
#[test]
fn the_run_after_one_that_ended_is_on_the_page_at_once() {
    host::run(|host| async move {
        let watcher = host.key("A page");
        for run in 0..3 {
            let agent = host.agent();
            let (_, failed) = ls(&agent, None).await;
            assert!(!failed, "run {run}: the agent lists the workspace");
            let opened = Instant::now();
            while agents(&on_the_page(&watcher).await).is_empty() {
                assert!(
                    opened.elapsed() < COMES_BACK_WITHIN,
                    "run {run}: the page never listed the agent {:?} after its session opened",
                    opened.elapsed()
                );
            }
            drop(agent);
            let ended = Instant::now();
            while !agents(&on_the_page(&watcher).await).is_empty() {
                assert!(
                    ended.elapsed() < LEAVES_WITHIN,
                    "run {run}: the agent is still here {:?} after its session ended",
                    ended.elapsed()
                );
            }
            // The page sits with the agent gone while its client restarts the server.
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// An agent stays in who is here for as long as its session lasts, however long it goes without
/// acting: one whose session was killed leaves, and the one started right after it, which took
/// one action and then waits on its person, is still listed, once, until its own session ends.
#[test]
fn a_quiet_agent_stays_while_its_session_lasts() {
    host::run(|host| async move {
        let watcher = host.key("A page");
        // One run of the computer's, killed: its session ends without a word, and it leaves.
        let killed = host.agent();
        let (_, failed) = ls(&killed, None).await;
        assert!(!failed, "the killed run lists the workspace");
        assert!(
            !agents(&on_the_page(&watcher).await).is_empty(),
            "it is here"
        );
        drop(killed);
        let ended = Instant::now();
        while !agents(&on_the_page(&watcher).await).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "the killed agent is still here {:?} after its session ended",
                ended.elapsed()
            );
        }
        // The next run takes one action and then waits on its person, quiet.
        let waiting = host.agent();
        let (_, failed) = act(&waiting, "device#info", json!({})).await;
        assert!(!failed, "the waiting run takes one action");
        let quiet = Instant::now();
        while quiet.elapsed() < QUIET {
            let here_now = on_the_page(&watcher).await;
            let listed = agents(&here_now);
            assert_eq!(
                listed.len(),
                1,
                "the waiting agent is listed once {:?} after its last action, with its session \
                 open: {listed:?}",
                quiet.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        drop(waiting);
        let ended = Instant::now();
        while !agents(&on_the_page(&watcher).await).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "the agent is still here {:?} after its session ended",
                ended.elapsed()
            );
        }
    });
}
