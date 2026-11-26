//! A person's agent on computers of theirs (`tilo login`, then `tilo mcp`) is one agent wherever it
//! runs: every session that says it runs "Claude Code" acts as the one agent the person's root and
//! that name make ([`tilo_actions::remote::device_agent_id`]), named so in what it writes, and an
//! agent under another name is another agent.
use super::agent_mcp::tool;
use super::hosted_page_stream::SignedPage;
use super::*;
use tilo_mcp::McpServer;
use tilo_server_protocol::page::{PageAction, PageAnswer, PageAsk};
use tilo_session_actions::SessionActions;

const TASKS: &str = "so.tilo.tasks";

/// The live agent's activity must be beside their name in the same People row.
fn reviewer_is(html: &str, expected: Option<&str>) -> bool {
    let Some((_, people)) = html.split_once("class=\"tilo-people\"") else {
        return false;
    };
    let (people, _) = people.split_once("</section>").unwrap();
    let mut rows = people
        .split("<li ")
        .skip(1)
        .filter_map(|row| row.split_once("</li>").map(|(row, _)| row))
        .filter(|row| row.contains(">Reviewer</span>"));
    let reviewer = rows.next();
    assert!(rows.next().is_none(), "one agent appears twice: {people}");
    match expected {
        Some(activity) => reviewer.is_some_and(|row| {
            row.contains("data-present") && row.contains(&format!(">{activity}</span>"))
        }),
        None => reviewer
            .is_none_or(|row| !row.contains("data-present") && !row.contains("tilo-person-what")),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_signed_home_follows_an_agents_activity_and_departure() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home server", DeviceCapabilities::member());
    let device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "Reviewer on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let workspace = hosted
        .workspaces()
        .unwrap()
        .create("My workspace")
        .await
        .unwrap()
        .workspace;
    let mut person = Browser::open_at(&hosted, &device, &host, &workspace).await;
    person.create("so.tilo.word-processor");
    saved(&person.peer.barrier().unwrap()).unwrap();
    let open = || SignedPage::open(&hosted, &device, &host, &workspace, "", "");
    let mut home = open().await;
    home.until("the initial home before the agent connects", |html| {
        (html.contains("tilo-home-apps") && reviewer_is(html, None)).then_some(())
    })
    .await
    .unwrap();

    let computer = Browser::open_at(&hosted, &laptop, &host, &workspace).await;
    tilo_session_client::device::session_as_agent(&computer.peer, "Reviewer")
        .await
        .unwrap();
    let user = laptop.cert.user_public_key;
    let mcp = McpServer::for_agent(
        SessionActions::new(
            &computer.peer,
            computer.workspace,
            host.cert.device_public_key,
        ),
        workspace.clone(),
        &tilo_actions::remote::device_agent_id(&user, "Reviewer"),
        &hex::encode(user),
    )
    .with_agent_session("reviewer-home");
    let (reply, failed) = tool(&mcp, "tilo_ls", serde_json::json!({})).await;
    assert!(!failed, "{reply}");
    home.until("the open home shows the newly present agent", |html| {
        reviewer_is(html, Some("Looking at the apps")).then_some(())
    })
    .await
    .unwrap();
    open()
        .await
        .until("a newly opened home already shows that activity", |html| {
            (html.contains("tilo-home-apps") && reviewer_is(html, Some("Looking at the apps")))
                .then_some(())
        })
        .await
        .unwrap();

    let (reply, failed) = tool(
        &mcp,
        "tilo_do",
        serde_json::json!({
            "path": "word-processor#append-paragraph", "args": {"text": "Reviewed"}
        }),
    )
    .await;
    assert!(!failed, "{reply}");
    home.until("the same open home shows the agent's next action", |html| {
        reviewer_is(html, Some("Working in Word processor")).then_some(())
    })
    .await
    .unwrap();

    home.ask(&PageAsk::Act {
        id: 0,
        target: None,
        action: PageAction::Focus,
    })
    .await;
    tilo_test_env::within(
        "an unchanged home answers without replacing its People section",
        async {
            loop {
                match home.next_answer().await {
                    PageAnswer::Changed(patches) => assert!(
                        patches
                            .iter()
                            .all(|patch| !patch.html.contains("class=\"tilo-people\""))
                    ),
                    PageAnswer::Patched(patches) => {
                        assert!(patches.is_empty(), "{patches:?}");
                        break;
                    }
                    other => panic!("unexpected home response: {other:?}"),
                }
            }
        },
    )
    .await;

    drop(mcp);
    computer.peer.close();
    home.until("the departed agent's activity is removed", |html| {
        reviewer_is(html, None).then_some(())
    })
    .await
    .unwrap();
}

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

/// An issue `title` made by the agent `name` on the computer `computer`, in its own session.
async fn issue_by(hosted: &Hosted, computer: &Device, host: &Device, name: &str, title: &str) {
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
                {"path": "tasks#create", "args": {"title": title}}}}),
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
    browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();

    issue_by(&hosted, &laptop, &host, "Claude Code", "From the laptop").await;
    issue_by(
        &hosted,
        &build_box,
        &host,
        "Claude Code",
        "From the build box",
    )
    .await;
    issue_by(&hosted, &build_box, &host, "Reviewer", "From the reviewer").await;

    let person = hex::encode(root.verifying_key().to_bytes());
    let user = root.verifying_key().to_bytes();
    let claude = tilo_actions::remote::device_agent_id(&user, "Claude Code");
    let reviewer = tilo_actions::remote::device_agent_id(&user, "Reviewer");
    assert_ne!(claude, reviewer, "another name is another agent");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        // Tasks keeps its issues in parts of its own, so who wrote them is read from every
        // document the page holds, not the issues alone.
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
        // Both computers' issues are the one agent's, named as the person named it.
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
            "the issues never arrived as one agent from two computers: {by:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
