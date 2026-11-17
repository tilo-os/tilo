//! A conversation with an agent before anything was said in it: Chat lists it only once it
//! holds a message, and it takes that first message all the same.

use super::*;
use crate::testing::MountedChat;
use ui::framework::testing::TestApp;
use ui::tilo_resource::NonBlank;
use ui::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents, workspace_agents};

/// The workspace the window is in.
const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

/// Make an agent the person keeps, and open the conversation with it as search's agent hit
/// does: nothing has been said in it yet.
fn open_agent(app: &mut TestApp<ChatShell>) {
    app.frame();
    let agents = app
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .expect("the workspace's agents");
    agents
        .invoke(
            &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Assistant").expect("a name"),
                keeper: None,
                id: Some("assistant".to_owned()),
                model: None,
                mcp_client: None,
            }),
        )
        .expect("the agent is made");
    app.frame();
    app.call_on(
        "chat.workspace",
        "open-agent",
        serde_json::json!({ "agent": "assistant" }),
    )
    .expect("the conversation with the agent opens");
    app.settle();
}

#[test]
fn a_conversation_with_an_agent_takes_its_first_message_before_it_is_listed() {
    ui::i18n::I18nManager::init("en-US");
    let mut window = TestApp::<ChatShell>::in_workspace(WORKSPACE).expect("a window");
    open_agent(&mut window);
    let conversation = window.chat().active_channel.clone();
    let members =
        crate::chat::Chat::direct_members(&ui::tilo_resource::Id::from_raw(conversation.clone()))
            .expect("the window shows a direct conversation");
    assert!(
        members.contains(&ui::tilo_resource::MemberId::agent("assistant".to_owned())),
        "the conversation is the one with the agent: {members:?}"
    );
    assert!(
        window.chat().channel(&conversation).is_none(),
        "nothing was said yet, so no row lists the conversation"
    );
    assert!(
        window.chat().takes_messages(),
        "a conversation with an agent takes its first message"
    );

    // What the person types in the field goes to that conversation in Chat's own record, by
    // the field's own Enter.
    let typed = "Remind me in 30 minutes";
    window.harness().type_text(typed);
    window.settle();
    let held = window.chat().composer_main.value();
    assert_eq!(held, typed, "the field holds what was typed");
    window.harness().key("Enter");
    window.settle();
    let rows = crate::testing::mounted_rows(&mut window, &conversation);
    assert_eq!(
        rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
        vec![typed],
        "the typed message is the conversation's first"
    );
    assert_eq!(
        window.chat().composer_main.value(),
        "",
        "the field is ready for the next message"
    );
    assert!(
        window.chat().channel(&conversation).is_some(),
        "the conversation is listed once it holds a message"
    );
}
