#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use assistant::App;
use serde_json::json;
use ui::prelude::*;
use ui::tilo_actions::remote::{Caller, InvokeArgument, TestHost};
use ui::tilo_reference::ResourceRef;
use ui::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents};

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
fn chat_api() -> TestHost {
    TestHost::new().completing(|_| ui::tilo_actions::ActionOutput {
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
    let hosted = app
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .expect("the workspace's agents")
        .get()
        .active()
        .all(|agent| agent.on_host);
    assert!(
        hosted,
        "the workspace's host runs it, so it answers while every device is off"
    );
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
    let asked = chat.asked();
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
