//! What a person's assistant does on its own on a server, through a turn the server carries out
//! with no device of the person on: the model of that turn finds the server's own services among
//! the tools its turn has, takes their actions, and is answered what each answered.

use super::HostAgents as ServerAgents;
use super::*;
use crate::harness::hosted::serving_records_with_actions as serving_with_actions;
use tilo_server::{
    workspace_bots::{initiative::Initiative, workspace_resource},
    workspaces::Workspaces,
};

/// The workspace as a person has it: their agent made to work on the server, the chat with them
/// in it, and the tracker. The document the tracker's first issues are kept in.
async fn furnished(
    page: &Browser,
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
) -> String {
    make_agent(page, workspaces, workspace, person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    let tracker_document = workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(workspace, &governor, &catalogue, TASKS, &tracker_document)
        .await
        .unwrap();
    tasks::part::chain().part(0).as_str().to_owned()
}

/// What the server's own services answer reaches the model of a turn. They answer an action with
/// its receipt alone, while the tools a turn has read how an invocation ended by following it,
/// so the server keeps the one for the other: the assistant turns its person's look-round on and
/// then reads their events, both through `tilo_do`, and is told what each answered, not that it
/// is still pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_servers_own_services_answer_reaches_the_model_of_a_turn() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = serving_with_actions(
        &root,
        &server_device,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        state.path().join("initiative"),
        state.path().join("events"),
        Some(actions.clone()),
    );

    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Look round for me every hour.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = ServerAgents::new(
        workspaces.clone(),
        actions.clone(),
        Some(does_then_answers(
            vec![
                serde_json::json!({
                    "path": "tilo.initiative/look-round#turn-on",
                    "args": {"when": "every 3600 seconds"},
                }),
                serde_json::json!({"path": "tilo.events/events#list"}),
            ],
            "I look round every hour now.",
            Arc::clone(&asked),
        )),
    )
    .with_events(Some(events))
    .with_initiative(Some(initiative));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the turn never ended; the model was asked {:?} times",
            asked.lock().unwrap().len()
        )
    })
    .unwrap();

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3, "two tool calls, then what each answered");
    // The look-round's own action: it is on, at the rhythm asked for.
    let (turned_on, failed) = answered_to(&asked[1], "call-1");
    assert!(
        !failed && turned_on.starts_with("ok") && turned_on.contains("every 3600 seconds"),
        "what turning it on answered the model: {turned_on}"
    );
    // The events' own action, taken after: the look-round is one of its person's events now.
    let (listed, failed) = answered_to(&asked[2], "call-2");
    assert!(
        !failed && listed.starts_with("ok") && listed.contains("Look-round"),
        "what the person's events answered the model: {listed}"
    );
}
