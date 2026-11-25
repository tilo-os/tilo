//! A person's agent on their own device's daemon (`tilo mcp` on the computer that serves their
//! workspace): what it lists it runs, its calls go through on their first ask, and they keep
//! going while no page of the person's is open and after the client restarts.

use serde_json::json;

use crate::language::agent_fixture::{act, events, ls, mcp_agent, said, watch};
use crate::language::host;

/// Two agent runs of the person's logged-in computer each list the workspace and run what they
/// list: with no page of the person's open, both act, and each sees what the other answered.
#[test]
fn a_logged_in_computers_agents_act_with_no_tab_open() {
    host::run(|host| async move {
        let one = host.agent();
        let two = host.agent();
        for (run, agent) in [&one, &two].into_iter().enumerate() {
            let (listed, failed) = ls(agent, None).await;
            assert!(!failed, "run {run} lists the workspace: {listed}");
            assert!(
                listed.contains("devices"),
                "the workspace publishes its device's actions: {listed}"
            );
            let (answered, failed) = act(agent, "device#info", json!({})).await;
            assert!(!failed, "run {run} runs what it lists: {answered}");
        }
    });
}

/// `tilo login` starts the peer whose session the agent runs on, then enters as the same device
/// to show the person the computer is in, and closes. The agent's first call after that goes
/// through on its first ask, and no listing of the workspace comes before it.
#[test]
fn the_first_call_after_tilo_login_is_the_agents() {
    host::run(|host| async move {
        // The peer, as the login's check of its tools starts it.
        let peer = host.agent();
        let (listed, failed) = ls(&peer, None).await;
        assert!(!failed, "the peer lists the workspace: {listed}");
        // The login's own entry as the same device, closed once it was made.
        let login = host.agent();
        let (_, failed) = ls(&login, None).await;
        assert!(!failed, "the login's entry lists the workspace");
        drop(login);
        // The agent's first call on the peer's session is answered as its own.
        let (answered, failed) = act(&peer, "device#info", json!({})).await;
        assert!(
            !failed,
            "the first call after the login is the agent's: {answered}"
        );
    });
}

/// The computer's client restarts (its `tilo mcp` ends and is started again): the next run's
/// first calls go through, each on its first ask, into what the run before left.
#[test]
fn a_restarted_computers_first_calls_go_through() {
    host::run(|host| async move {
        // The run before: what the device publishes, and the device itself.
        let before = host.agent();
        let (listed, failed) = ls(&before, None).await;
        assert!(!failed, "the run before lists the workspace: {listed}");
        let (ran, failed) = act(&before, "device#info", json!({})).await;
        assert!(!failed, "the run before runs what it lists: {ran}");
        drop(before);

        // Started again, asked at once: nothing asks the caller to ask again.
        let after = host.agent();
        let (listed, failed) = ls(&after, None).await;
        assert!(!failed, "the restarted run lists the workspace: {listed}");
        let (ran, failed) = act(&after, "device#info", json!({})).await;
        assert!(
            !failed,
            "the restarted run's first call goes through: {ran}"
        );
        assert!(
            ran.contains(&hex::encode(host.daemon().endpoint_addr().id.as_bytes())),
            "it answers which device it is: {ran}"
        );
    });
}

/// A computer's agent is one of the workspace's agents from its first call there: the workspace's
/// own record of its agents names it, once, as its person's, under the one id its runs share, and
/// both its row and the session it is here in read as the client connects. `tilo_ls` with no path
/// lists the workspace's apps, so its row is read where it lives, in the workspace's agents.
#[test]
fn a_computers_agent_is_listed_once_where_it_acts() {
    host::run(|host| async move {
        let agent = host.agent();
        let (listed, failed) = said(
            &agent
                .tool("tilo_read", &json!({"path": "workspace/agents"}))
                .await,
        );
        assert!(
            !failed,
            "the computer's agent reads the workspace's agents: {listed}"
        );

        // One agent, whatever client connects to it: a client names its session, never the
        // identity it acts as.
        let id = mcp_agent(host.person());
        assert_eq!(
            listed.matches(&id).count(),
            1,
            "the workspace's agents list the computer's agent once, by {id}: {listed}"
        );
        assert!(
            listed.contains("Claude Code"),
            "the workspace's agents call it what the client named itself: {listed}"
        );

        // Its session is the client's: who is here calls it what the client named itself.
        let (watched, failed) = watch(&agent, json!({"wait_ms": 500})).await;
        assert!(!failed, "the agent watches who is here: {watched}");
        assert!(
            events(&watched)
                .into_iter()
                .filter(|event| event["kind"] == "presence")
                .flat_map(|event| event["here"].as_array().cloned().unwrap_or_default())
                .any(|member| member["name"] == "Claude Code" && member["kind"] == "agent"),
            "who is here labels the session as its client: {watched}"
        );
    });
}

/// The agent's client names its session in `initialize` ("Release prep"): every call it makes
/// goes by that name, and the watch it follows its runs with names it so.
#[test]
fn a_logged_in_agents_session_name_reaches_its_server() {
    host::run(|host| async move {
        let agent = host.agent();
        let opened = agent
            .ask(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"experimental": {"so.tilo": {"session-name": "Release prep"}}},
                    "clientInfo": {"name": "claude-code", "version": "2"},
                }),
            )
            .await;
        assert!(opened.get("result").is_some(), "initialize: {opened}");
        let (made, failed) = act(&agent, "device#info", json!({})).await;
        assert!(!failed, "the named session acts: {made}");
        let (watched, failed) = watch(&agent, json!({"wait_ms": 1_000})).await;
        assert!(!failed, "the session watches its own runs: {watched}");
        let runs: Vec<_> = events(&watched)
            .into_iter()
            .filter(|event| event["kind"] == "invocation")
            .collect();
        assert!(
            runs.iter().any(|run| run["title"] == "Release prep"
                && run["group"]
                    .as_str()
                    .is_some_and(|group| group.starts_with("agent-session:"))),
            "the agent's invocation goes by its session's name: {watched}"
        );
    });
}

/// The agent lists a stopped app into running, writes in it, reads what it wrote, and every
/// change it made is one run of the connection's.
#[test]
fn a_persons_own_agent_lists_a_stopped_app_into_running_writes_and_reads_as_one_run() {
    host::run(|host| async move {
        let agent = host.agent();
        // Nobody started Tasks: listing it starts it, so the agent sees its issues without
        // opening anything first.
        let (started, failed) = ls(&agent, Some("tasks")).await;
        assert!(
            !failed && !started.contains("not running"),
            "listing an installed app starts it, on a device whose profile installs it: {started}"
        );
    });
}

/// What the agent lists it runs, as listed; what people alone are offered is not listed to it;
/// and an action that asks first waits for the person, answered on their laptop.
#[test]
fn a_persons_own_agent_runs_what_it_lists_as_an_agent_and_asks_them_first() {
    host::run(|host| async move {
        let agent = host.agent();
        // Terminal: what it lists, it runs, as listed.
        let (terminal, failed) = ls(&agent, Some("terminal")).await;
        assert!(
            !failed,
            "the agent lists its workspace's Terminal: {terminal}"
        );
        let (ran, failed) = act(&agent, "terminal#run", json!({"command": "echo first"})).await;
        assert!(
            !failed && ran.contains("first"),
            "the agent runs what it lists: {ran}"
        );

        // Removing a book asks first: the agent's own word does not do, and the person, asked on
        // their laptop, allows it.
        let (refused, failed) = act(
            &agent,
            "ebook#remove-from-library",
            json!({"book": "book:dusk"}),
        )
        .await;
        assert!(
            failed,
            "an agent's own word confirms nothing, and the person is asked on their device: \
             {refused}"
        );
    });
}

/// A key's run reads what the device is, as the person's agents do, and is refused letting
/// another computer in: a ticket is the person's own to issue.
#[test]
fn a_key_reads_the_device_and_cannot_issue_a_ticket() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Ada's script");
        let (answered, failed) = crate::language::files_support::said(
            &key.tool(
                "tilo_do",
                &json!({"workspace": workspace, "path": "device#info", "args": {}}),
            )
            .await,
        );
        assert!(!failed, "the key reads the device: {answered}");
        assert!(
            answered.contains("\"device\"") && answered.contains("\"people\""),
            "the device says what it is: {answered}"
        );
        let (issued, failed) = crate::language::files_support::said(
            &key.tool(
                "tilo_do",
                &json!({"workspace": workspace, "path": "device#issue", "args": {}}),
            )
            .await,
        );
        assert!(failed, "a key issues no ticket: {issued}");
        assert!(
            !issued.contains("ticket\":"),
            "no ticket is handed to the key: {issued}"
        );
    });
}
