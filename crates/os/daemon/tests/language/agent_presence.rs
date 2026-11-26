//! An agent in who is here: present from the moment its session connects and waiting before it
//! asks anything, heard again by the page each time its client comes back, staying while its
//! session lasts however long it goes without acting, and gone as soon as its session ends.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider as _;

use crate::language::agent_fixture::{act, events, ls, watch};
use crate::language::host;

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

/// Who is here, as a page of the person's reads it: one of their own devices calling their
/// workspace's MCP tools while a window of theirs is open.
async fn on_the_page(page: &host::Key, workspace: &str) -> Vec<Value> {
    let result = page
        .tool(
            "tilo_watch",
            &json!({"workspace": workspace, "wait_ms": 500}),
        )
        .await;
    let (watched, failed) = crate::language::agent_fixture::said(&result);
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
        let watcher = host.page_key("A page");
        let agent = host.agent();
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        assert!(
            !agents(&on_the_page(&watcher, host.workspace()).await).is_empty(),
            "the connected agent is here"
        );
        let ended = Instant::now();
        drop(agent);
        while !agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
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
        let watcher = host.page_key("A page");
        let agent = host.agent();
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        assert!(
            !agents(&on_the_page(&watcher, host.workspace()).await).is_empty(),
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
        while !agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "the stopped agent is still here {:?} after its session ended",
                ended.elapsed()
            );
        }
        // The stopped session stays closed: the agent is not said again.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            agents(&on_the_page(&watcher, host.workspace()).await).is_empty(),
            "a stopped agent came back on its own"
        );
    });
}

/// One computer runs its agent again and again: the page, open the whole time, lists each run
/// once, and drops it as its session ends.
#[test]
fn an_agent_that_comes_back_is_heard_by_an_open_page_every_time() {
    host::run(|host| async move {
        let watcher = host.page_key("A page");
        let mut seen = std::collections::BTreeSet::new();
        for run in 0..4 {
            let agent = host.agent();
            let (_, failed) = ls(&agent, None).await;
            assert!(!failed, "run {run}: the agent lists the workspace");
            let opened = Instant::now();
            loop {
                let here_now = on_the_page(&watcher, host.workspace()).await;
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
            while !agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
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
        let watcher = host.page_key("A page");
        for run in 0..3 {
            let agent = host.agent();
            let (_, failed) = ls(&agent, None).await;
            assert!(!failed, "run {run}: the agent lists the workspace");
            let opened = Instant::now();
            while agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
                assert!(
                    opened.elapsed() < COMES_BACK_WITHIN,
                    "run {run}: the page never listed the agent {:?} after its session opened",
                    opened.elapsed()
                );
            }
            drop(agent);
            let ended = Instant::now();
            while !agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
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
        let watcher = host.page_key("A page");
        // One run of the computer's, killed: its session ends without a word, and it leaves.
        let killed = host.agent();
        let (_, failed) = ls(&killed, None).await;
        assert!(!failed, "the killed run lists the workspace");
        assert!(
            !agents(&on_the_page(&watcher, host.workspace()).await).is_empty(),
            "it is here"
        );
        drop(killed);
        let ended = Instant::now();
        while !agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
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
            let here_now = on_the_page(&watcher, host.workspace()).await;
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
        while !agents(&on_the_page(&watcher, host.workspace()).await).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "the agent is still here {:?} after its session ended",
                ended.elapsed()
            );
        }
    });
}

/// The request that stops the agent this device serves, or lets it go on, as the person asks of
/// the MCP server from their card.
fn stop(action: &str) -> tilo_actions::remote::InvokeRequest {
    use tilo_actions::remote::mcp_server;
    tilo_actions::remote::InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: mcp_server::target(action),
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
    }
}

/// The workspace's invocations, as the person reads them: the record every record read
/// reaches, holding its page of them.
async fn invocation_page(
    host: &host::Host,
) -> tilo_actions::remote::workspace_invocations::Invocations {
    let entry = host::record(
        host,
        tilo_actions::remote::workspace_invocations::PROVIDER,
        "",
    )
    .await
    .expect("the workspace reads its invocations");
    serde_json::from_value(entry["value"].clone()).expect("the page reads as one")
}

/// The person stops their agent while it runs: what it was running is cancelled and writes
/// nothing more, its next call is refused, and letting it go on answers it again. The stop is
/// the person's own ask of the MCP server, as their Settings page and their console make it.
#[test]
fn a_stopped_agent_is_cancelled_refused_and_goes_on() {
    use tilo_actions::remote::mcp_server;
    use tilo_actions::remote::workspace_invocations::InvocationStatus;

    host::run(|host| async move {
        let agent = std::sync::Arc::new(host.agent());
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        // A watch with no cursor answers what holds now, which a page draws; one that waits for
        // news holds a cursor. Read once, then wait from it: what the stop moves is the news.
        let (holds, failed) = watch(&agent, json!({"wait_ms": 0})).await;
        assert!(!failed, "the agent reads who is here: {holds}");
        let cursor = serde_json::from_str::<Value>(&holds)
            .ok()
            .and_then(|answered| {
                answered
                    .get("cursor")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .expect("the watch answers the cursor to wait from");
        let running = {
            let agent = std::sync::Arc::clone(&agent);
            tokio::spawn(async move {
                watch(&agent, json!({"wait_ms": 60_000, "cursor": cursor})).await
            })
        };
        let started = Instant::now();
        loop {
            let listed = invocation_page(&host).await;
            if listed
                .items
                .iter()
                .any(|item| item.status == InvocationStatus::Running)
            {
                break;
            }
            assert!(
                started.elapsed() < COMES_BACK_WITHIN,
                "the agent's call is running: {listed:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let stopped = host
            .dispatch()
            .invoke(&host.caller(), stop(mcp_server::STOP))
            .await
            .expect("the dispatch answers")
            .expect("the person stops their agent");
        assert!(
            matches!(
                stopped.status,
                tilo_actions::remote::ReceiptStatus::Completed { .. }
            ),
            "{stopped:?}"
        );
        let (said, failed) = tilo_test_env::within("the running call to end", running)
            .await
            .expect("the call ends");
        assert!(
            failed || said.to_lowercase().contains("cancel"),
            "the running call ended cancelled: {said}"
        );
        // The record of what it ran is settled, and nothing of it changes afterwards.
        let settled = invocation_page(&host).await;
        let ran = settled
            .items
            .iter()
            .find(|item| item.status != InvocationStatus::Running)
            .expect("the cancelled call is recorded");
        let after = invocation_page(&host).await;
        let same = after
            .items
            .iter()
            .find(|item| item.id == ran.id)
            .expect("the record stays");
        assert_eq!(
            (same.status, same.changes, same.ended_ms),
            (ran.status, ran.changes, ran.ended_ms),
            "a cancelled call writes nothing more"
        );
        // Its next call is refused, and reaches nothing.
        let (_, refused) = ls(&agent, None).await;
        assert!(refused, "a stopped agent is refused every call");
        // The person lets it go on: its next call is answered.
        let resumed = host
            .dispatch()
            .invoke(&host.caller(), stop(mcp_server::GO_ON))
            .await
            .expect("the dispatch answers")
            .expect("the person lets their agent go on");
        assert!(
            matches!(
                resumed.status,
                tilo_actions::remote::ReceiptStatus::Completed { .. }
            ),
            "{resumed:?}"
        );
        let (_, refused) = ls(&agent, None).await;
        assert!(!refused, "a let-go-on agent acts again");
    });
}

/// The person's MCP server, as their shell reads it in `workspace`: every agent it let in, and
/// the runs open now.
async fn mcp_server(
    host: &host::Host,
    workspace: &str,
) -> tilo_actions::remote::mcp_server::Server {
    use tilo_actions::remote::mcp_server;
    use tilo_session_client::daemon_documents::{DaemonShell, whole};
    let link = host
        .daemon()
        .shell_link()
        .expect("the daemon hands its shell a link");
    link.set_nonblocking(true)
        .expect("the link is made nonblocking");
    let client = runtime_collab::ShellClient::open(
        tokio::net::UnixStream::from_std(link).expect("a socket"),
    )
    .0;
    let shell = DaemonShell::new(Ok(client), tokio::runtime::Handle::current());
    // The resource's own name is what the published table keys it by, and the app id it is
    // served under is its own too (the daemon keeps it, not an app of the person's).
    let read = whole(workspace, mcp_server::RESOURCE, mcp_server::RESOURCE);
    let answer = shell
        .read(read)
        .await
        .expect("the daemon reads its MCP server");
    serde_json::from_value(answer["records"][0]["value"].clone()).expect("the server reads as one")
}

/// The MCP server lists the run this device's daemon makes for the person's agent, from the
/// moment its connection opens; the person stops it and it comes back; nobody else reads or
/// stops it.
#[test]
fn the_persons_mcp_server_lists_stops_and_lists_again_the_agent_this_device_runs() {
    use tilo_actions::remote::mcp_server;

    host::run(|host| async move {
        let agent = host.agent();
        let (_, failed) = ls(&agent, None).await;
        assert!(!failed, "the agent lists the workspace");
        let listed = mcp_server(&host, &host.workspace()).await;
        assert_eq!(listed.grants.len(), 1, "the run is listed: {listed:?}");
        assert_eq!(listed.grants[0].sessions.len(), 1, "{listed:?}");
        // The person stops it: the listing keeps the agent, with no run open.
        let stopped = host
            .dispatch()
            .invoke(&host.caller(), stop(mcp_server::STOP))
            .await
            .expect("the dispatch answers")
            .expect("the person stops their agent");
        assert!(
            matches!(
                stopped.status,
                tilo_actions::remote::ReceiptStatus::Completed { .. }
            ),
            "{stopped:?}"
        );
        let after = mcp_server(&host, &host.workspace()).await;
        assert_eq!(after.grants.len(), 1, "the agent stays: {after:?}");
        assert!(
            after.grants[0].sessions.is_empty(),
            "its run is ended: {after:?}"
        );
        // The person lets their agent go on, and the run they held ends: the agent comes back
        // as a run of its own, listed once.
        let resumed = host
            .dispatch()
            .invoke(&host.caller(), stop(mcp_server::GO_ON))
            .await
            .expect("the dispatch answers")
            .expect("the person lets their agent go on");
        assert!(
            matches!(
                resumed.status,
                tilo_actions::remote::ReceiptStatus::Completed { .. }
            ),
            "{resumed:?}"
        );
        drop(agent);
        let again = host.agent();
        let (said, failed) = ls(&again, None).await;
        assert!(!failed, "the agent comes back: {said}");
        let back = mcp_server(&host, &host.workspace()).await;
        assert_eq!(back.grants.len(), 1, "one agent, not two: {back:?}");
        assert_eq!(back.grants[0].sessions.len(), 1, "{back:?}");
        // Nobody else stops its runs. A server is read over the shell link the daemon makes for
        // its own person's shell alone, so another person has no link to read it on, and what
        // they can reach — the device's dispatch — refuses them.
        let stranger = tilo_actions::remote::Caller::local_person("someone else");
        let refused = host
            .dispatch()
            .invoke(&stranger, stop(mcp_server::STOP))
            .await;
        assert!(
            refused.as_ref().is_none_or(|answer| answer.is_err()),
            "another person stops nothing: {refused:?}"
        );
        let still = mcp_server(&host, &host.workspace()).await;
        assert_eq!(still.grants[0].sessions.len(), 1, "{still:?}");
        // The person revokes the key their agent runs under: its run ends, it leaves the
        // listing, and its key is refused from then on.
        let grant = still.grants[0].id.clone();
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::device_tickets::target(
                tilo_actions::remote::device_tickets::REVOKE,
            ),
            argument: tilo_actions::remote::device_tickets::Revoke::retired(&grant).argument(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let revoked = host
            .dispatch()
            .invoke(&host.caller(), request)
            .await
            .expect("the dispatch answers")
            .expect("the person revokes their agent's key");
        assert!(
            matches!(
                revoked.status,
                tilo_actions::remote::ReceiptStatus::Completed { .. }
            ),
            "{revoked:?}"
        );
        let until = Instant::now() + COMES_BACK_WITHIN;
        loop {
            let listed = mcp_server(&host, &host.workspace()).await;
            if listed.grants.is_empty() {
                break;
            }
            assert!(
                Instant::now() < until,
                "the revoked agent leaves the listing: {listed:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (_, refused) = ls(&again, None).await;
        assert!(refused, "a revoked agent's key is refused every call");
    });
}
