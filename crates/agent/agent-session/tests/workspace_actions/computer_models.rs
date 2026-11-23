//! The model a person chose in their computer's Agent models pane is the one their workspace's
//! agents answer on there: a model the computer runs itself, linked into the workspace's own model
//! configuration, once for a choice made before and again each time the pane changes it. With
//! nothing linked, the agent says so in Chat, in the person's own language, and where to choose one.
use super::hosted_agent_turns::{SCOUT, conversation_part, make_agent, write_to_agent};
use super::*;
use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
};
use tilo_server::workspaces::person_model::scripted::{Reply, ScriptedProvider};
use tilo_server::workspaces::this_computer::Linking;
use ui::tilo_resource::Id;

const CHAT: &str = "so.tilo.chat";

/// A model server on this computer, as its Agent models pane keeps it: Ollama on loopback, asked
/// without a key, offering `models`, of which agents answer with `chosen`.
fn ollama(models: &[&str], chosen: &str) -> ProviderConfig {
    let profile = ProviderProfile {
        id: "ollama".to_owned(),
        label: "Ollama".to_owned(),
        protocol: ProviderProtocol::OpenAiCompatible,
        endpoint: "http://127.0.0.1:11434/v1".to_owned(),
        location: EndpointLocation::ThisDevice,
        models: models
            .iter()
            .map(|model| ModelDescriptor {
                tool_calling: true,
                ..ModelDescriptor::new(*model)
            })
            .collect(),
        default_model: Some(chosen.to_owned()),
        ..ProviderProfile::local_vllm()
    };
    ProviderConfig {
        default_profile: Some(profile.id.clone()),
        profiles: vec![profile],
        ..ProviderConfig::default()
    }
}

/// A workspace on a person's computer with their agent Scout in it and its Chat: the server, its
/// workspaces, the workspace, and who writes from where.
async fn a_workspace_with_scout() -> (
    crate::harness::hosted::Hosted,
    tilo_server::workspaces::Workspaces,
    String,
    (SigningKey, crate::support::Device),
    tempfile::TempDir,
) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    make_agent(
        &page,
        &workspaces,
        &workspace,
        &hex::encode(root.verifying_key().to_bytes()),
        None,
    )
    .await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    // A person's own computer calls the models on itself and on its network.
    workspaces.call_private_model_addresses(true);
    (server, workspaces, workspace, (root, browser), state)
}

/// What Scout replied to `message`, read back from the conversation.
async fn reply_to(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    message: &Id,
) -> chat::chat::Message {
    let part = conversation_part(workspaces, workspace).await;
    part.messages
        .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)))
        .cloned()
        .expect("Scout replied in the conversation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_model_chosen_in_a_computers_agent_models_pane_answers_its_agents() {
    let (server, workspaces, workspace, (root, browser), _state) = a_workspace_with_scout().await;
    let actions = server.roster.actions.clone();
    let theirs = ScriptedProvider::answering([
        Reply::text("Llama here: the plan is ready."),
        Reply::text("Qwen here: Ada owns it."),
    ]);
    workspaces.answer_models_with(theirs.clone());

    // Before agents answered on the workspace's own models, Ada chose a model Ollama runs on her
    // computer in its Agent models pane. That choice is linked into the workspace once.
    let chose = ollama(&["llama3.2", "qwen3"], "llama3.2");
    assert!(
        workspaces
            .link_this_computer(&workspace, &chose, Linking::Once)
            .unwrap(),
        "the computer's choice becomes the workspace's"
    );
    assert!(
        !workspaces
            .link_this_computer(&workspace, &chose, Linking::Once)
            .unwrap(),
        "once: a workspace it was linked into is not linked again"
    );
    let models = workspaces.workspace_models(&workspace);
    assert_eq!(models.default_profile.as_deref(), Some("ollama"));
    assert!(
        models
            .profiles
            .iter()
            .all(|profile| profile.credential.is_none()),
        "no key comes with it"
    );

    let agents = server.approving(&workspace, &[SCOUT]).await;
    let written = (&root, &browser);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "what is planned?",
    )
    .await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    let reply = reply_to(&workspaces, &workspace, &first).await;
    assert!(reply.text.contains("the plan is ready"), "{reply:?}");
    let asked = theirs.asked();
    assert_eq!(asked.len(), 1, "one answer, asked of the computer's model");
    assert_eq!(asked[0].model, "llama3.2");
    assert_eq!(
        asked[0].key, None,
        "a model on the computer is asked without a key"
    );

    // She picks another model in the pane: the workspace, still on her computer's choice, follows.
    let picked = ollama(&["llama3.2", "qwen3"], "qwen3");
    assert!(
        workspaces
            .link_this_computer(&workspace, &picked, Linking::Following)
            .unwrap()
    );
    let second = write_to_agent(&actions, &workspaces, &workspace, written, "who owns it?").await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    let reply = reply_to(&workspaces, &workspace, &second).await;
    assert!(reply.text.contains("Ada owns it"), "{reply:?}");
    assert_eq!(
        theirs.asked()[1].model,
        "qwen3",
        "the model picked in the pane answers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_model_linked_the_agent_says_so_in_chat_in_its_persons_language() {
    let (server, workspaces, workspace, (root, browser), _state) = a_workspace_with_scout().await;
    let actions = server.roster.actions.clone();
    let theirs = ScriptedProvider::answering([Reply::text("never asked")]);
    workspaces.answer_models_with(theirs.clone());

    // Ada reads her computer in German, and neither she nor her computer linked a model.
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .speaking(|| Some("de-DE".to_owned()));
    let written = (&root, &browser);
    let asked = write_to_agent(&actions, &workspaces, &workspace, written, "Was steht an?").await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    let reply = reply_to(&workspaces, &workspace, &asked).await;
    let said = format!("{} {:?}", reply.text, reply.work);
    assert!(
        said.contains(
            "Für diese Art von Arbeit ist kein Modell verknüpft. Wähle eines in den Einstellungen \
             unter Agentenmodelle."
        ),
        "the refusal reads in German and names the pane: {said}"
    );
    assert!(
        !chat::summons::unanswered(Some(&reply)),
        "the turn ended: {:?}",
        reply.work
    );
    assert!(theirs.asked().is_empty(), "no provider is asked");
}
