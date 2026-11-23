#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::sync::Arc;

use assistant::App;
use serde_json::json;
use ui::prelude::*;
use ui::tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, TestProvider};
use ui::tilo_reference::ResourceRef;
use ui::tilo_resource::NonBlank;
use ui::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents, workspace_agents};

/// The workspace the window is in.
const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

fn english() {
    ui::i18n::I18nManager::init("en-US");
}

fn shows(app: &mut TestApp<App>, target: &str) -> bool {
    app.harness().targets().contains_key(target)
}

/// Whether the window says `text`, as a name, value or description a screen reader reads.
fn says(app: &mut TestApp<App>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    })
}

/// Chat as the session's API reaches it: it takes each message it is sent and answers
/// nothing, as `send` does for a caller that opens no window.
fn chat_api() -> TestProvider {
    TestProvider::new().completing(|_| ui::tilo_actions::ActionOutput {
        value: None,
        resources: Vec::new(),
    })
}

/// The agents working in the window's workspace, as its own document of them has them.
fn agents(app: &mut TestApp<App>) -> Vec<(String, String, Option<String>)> {
    let record = app
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .expect("the workspace's agents");
    record
        .get()
        .active()
        .map(|agent| {
            (
                agent.id.0.clone(),
                agent.name.clone(),
                agent.mcp_client.clone(),
            )
        })
        .collect()
}

#[test]
fn every_scenario_shows_its_place_built_from_components() {
    english();
    let cases: [(&str, &[&str]); 7] = [
        ("empty", &["assistant-create", "assistant-supervision"]),
        (
            "main-chat",
            &[
                "assistant-open-in-chat",
                "assistant-start-sub-agent",
                "assistant-panel-tabs",
                "assistant-task-reply",
                "assistant-task-find-deck",
                "assistant-task-usage",
                "assistant-task-churn",
            ],
        ),
        (
            "sub-agent",
            &[
                "assistant-sub-agent-role",
                "assistant-sub-agent-conversation",
            ],
        ),
        (
            "job",
            &[
                "assistant-job",
                "assistant-job-start",
                "assistant-job-cancel",
            ],
        ),
        (
            "scheduled",
            &[
                "assistant-scheduled-call",
                "assistant-scheduled-rundown",
                "assistant-event-posts",
                "assistant-event-dates",
            ],
        ),
        ("resolved", &["assistant-task-invoice"]),
        (
            "supervision",
            &["assistant-state", "assistant-supervision-facts"],
        ),
    ];
    for (scenario, targets) in cases {
        let mut app = TestApp::<App>::capture(Some(scenario)).expect("capture");
        for target in targets {
            assert!(shows(&mut app, target), "{scenario} shows {target}");
        }
        // The places stand in every scenario, and facts are parts of a row, never joined with
        // a glyph.
        for place in ["Main chat", "Sub-agents", "Supervision"] {
            assert!(says(&mut app, place), "{scenario} lists {place}");
        }
        assert!(!says(&mut app, "·"), "{scenario} joins facts with a dot");
    }
}

#[test]
fn a_task_is_listed_where_it_stands_and_never_under_another_stage() {
    english();
    let mut app = TestApp::<App>::capture(Some("main-chat")).expect("capture");
    // What comes next stands with the tasks that are open, and what is resolved is its own
    // view of the panel.
    assert!(shows(&mut app, "assistant-task-churn"));
    assert!(!shows(&mut app, "assistant-task-invoice"));
    let mut resolved = TestApp::<App>::capture(Some("resolved")).expect("capture");
    assert!(!shows(&mut resolved, "assistant-task-reply"));
}

#[test]
fn a_window_someone_uses_says_what_no_resource_answers_instead_of_none() {
    english();
    let mut app = TestApp::<App>::in_workspace(WORKSPACE).expect("the window");
    app.frame();
    // Nothing publishes the assistant's tasks here, so the panel does not claim there are
    // none.
    assert!(shows(&mut app, "assistant-tasks-not-here"));
    assert!(!shows(&mut app, "assistant-tasks-none-active"));
}

#[test]
fn creating_the_assistant_makes_an_agent_of_the_workspace_the_person_keeps() {
    english();
    let mut app = TestApp::<App>::in_workspace(WORKSPACE).expect("the window");
    app.frame();
    assert!(agents(&mut app).is_empty(), "a new workspace has no agent");
    assert!(shows(&mut app, "assistant-create"));
    assert!(
        !shows(&mut app, "assistant-start-sub-agent"),
        "nobody to hand a job to yet"
    );

    app.harness().click_named("assistant-create").unwrap();
    app.frame();
    let made = agents(&mut app);
    assert_eq!(
        made.len(),
        1,
        "one agent, in Chat's resource of them: {made:?}"
    );
    assert_eq!(made[0].0, "assistant", "the id its actor works under");
    assert_eq!(made[0].1, "Assistant");
    assert_eq!(made[0].2, None, "no client of its own connects as it");
    // The window shows it at once: the conversation is Chat's, and there is someone to hand
    // a job to.
    assert!(!shows(&mut app, "assistant-create"));
    assert!(shows(&mut app, "assistant-open-in-chat"));
    assert!(says(&mut app, "The conversation with Assistant is in Chat"));
    assert!(shows(&mut app, "assistant-start-sub-agent"));
}

#[test]
fn a_viewer_makes_no_assistant() {
    english();
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the window");
    app.frame();
    assert!(shows(&mut app, "assistant-create"));
    // The press is the workspace's to refuse: no agent is made, and the step still stands.
    let _ = app.harness().click_named("assistant-create");
    app.frame();
    assert!(
        agents(&mut app).is_empty(),
        "the workspace's grants refused"
    );
    assert!(shows(&mut app, "assistant-create"), "the step still stands");
    assert!(!shows(&mut app, "assistant-start-sub-agent"));
}

#[test]
fn a_job_for_a_sub_agent_goes_to_the_main_agent_through_chats_own_send() {
    english();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_ID", WORKSPACE);
    let chat = Arc::new(chat_api());
    let _served = ui::RemoteActions::enter(chat.clone(), Caller::local_person("ada"));
    let mut app = TestApp::<App>::in_workspace(WORKSPACE).expect("the window");
    app.frame();
    app.harness().click_named("assistant-create").unwrap();
    app.frame();
    let assistant = agents(&mut app).remove(0).0;

    app.harness()
        .click_named("assistant-start-sub-agent")
        .unwrap();
    app.frame();
    // With no job said, nothing is sent and the window says what is missing.
    app.harness().click_named("assistant-job-start").unwrap();
    app.settle();
    assert!(chat.asked().is_empty(), "an empty job sends nothing");
    assert!(says(&mut app, "Say what the job is first"));

    app.harness().click_named("assistant-job").unwrap();
    app.harness().type_text("Find out why sign-ups dropped");
    app.frame();
    app.harness().click_named("assistant-job-start").unwrap();
    app.settle();
    let asked = sent(&chat);
    assert_eq!(asked.len(), 1, "one message: {asked:?}");
    // Chat's own `send`, in this workspace: the conversation is Chat's to open and post in.
    let conversations = ResourceRef::whole("so.tilo.chat", "chat.workspace", "Chat");
    assert_eq!(
        asked[0].target,
        conversations.action_target(WORKSPACE, "so.tilo.chat", None, "send")
    );
    assert_eq!(
        asked[0].argument,
        InvokeArgument::Json(
            json!({
                "to": [{ "kind": "agent", "id": assistant, "name": "Assistant" }],
                "attached": [],
                "text": "Start a sub-agent for this job: Find out why sign-ups dropped",
                "opens": false,
            })
            .as_object()
            .unwrap()
            .clone()
        ),
        "to the main agent, the job in the person's words, and nothing opens over the window"
    );
    // The job is handed over: the window is back on the main chat, where the message is.
    app.frame();
    assert!(shows(&mut app, "assistant-open-in-chat"));
    assert!(!shows(&mut app, "assistant-job"));
}

/// The provider that answers the agent's transforms, as `assistant`'s own composer asks it.
const TRANSFORMS: &str = "tilo.agent.transforms";

/// The provider that answers the agent's transforms as `decided`, and Chat's `send` otherwise.
fn deciding(decided: serde_json::Value) -> Arc<TestProvider> {
    Arc::new(TestProvider::new().completing(move |request| {
        if request.target.provider == TRANSFORMS {
            ui::tilo_actions::ActionOutput {
                value: Some(decided.clone()),
                resources: Vec::new(),
            }
        } else {
            ui::tilo_actions::ActionOutput::default()
        }
    }))
}

/// A window with its assistant and the composer open, and `api` answering its steps.
fn asking(
    api: Arc<TestProvider>,
) -> (
    TestApp<App>,
    tilo_test_env::ScopedEnv,
    ui::RemoteActionsGuard,
) {
    english();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_ID", WORKSPACE);
    let served = ui::RemoteActions::enter(api, Caller::local_person("ada"));
    let mut app = TestApp::<App>::in_workspace(WORKSPACE).expect("the window");
    app.frame();
    app.harness().click_named("assistant-create").unwrap();
    app.frame();
    app.harness()
        .click_named("assistant-start-sub-agent")
        .unwrap();
    app.frame();
    (app, env, served)
}

/// The requests `api` was asked, other than the transform that decides the request.
fn sent(api: &TestProvider) -> Vec<InvokeRequest> {
    api.asked()
        .into_iter()
        .filter(|request| request.target.provider != TRANSFORMS)
        .collect()
}

/// A request the agent can answer is answered in the composer, and no sub-agent starts.
#[test]
fn a_request_the_agent_answers_is_answered_in_the_composer() {
    let api = deciding(json!({"answer": "It is 3 pm where they are."}));
    let (mut app, _env, _served) = asking(Arc::clone(&api));

    app.harness().click_named("assistant-job").unwrap();
    app.harness().type_text("What time is it where they are?");
    app.frame();
    app.harness().click_named("assistant-job-start").unwrap();
    app.settle();

    assert!(
        says(&mut app, "It is 3 pm where they are."),
        "the agent's answer stands in the composer"
    );
    assert!(
        shows(&mut app, "assistant-job"),
        "the composer stays for the person to ask again"
    );
    // It was asked the request through the host's own transforms, with the person's words.
    let asked = api.asked();
    let decided = asked
        .iter()
        .find(|request| request.target.provider == TRANSFORMS)
        .expect("the agent is asked what the request is");
    assert_eq!(decided.target.address.action, "respond");
    assert_eq!(
        decided.argument,
        InvokeArgument::Json(
            json!({"request": "What time is it where they are?"})
                .as_object()
                .unwrap()
                .clone()
        )
    );
    assert!(
        sent(&api).is_empty(),
        "an answered question starts no sub-agent"
    );
}

/// A request the agent names work for starts a sub-agent, as it did before the ask.
#[test]
fn a_request_the_agent_names_work_for_starts_a_sub_agent() {
    let api = deciding(json!({"work": "Find out why sign-ups dropped"}));
    let (mut app, _env, _served) = asking(Arc::clone(&api));

    app.harness().click_named("assistant-job").unwrap();
    app.harness().type_text("Why did sign-ups drop?");
    app.frame();
    app.harness().click_named("assistant-job-start").unwrap();
    app.settle();

    let sent = sent(&api);
    assert_eq!(sent.len(), 1, "the job goes to the main agent: {sent:?}");
    assert!(
        sent[0]
            .argument
            .to_slot()
            .is_some_and(|argument| argument.contains("Why did sign-ups drop?")),
        "the sub-agent gets the person's own words"
    );
    app.frame();
    assert!(
        shows(&mut app, "assistant-open-in-chat"),
        "the main chat is where the work stands"
    );
    assert!(!shows(&mut app, "assistant-job"), "the composer closes");
}

/// The workspace's document of its agents, by its id.
fn agents_document() -> DocumentId {
    DocumentId::parse(&ui::resource::workspace::document_id(workspace_agent::KIND))
        .expect("the id of the workspace's agents")
}

/// The window while the workspace's agents are on their way: it shows them neither as not
/// here nor as none, offers no first assistant, and has no place to go yet.
fn on_their_way(app: &mut TestApp<App>) {
    assert!(
        !shows(app, "assistant-agents-not-here"),
        "not taken for missing"
    );
    assert!(!shows(app, "assistant-create"), "nor for none");
    assert!(!shows(app, "assistant-open-in-chat"));
    assert!(!shows(app, "assistant-supervision"));
}

#[test]
fn a_window_opened_before_the_agents_arrive_shows_them_when_they_do() {
    english();
    let mut app = TestApp::<App>::awaiting_the_workspace(WORKSPACE, workspace_agent::OWNER)
        .expect("the window");
    app.frame();
    // On their way, the agents are neither taken for missing nor for none: the window waits
    // for them, and nothing offers to make an assistant the workspace may already hold.
    on_their_way(&mut app);
    assert!(
        app.wanted_resources().contains(&agents_document()),
        "the workspace is asked for them"
    );

    // The workspace delivers its agents, the person's assistant among them.
    app.workspace_delivers::<WorkspaceAgents>(workspace_agent::OWNER, |agents| {
        agents
            .invoke(
                &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    name: NonBlank::parse("Assistant").expect("a name"),
                    id: Some("assistant".to_owned()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())
    })
    .expect("delivered");
    for _ in 0..4 {
        app.frame();
    }
    // The window shows them with nobody opening it again: the conversation is Chat's, and
    // there is someone to hand a job to.
    assert!(!shows(&mut app, "assistant-agents-not-here"));
    assert!(!shows(&mut app, "assistant-create"));
    assert!(shows(&mut app, "assistant-open-in-chat"));
    assert!(says(&mut app, "The conversation with Assistant is in Chat"));
    assert!(shows(&mut app, "assistant-start-sub-agent"));
    assert_eq!(agents(&mut app).len(), 1, "the one the workspace holds");
}

#[test]
fn a_window_opened_before_the_workspace_answered_offers_the_first_assistant_once_it_holds_none() {
    english();
    let mut app = TestApp::<App>::awaiting_the_workspace(WORKSPACE, workspace_agent::OWNER)
        .expect("the window");
    app.frame();
    on_their_way(&mut app);

    // The workspace holds no agents yet: their document is new here, and the one step that
    // makes the first assistant stands where the sentence was.
    app.workspace_answered(agents_document(), WorkspaceAnswer::HoldsNone);
    for _ in 0..4 {
        app.frame();
    }
    assert!(!shows(&mut app, "assistant-agents-not-here"));
    assert!(shows(&mut app, "assistant-create"));
    app.harness().click_named("assistant-create").unwrap();
    app.frame();
    assert_eq!(
        agents(&mut app).len(),
        1,
        "the first agent of the workspace"
    );
    assert!(shows(&mut app, "assistant-open-in-chat"));
}

#[test]
fn a_person_without_a_name_reads_as_you_at_the_foot_never_as_their_key() {
    english();
    let mut app = TestApp::<App>::in_workspace(WORKSPACE).expect("the window");
    app.frame();
    assert!(shows(&mut app, "assistant-person"));
    assert!(
        says(&mut app, "You"),
        "the window's own person reads as You"
    );
    assert!(
        !says(&mut app, "test-author"),
        "a person nobody named never reads as their key"
    );
}
