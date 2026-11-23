//! A turn of an agent on the workspace's server is an agent session, as a connected agent's is:
//! the people's pages read it as one, with what it was asked, whom it acted for, and the changes
//! it made, grouped as that session's work.
use super::hosted_agent_turns::{SCOUT, TASKS, file_issue, looks_then_answers, make_agent};
use super::*;

/// What the page's History reads of the agent's changes in `route`: each one's work group and
/// purpose.
fn agent_work(
    page: &mut Browser,
    route: &DocumentRoute,
) -> Vec<(Option<tilo_actions::WorkGroup>, Option<String>)> {
    page.pump();
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in page.documents.get(route).into_iter().flatten() {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    let mut entries = tilo_actions::EntryReader::default();
    doc.get_changes(&[])
        .iter()
        .filter_map(|change| entries.read(change.actor_id(), change.message()))
        .filter(|context| {
            context
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.acting.is_agent())
        })
        .map(|context| (context.group, context.purpose))
        .collect()
}

/// A person gives the server's agent an issue: the page that follows the issues reads every change
/// the agent made as one session of its, named by the agent, its purpose what the person asked,
/// as Console lists a connected agent's session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_on_the_server_is_an_agent_session_the_page_reads() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let acts_for = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &acts_for,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut page = Browser::open_at(&server, &browser, &acts_for, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let tracker_part = file_issue(
        &actions,
        &workspaces,
        &workspace,
        &root,
        (&acts_for, &browser),
    )
    .await;
    page.follow_chain(TASKS, tasks::issue_list::chain().chain_head().as_str());

    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::default()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tilo_test_env::within("the turn ends", agents.answer(&workspace, &turns[0]))
        .await
        .unwrap();
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker_part,
    };
    let deadline = tilo_test_env::hang_deadline();
    let work = loop {
        let work = agent_work(&mut page, &route);
        if !work.is_empty() {
            break work;
        }
        assert!(
            Instant::now() < deadline,
            "the agent's changes never reached the page: heard {}",
            page.heard(&route)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (Some(session), _) = &work[0] else {
        panic!("the agent's change names no session: {work:?}");
    };
    assert!(
        session.kind == "agent-session" && session.id.starts_with("agent-session:"),
        "the turn is the agent's session: {session:?}"
    );
    assert!(
        work.iter()
            .all(|(group, purpose)| group.as_ref() == Some(session)
                && purpose
                    .as_deref()
                    .is_some_and(|said| said.contains("Plan the launch"))),
        "every change of the turn is its one session's, for what the person asked: {work:?}"
    );
}
