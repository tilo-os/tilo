//! A person's agent on computers of theirs (`tilo login`, then `tilo mcp`) is one agent wherever it
//! runs: every session that says it runs "Claude Code" acts as the one agent the person's root and
//! that name make ([`tilo_actions::remote::device_agent_id`]), named so in what it writes, and an
//! agent under another name is another agent.
use super::*;
use tilo_mcp::McpServer;
use tilo_session_client::remote::SessionActions;

const NOTES: &str = "so.tilo.notes";

/// Who `changes` recorded acting, from the actions they carry.
fn invokers(changes: &[([u8; 32], Vec<u8>)]) -> Vec<tilo_actions::ActionInvoker> {
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in changes {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    doc.get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.invoker)
        .collect()
}

/// A note `title` made by the agent `name` on the computer `computer`, in its own session.
async fn note_by(hosted: &Hosted, computer: &Device, host: &Device, name: &str, title: &str) {
    let agent = Browser::open(hosted, computer, host).await;
    tilo_session_client::device::session_as_agent(&agent.peer, name)
        .await
        .unwrap();
    let person = hex::encode(computer.cert.user_public_key);
    let id = tilo_actions::remote::device_agent_id(&computer.cert.user_public_key, name);
    let server = McpServer::for_agent(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        &id,
        &person,
    )
    .with_agent_session(format!("session-{title}"));
    let answer = server
        .handle(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments":
                {"path": "notes#new-note", "args": {"title": title}}}}),
        )
        .await
        .unwrap();
    assert_ne!(answer["result"]["isError"], true, "{answer}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_computers_session_of_one_named_agent_is_that_one_agent() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let build_box = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    browser.create(NOTES);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    note_by(&hosted, &laptop, &host, "Claude Code", "From the laptop").await;
    note_by(
        &hosted,
        &build_box,
        &host,
        "Claude Code",
        "From the build box",
    )
    .await;
    note_by(&hosted, &build_box, &host, "Reviewer", "From the reviewer").await;

    let person = hex::encode(root.verifying_key().to_bytes());
    let user = root.verifying_key().to_bytes();
    let claude = tilo_actions::remote::device_agent_id(&user, "Claude Code");
    let reviewer = tilo_actions::remote::device_agent_id(&user, "Reviewer");
    assert_ne!(claude, reviewer, "another name is another agent");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        // Notes keeps each note in a part of its own, so who wrote them is read from every
        // document the page holds, not the notes alone.
        let by: Vec<_> = browser
            .documents
            .values()
            .flat_map(|changes| invokers(changes))
            .collect();
        let of = |agent: &str| {
            by.iter()
                .filter(|invoker| {
                    invoker.acting.agent_id() == Some(agent)
                        && invoker.acting.acted_for_person() == Some(person.as_str())
                })
                .collect::<Vec<_>>()
        };
        let (claude_wrote, reviewer_wrote) = (of(&claude), of(&reviewer));
        // Both computers' notes are the one agent's, named as the person named it.
        if claude_wrote.len() >= 2 && !reviewer_wrote.is_empty() {
            assert!(
                claude_wrote
                    .iter()
                    .all(|invoker| invoker.name.as_deref() == Some("Claude Code")),
                "{claude_wrote:?}"
            );
            assert!(
                reviewer_wrote
                    .iter()
                    .all(|invoker| invoker.name.as_deref() == Some("Reviewer")),
                "{reviewer_wrote:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the notes never arrived as one agent from two computers: {by:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
