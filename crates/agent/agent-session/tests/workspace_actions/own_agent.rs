//! A person's own agent on a computer of theirs (`tilo login`, then
//! `tilo mcp`): its session says it runs an agent, as `tilo login` makes it
//! say, and from then on the host serves it as that agent acting for the
//! person, within an agent's bounds.
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;

/// What a tool call answers, and whether it failed.
async fn tool(server: &McpServer<SessionActions>, name: &str, arguments: Value) -> (String, bool) {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": arguments}}))
        .await
        .expect("a call is answered");
    let result = &answer["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (
        if text.is_empty() {
            answer.to_string()
        } else {
            text
        },
        result["isError"] == true || answer.get("error").is_some(),
    )
}

/// A person's own agent opens the workspace's Terminal session on a terminal, as `run` is
/// asked: what is typed there on its computer is recorded on the session by the host, as the
/// commands `run` runs are, once each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_typed_on_a_terminal_the_agent_opened_is_recorded_on_the_session() {
    Box::pin(typed_on_a_terminal_is_recorded()).await;
}

async fn typed_on_a_terminal_is_recorded() {
    use ush::environment_terminals::{FromTerminal, ToTerminal, quic, wire};
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop_device = enrol(&root, "laptop", DeviceCapabilities::primary());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let shell_state = tempfile::tempdir().unwrap();
    let hosted = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options {
            shell: Some(
                tilo_server::agents::host_shells(shell_state.path().join("shells")).unwrap(),
            ),
            ..Options::offering(crate::common::catalogue)
        },
    )
    .await;
    let _asked = crate::harness::hosted::Laptop::new(
        ["Claude Code".to_owned()],
        crate::harness::hosted::Unlisted::Deny,
    )
    .spawn(&hosted.server, root.clone(), &laptop_device);
    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        hex::encode(root.verifying_key().to_bytes()),
    )
    .with_agent_session("session-7");

    let (opened, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#open-terminal", "args": {"cols": 100, "rows": 30}}),
    )
    .await;
    assert!(!failed, "{opened}");
    let after = opened.split_once("stream").map_or("", |(_, after)| after);
    let stream: String = after
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|part| part.len() == 32)
        .unwrap_or_else(|| panic!("a stream: {opened}"))
        .to_owned();

    // The agent's computer takes the stream as itself and types a command.
    let endpoint =
        test_server::endpoint(iroh::SecretKey::from_bytes(&computer.key.to_bytes())).await;
    let address = hosted.server.node_addr();
    let typed = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();
            let (connection, mut to, mut from) = quic::dial(&endpoint, address, &runtime).unwrap();
            wire::write_hello(&mut to, &wire::Hello { stream }).unwrap();
            let welcome = wire::read_welcome(&mut from).unwrap();
            assert!(
                matches!(welcome, Some(wire::Welcome::Opened { .. })),
                "{welcome:?}"
            );
            let mut drawn = Vec::new();
            let mut typed = false;
            loop {
                let text = String::from_utf8_lossy(&drawn).into_owned();
                if text.contains("typed-42") {
                    break;
                }
                // The shell's prompt (it ends `$ `, or `# ` for root) says it reads a line.
                if !typed && (text.contains("$ ") || text.contains("# ")) {
                    let line = b"echo typed-$((40+2))\r".to_vec();
                    wire::write_to(&mut to, &ToTerminal::In(line)).unwrap();
                    typed = true;
                }
                match wire::read_from(&mut from).unwrap() {
                    Some(FromTerminal::Out(bytes)) => drawn.extend(bytes),
                    other => panic!("{other:?}: {text:?}"),
                }
            }
            drop(connection);
        }),
    )
    .await;
    typed.expect("the terminal answers in time").unwrap();

    // The host recorded it on the session, by the agent, once.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (session, failed) = tool(
            &server,
            "tilo_read",
            json!({"path": "terminal/Terminal session"}),
        )
        .await;
        assert!(!failed, "{session}");
        let recorded = session.matches("echo typed-$((40+2))").count();
        if recorded > 0 {
            assert_eq!(recorded, 1, "recorded once: {session}");
            assert!(session.contains(r#""exit":0"#), "as it ended: {session}");
            assert!(
                session.contains(r#""actor":{"agent""#),
                "by the agent: {session}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never recorded: {session}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
