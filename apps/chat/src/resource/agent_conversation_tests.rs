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

/// A conversation with an agent is named by what the workspace's own list of its agents calls
/// it: the window and the direct messages list read "Assistant", never a placeholder cut from its
/// id, also while the agent works on the workspace's server and no presence here names it.
#[test]
fn a_conversation_with_an_agent_is_named_by_the_workspaces_own_list() {
    ui::i18n::I18nManager::init("en-US");
    let mut window = TestApp::<ChatShell>::in_workspace(WORKSPACE).expect("a window");
    open_agent(&mut window);
    window.frame();
    let agent = ui::tilo_resource::MemberId::agent("assistant".to_owned());
    assert_eq!(window.chat().display_name(&agent), "Assistant");
    // The conversation is listed once it holds a message; that row is what the
    // window's title and the direct messages list read.
    window.harness().type_text("Remind me in 30 minutes");
    window.settle();
    window.harness().key("Enter");
    window.settle();
    let conversation = window.chat().active_channel.clone();
    let row = window
        .chat()
        .direct_contacts()
        .into_iter()
        .find(|held| held.id == conversation)
        .expect("the open conversation is listed");
    assert_eq!(row.name, "Assistant", "the direct messages list names it");
}

/// A window opened at an agent of the workspace, as the Assistant window opens Chat beside it,
/// shows the conversation with that agent: never the first channel, where what the person types
/// would go to everyone.
#[test]
fn a_window_opened_at_an_agent_shows_the_conversation_with_it() {
    ui::i18n::I18nManager::init("en-US");
    let mut window = TestApp::<ChatShell>::in_workspace(WORKSPACE).expect("a window");
    window.frame();
    let agents = window
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .expect("the workspace's agents");
    agents
        .invoke(
            &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Assistant").expect("a name"),
                id: Some("assistant".to_owned()),
                model: None,
                mcp_client: None,
            }),
        )
        .expect("the agent is made");
    window.frame();
    let at = ui::tilo_resource::items::item_resource_id::<WorkspaceAgents>(
        "agents",
        &ui::tilo_resource::Id::from_raw("assistant"),
    );
    let mut open = window.chat().opener();
    open(&at);
    window.settle();
    let conversation = window.chat().active_channel.clone();
    let members =
        crate::chat::Chat::direct_members(&ui::tilo_resource::Id::from_raw(conversation.clone()))
            .unwrap_or_else(|| panic!("a direct conversation is open, not {conversation}"));
    assert!(
        members.contains(&ui::tilo_resource::MemberId::agent("assistant".to_owned())),
        "the conversation is the one with the agent: {members:?}"
    );
    assert!(window.chat().takes_messages(), "it takes the first message");
}

/// A person's own agent is listed among their direct messages before anything was said to it,
/// at the conversation with it: the place a person talks to their assistant from, in Chat itself.
#[test]
fn a_persons_own_agent_is_listed_before_its_first_message() {
    ui::i18n::I18nManager::init("en-US");
    let mut window = TestApp::<ChatShell>::in_workspace(WORKSPACE).expect("a window");
    window.frame();
    let agents = window
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .expect("the workspace's agents");
    agents
        .invoke(
            &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Assistant").expect("a name"),
                id: Some("assistant".to_owned()),
                model: None,
                mcp_client: None,
            }),
        )
        .expect("the agent is made");
    agents
        .invoke(
            &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Shared researcher").expect("a name"),
                id: Some("shared-researcher".to_owned()),
                model: None,
                mcp_client: None,
            }),
        )
        .expect("another workspace agent");
    window.settle();
    let listed = |window: &mut TestApp<ChatShell>| -> Vec<(String, String)> {
        window
            .chat()
            .sidebar_sections()
            .last()
            .map(|section| {
                section
                    .items
                    .iter()
                    .map(|item| (item.label.clone(), item.id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let me = window.chat().me.id.clone();
    let with = crate::chat::Chat::direct_id([
        me,
        ui::tilo_resource::MemberId::agent("assistant".to_owned()),
    ]);
    let place = crate::sidebar::channel_place(with.as_str());
    assert_eq!(
        listed(&mut window),
        [("Assistant".to_owned(), place.clone())],
        "the direct messages list the agent, at the conversation with it"
    );

    // Opened, it is listed once.
    let mut open = window.chat().opener();
    open(
        &ui::tilo_resource::items::item_resource_id::<WorkspaceAgents>(
            "agents",
            &ui::tilo_resource::Id::from_raw("assistant"),
        ),
    );
    window.settle();
    assert_eq!(window.chat().active_channel, with.as_str());
    assert_eq!(listed(&mut window), [("Assistant".to_owned(), place)]);
    assert_eq!(
        window.chat().empty_channel_message(),
        Some(crate::i18n::t("chat-assistant-look-round")),
        "the personal DM explains how to ask for scheduled workspace checks"
    );
    assert_eq!(
        window.chat().conversation_placeholder(with.as_str()),
        crate::i18n::t("chat-assistant-composer-placeholder")
    );

    // Other agents remain addressable when a person explicitly chooses one.
    window
        .call_on(
            "chat.workspace",
            "open-agent",
            serde_json::json!({"agent": "shared-researcher"}),
        )
        .expect("open the chosen agent");
    window.settle();
    assert_eq!(
        window.chat().conversation_agent().as_deref(),
        Some("shared-researcher")
    );
    let chosen = crate::sidebar::channel_place(&window.chat().active_channel);
    assert!(
        listed(&mut window).contains(&("Shared researcher".to_owned(), chosen)),
        "the newly opened agent DM has a named sidebar row before its first message"
    );
    assert_eq!(window.chat().empty_channel_message(), None);
    let chat = window.chat();
    assert_eq!(
        chat.conversation_placeholder(&chat.active_channel),
        crate::i18n::t("chat-person-chat-placeholder")
    );
}

#[test]
fn setting_up_from_chat_opens_the_same_personal_dm_on_every_request() {
    ui::i18n::I18nManager::init("en-US");
    let mut window = TestApp::<ChatShell>::in_workspace(WORKSPACE).expect("Chat");
    window.harness().resize((420, 800)).unwrap();
    window.settle();
    let agents = window
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .unwrap();
    assert!(agents.get().agents.is_empty());
    let setup = window
        .chat()
        .sidebar_sections()
        .into_iter()
        .flat_map(|section| section.items)
        .find(|item| item.id == "set-up-personal-assistant")
        .expect("a first-use entry in Chat");
    assert!(
        setup.chosen_by_press,
        "browsing the sidebar never provisions an assistant"
    );
    window.with_widget::<ui::widget::NavShell, _>(|shell| {
        shell.show(ui::widget::NavPane::Sidebar);
        shell.snap();
    });
    window.settle();
    for attempt in 0..2 {
        if attempt == 0 {
            let row = window
                .harness()
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.label() == Some("Set up personal assistant"))
                .and_then(|(_, node)| node.bounds())
                .expect("the setup control is visible");
            window
                .harness()
                .click_at(f64::midpoint(row.x0, row.x1), f64::midpoint(row.y0, row.y1));
        } else {
            window
                .call_on_finished(
                    "chat.workspace",
                    "set-up-personal-assistant",
                    serde_json::json!({}),
                )
                .expect("retry through the published action");
        }
        window.settle();
        let state = agents.get();
        let person = window.chat().me.id.clone();
        let assistant = state.personal_assistant(person.id()).expect("my assistant");
        assert_eq!(
            window.chat().conversation_agent().as_deref(),
            Some(assistant.id.0.as_str())
        );
        assert_eq!(window.chat().window_name(), assistant.name);
        assert_eq!(
            window.with_widget::<ui::widget::NavShell, _>(|shell| shell.front_pane()),
            Some(ui::widget::NavPane::Content),
            "setup reveals the conversation when the sidebar fills the window"
        );
        assert_eq!(state.agents.len(), 1);
    }
}

#[test]
fn a_denied_setup_leaves_the_viewer_in_their_original_conversation() {
    let mut window = TestApp::<ChatShell>::with_role(ui::Role::Viewer).expect("Chat");
    window.frame();
    let before = window.chat().active_channel.clone();
    assert!(
        window
            .call_on_finished(
                "chat.workspace",
                "set-up-personal-assistant",
                serde_json::json!({})
            )
            .is_err()
    );
    assert_eq!(window.chat().active_channel, before);
    assert!(
        window
            .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
            .unwrap()
            .get()
            .agents
            .is_empty()
    );
}
