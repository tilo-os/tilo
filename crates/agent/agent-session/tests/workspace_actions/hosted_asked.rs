//! A person mentions the workspace's hosted agent in a Chat message, and the agent answers under
//! the message: the mention is an ask of the agent package, the server claims its turn, works
//! it with its model and writes the reply through the package's own actions, as the agent. No
//! device of the person's has to be on, and nothing of Chat's own fields is read.
use super::*;
use std::sync::Mutex;
use tilo_server::workspace_agents::{Ask, ServerAgents};
use tilo_workspace_service::asks::{self, AskState};
use tilo_workspace_service::tl_records::runtime::Value;

/// What the agent package holds of the asks in Chat's record now, as the person's devices read
/// it.
async fn asked(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
) -> Vec<asks::Asked> {
    let packages = asks::chat_packages().unwrap();
    workspaces
        .with_tl_record(
            workspace,
            &packages,
            asks::Actor::Member(MemberId::person(person.to_owned())),
            |opened| asks::asks(opened),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_agent_answers_the_message_that_mentions_it() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &host,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    // Ada opens a channel in Chat and writes "@Scout what is planned?" there. Her device's host
    // makes the ask from the mention as it takes her message in, as hers.
    let packages = asks::chat_packages().unwrap();
    let ada = asks::Actor::Member(MemberId::person(person.clone()));
    let chat = packages.app().to_owned();
    workspaces
        .with_tl_record(&workspace, &packages, ada.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let channel = opened.invoke(
                &ada,
                (&chat, "new_channel"),
                vec![Value::resource(&all), Value::Text("plans".into())],
                None,
            )?;
            opened.invoke(
                &ada,
                (&chat, "send"),
                vec![
                    channel,
                    asks::mentioning(&MemberId::agent(SCOUT), "Scout", " what is planned?"),
                    Value::Opt(None),
                ],
                None,
            )
            .map(drop)
        })
        .await
        .unwrap();
    let waiting = asked(&workspaces, &workspace, &person).await;
    assert_eq!(waiting.len(), 1, "the mention asked once: {waiting:?}");
    assert_eq!(waiting[0].agent, MemberId::agent(SCOUT));
    assert_eq!(waiting[0].state, AskState::Waiting);

    // The server reads the ask as a turn of the agent's, for Ada, with what she wrote.
    let model_asked = Arc::new(Mutex::new(Vec::new()));
    let agents = ServerAgents::for_test(
        workspaces.clone(),
        actions.clone(),
        Some(looks_then_answers(1_000, Arc::clone(&model_asked))),
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the ask is the agent's one turn: {turns:?}");
    assert!(
        matches!(turns[0].ask, Ask::Asked { .. }),
        "{:?}",
        turns[0].ask
    );
    assert_eq!(turns[0].person, person, "it works for whoever asked");
    assert!(
        turns[0].request.contains("what is planned?"),
        "the turn asks what the person wrote: {}",
        turns[0].request
    );
    tokio::time::timeout(Duration::from_secs(30), agents.answer(&workspace, &turns[0]))
        .await
        .expect("the turn ends")
        .unwrap();

    // The ask is answered: its reply is the agent's, in the model's words, the step it took on
    // the way recorded, and the server's claim is the one its turn was taken under.
    let answered = asked(&workspaces, &workspace, &person).await;
    assert_eq!(answered.len(), 1, "{answered:?}");
    let ask = &answered[0];
    assert_eq!(ask.state, AskState::Done, "{ask:?}");
    let device = hex::encode(workspaces.governor().device_public_key);
    assert!(
        ask.claims.iter().any(|claim| claim.device == device),
        "the server claimed the turn: {:?}",
        ask.claims
    );
    let reply = ask.latest().expect("the ask has a reply");
    assert_eq!(reply.attempt, 1);
    assert_eq!(reply.state, AskState::Done);
    assert!(reply.body.contains("the plan is ready"), "{}", reply.body);
    assert!(!reply.superseded);
    assert_eq!(
        model_asked.lock().unwrap().len(),
        2,
        "one model call to look, one to answer"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "an answered ask is owed nothing"
    );
    page.peer.close();
}
