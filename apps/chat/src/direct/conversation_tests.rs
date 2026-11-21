//! A direct conversation as the workspace keeps it: the chat's own messages
//! between two of its people, named by them.

use ui::framework::testing::TestApp;

use crate::ChatShell;
use tilo_actions::policy::MemberId;

use crate::chat::Chat;
use crate::testing::MountedChat;

type Window = TestApp<ChatShell>;

#[test]
fn registered_person_effect_ids_match_the_native_broker_protocol() {
    use crate::effects::ChatEffect;

    assert_eq!(
        ChatEffect::OPEN_DIRECT_CONVERSATION.id,
        "open-direct-conversation"
    );
    assert_eq!(ChatEffect::SEND_DIRECT_MESSAGE.id, "send-direct-message");
}

/// Opening the conversation with a person shows it, on the row Chat publishes for them only once
/// it is open: the surface the package ships says so, so a closed Chat asked for it starts with a
/// window.
#[test]
fn the_surface_says_opening_a_conversation_with_a_person_shows_it() {
    use crate::effects::ChatEffect;

    // Named under an id of its own: the surface this process holds for Chat stays as it was.
    let app = "so.tilo.chat.shipped-surface";
    ui::app_surface::register_package(app, include_bytes!("../../surface.toml"));
    assert!(ui::app_surface::shows(
        app,
        ChatEffect::OPEN_DIRECT_CONVERSATION.id
    ));
    assert!(!ui::app_surface::shows(
        app,
        ChatEffect::SEND_DIRECT_MESSAGE.id
    ));
}

tilo_test_env::scenario! {
    /// The id of a direct conversation.
    /// Each case was a test of its own, on a fixture of its own; a failure
    /// names the cases that failed.
    fn a_conversations_id_names_its_members_people_or_agents;

    phase a_conversation_is_named_by_its_members_once;
    // The conversation two people have is named by the two of them, whichever
    // order they are named in, once.
    {
        let ada = MemberId::person("ad".repeat(32));
        let bob = MemberId::person("b0".repeat(32));
        let one = Chat::direct_id([ada.clone(), bob.clone()]);
        let other = Chat::direct_id([bob.clone(), ada.clone(), ada.clone()]);
        assert_eq!(one, other, "one conversation per pair, however named");
        assert_eq!(
            Chat::direct_members(&one),
            Some(vec![ada, bob]),
            "the id names its members back"
        );
        assert_eq!(Chat::direct_members(&"c_general".into()), None);
        // A conversation with oneself names one person; an id that names nobody,
        // or does not read as members, names no conversation.
        let cy = MemberId::person("c1".repeat(32));
        let alone = Chat::direct_id([cy.clone()]);
        assert_eq!(
            Chat::direct_members(&alone),
            Some(vec![cy]),
            "a conversation with oneself"
        );
        assert_eq!(
            Chat::direct_members(&Chat::direct_id(std::iter::empty::<MemberId>())),
            None,
            "an id that names nobody"
        );
        let dangling = format!("{}|", alone.as_str());
        assert_eq!(
            Chat::direct_members(&dangling.as_str().into()),
            None,
            "an id that does not read as members"
        );
    }

    phase an_agent_and_a_person_converse_apart_from_the_pair_of_people;
    {
        let ada = MemberId::person("ad".repeat(32));
        let agent = MemberId::agent("ag".repeat(32));
        assert_ne!(
            Chat::direct_id([ada.clone(), agent]),
            Chat::direct_id([ada, MemberId::person("ag".repeat(32))]),
            "who someone is names the conversation"
        );
    }
}

/// Ada's and Bob's windows in one workspace, each listing the other as a member,
/// the record delivered both ways.
fn a_pair_of_windows(workspace: &str, ada: &str, bob: &str) -> (Window, Window) {
    let mut ada_window = Window::for_person_in(ada, workspace).expect("Ada's window opens");
    let mut bob_window = Window::for_person_in(bob, workspace).expect("Bob's window opens");
    ada_window
        .deliver_changes_to(&mut bob_window)
        .expect("one resource");
    bob_window
        .deliver_changes_to(&mut ada_window)
        .expect("one resource");
    lists_member(&mut ada_window, bob, "Bob");
    lists_member(&mut bob_window, ada, "Ada");
    (ada_window, bob_window)
}

/// `window`'s workspace lists the person `id`, called `name`, as the member list its host
/// delivers says: whether or not they ever opened Chat.
fn lists_member(window: &mut Window, id: &str, name: &str) {
    window.harness().update_presence(|presence| {
        presence.set_display_name(MemberId::person(id.to_owned()), name);
        true
    });
    window.harness().relayout();
}

/// What `window`'s resource holds of its conversation with `peer`: each
/// message's text and the person it names as its author.
fn read_conversation(window: &mut Window, peer: &str) -> Vec<(String, String)> {
    let me = window
        .with_chat(|chat| chat.me.id.clone())
        .expect("a window");
    let id = Chat::direct_id([me, MemberId::person(peer.to_owned())]);
    crate::testing::mounted_rows(window, id.as_str())
        .into_iter()
        .map(|message| (message.text, message.author.id().to_owned()))
        .collect()
}

fn send(window: &mut Window, peer: &str, text: &str) {
    use crate::effects::ChatEffect;

    let sent = window
        .effect(&ChatEffect::SendDirectMessage {
            public_key: peer.into(),
            text: text.into(),
        })
        .expect("the window sends");
    assert_eq!(
        sent.value
            .as_ref()
            .and_then(|value| value["accepted"].as_bool()),
        Some(true),
        "the conversation takes {text:?}: {:?}",
        sent.value
    );
}

/// The messages `sender` wrote reach `reader`: the workspace sends the part
/// they are in, which the reader opens as a device joining a chat does.
fn deliver_messages(sender: &mut Window, reader: &mut Window) {
    reader
        .with_chat(|chat| chat.chain.clone())
        .flatten()
        .expect("the reader holds the chat's messages")
        .hold(0)
        .expect("the reader opens the part the conversation is in");
    sender
        .deliver_resource_to(reader, &crate::parts::part(0))
        .expect("the messages arrive");
    reader.harness().relayout();
}

/// Two people, a window each in one workspace, talk in the workspace's own
/// chat: what one's window sends, the other's window reads, in the messages
/// every channel of the workspace keeps beside it.
#[test]
fn two_peoples_chat_is_the_workspaces_own_messages() {
    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000b";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (mut ada_window, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);

    bob_window.with_chat(|chat| {
        assert!(
            chat.channels
                .iter()
                .any(|row| row.direct && row.members.contains(&MemberId::person(ada.clone()))),
            "Bob's window names Ada's conversation: {:?}",
            chat.channels
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>()
        );
    });

    send(&mut ada_window, &bob, "hello from Ada");
    deliver_messages(&mut ada_window, &mut bob_window);
    let messages = read_conversation(&mut bob_window, &ada);
    assert_eq!(messages.len(), 1, "Bob reads what Ada sent");
    assert_eq!(messages[0].0, "hello from Ada");
    assert_eq!(messages[0].1, ada, "the message says Ada wrote it");

    send(&mut bob_window, &ada, "hello from Bob");
    deliver_messages(&mut bob_window, &mut ada_window);
    let said: Vec<String> = read_conversation(&mut ada_window, &bob)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(
        said,
        ["hello from Ada", "hello from Bob"],
        "both people's messages are in the one conversation"
    );
}

/// What a window's box sends goes to the conversation Chat already keeps with
/// the person it names — Ada reads it there as one more message, carrying what
/// the box held — never a second conversation with the same person.
#[test]
fn a_box_send_reaches_the_conversation_the_two_already_have() {
    use crate::effects::ChatEffect;
    use ui::floating_surface::{Recipient, RecipientKind};

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000c";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (mut ada_window, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let conversation =
        Chat::direct_id([MemberId::person(ada.clone()), MemberId::person(bob.clone())]);
    let carried = tilo_reference::Attached::Text {
        text: "the wheel stutters".into(),
        from: None,
    };

    let sent = bob_window
        .effect(&ChatEffect::Send {
            to: vec![Recipient::found(RecipientKind::Person, ada, "Ada")],
            attached: vec![carried],
            text: "can you look?".into(),
            parent: None,
            body: None,
            opens: Some(true),
        })
        .expect("Bob's box sends");
    let value = sent.value.expect("the send says where the message went");
    assert_eq!(
        value["conversation"],
        conversation.to_string(),
        "the message joins the conversation they already have"
    );
    assert_eq!(value["accepted"].as_bool(), Some(true));
    bob_window.with_chat(|chat| {
        assert_ne!(
            chat.active_channel,
            conversation.to_string(),
            "the window that sent it goes on showing what it showed: the \
             conversation opens as a window of the shell"
        );
    });

    deliver_messages(&mut bob_window, &mut ada_window);
    assert_eq!(
        read_conversation(&mut ada_window, &bob),
        [("can you look?".to_owned(), bob.clone())],
        "Ada reads what the box sent"
    );
    let rows = crate::testing::mounted_rows(&mut ada_window, &conversation.to_string());
    assert_eq!(
        rows.len(),
        1,
        "the conversation holds the one message, not a second conversation"
    );
    assert_eq!(
        rows[0].attachments,
        [tilo_reference::Attached::Text {
            text: "the wheel stutters".into(),
            from: None,
        }],
        "the message carries what the box held"
    );
}

/// A box's send reaches a member of the workspace who never opened Chat: the workspace's member
/// list says who they are, and the message is waiting in their conversation when they first do.
#[test]
fn a_box_send_reaches_a_member_who_never_opened_chat() {
    use crate::effects::ChatEffect;
    use ui::floating_surface::{Recipient, RecipientKind};

    const WORKSPACE: &str = "00000000-0000-4000-8000-000000000010";
    let ada = "ad".repeat(32);
    let ben = "be".repeat(32);
    let mut ada_window = Window::for_person_in(&ada, WORKSPACE).expect("Ada's window opens");
    lists_member(&mut ada_window, &ben, "Ben");

    let sent = ada_window
        .effect(&ChatEffect::Send {
            to: vec![Recipient::found(RecipientKind::Person, ben.clone(), "Ben")],
            attached: Vec::new(),
            text: "have a look at this plan".into(),
            parent: None,
            body: None,
            opens: Some(true),
        })
        .expect("Ada's box sends to Ben");
    assert_eq!(
        sent.value
            .as_ref()
            .and_then(|value| value["accepted"].as_bool()),
        Some(true),
        "{:?}",
        sent.value
    );

    // Ben opens Chat for the first time: the resource and the message reach his window.
    let mut ben_window = Window::for_person_in(&ben, WORKSPACE).expect("Ben's window opens");
    ada_window
        .deliver_changes_to(&mut ben_window)
        .expect("one resource");
    lists_member(&mut ben_window, &ada, "Ada");
    deliver_messages(&mut ada_window, &mut ben_window);
    assert_eq!(
        read_conversation(&mut ben_window, &ada),
        [("have a look at this plan".to_owned(), ada.clone())],
        "Ben reads what Ada sent him"
    );
}

/// A box with no words to send but something to send it about — a task
/// handed to someone — posts the message carrying that, and nothing else.
#[test]
fn a_box_send_of_a_subject_alone_posts_the_message_without_words() {
    use crate::effects::ChatEffect;
    use ui::floating_surface::{Recipient, RecipientKind};

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000d";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (mut ada_window, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let conversation = Chat::direct_id([MemberId::person(ada.clone()), MemberId::person(bob)]);
    let carried = tilo_reference::Attached::Text {
        text: "#42 Ship the release".into(),
        from: None,
    };

    bob_window
        .effect(&ChatEffect::Send {
            to: vec![Recipient::found(RecipientKind::Person, ada, "Ada")],
            attached: vec![carried.clone()],
            text: String::new(),
            parent: None,
            body: None,
            opens: Some(true),
        })
        .expect("Bob's box sends the task alone");

    deliver_messages(&mut bob_window, &mut ada_window);
    let rows = crate::testing::mounted_rows(&mut ada_window, &conversation.to_string());
    assert_eq!(rows.len(), 1, "the message is there");
    assert_eq!(rows[0].text, "", "with no words beside what it carries");
    assert_eq!(
        rows[0].attachments,
        [carried],
        "the task the box was about is the message"
    );
}

/// A message a box sends to an agent lands in the conversation with that
/// agent, made as the message arrives: the workspace needs no channel made
/// first.
#[test]
fn a_box_send_to_an_agent_opens_its_conversation() {
    use crate::effects::ChatEffect;
    use ui::floating_surface::{Recipient, RecipientKind};

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000d";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (_, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let agent = "a5".repeat(16);
    let conversation = Chat::direct_id([MemberId::person(bob), MemberId::agent(agent.clone())]);

    let sent = bob_window
        .effect(&ChatEffect::Send {
            to: vec![Recipient::found(RecipientKind::Agent, agent, "Ada")],
            attached: Vec::new(),
            text: "please look".into(),
            parent: None,
            body: None,
            opens: Some(true),
        })
        .expect("Bob's box sends");
    let value = sent.value.expect("the send says where the message went");
    assert_eq!(value["conversation"], conversation.to_string());
    bob_window.with_chat(|chat| {
        assert_ne!(
            chat.active_channel,
            conversation.to_string(),
            "the window that sent it goes on showing what it showed: the \
             conversation opens as a window of the shell, floated where the \
             box stood"
        );
    });
    let rows = crate::testing::mounted_rows(&mut bob_window, &conversation.to_string());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "please look");
    assert_eq!(
        value["message"].as_str(),
        Some(rows[0].id.as_str()),
        "the send says which message it posted: what asked stands its turn by it"
    );
    bob_window.harness().relayout();
    bob_window.with_chat(|chat| {
        assert!(
            chat.direct_contacts()
                .iter()
                .any(|row| row.id == conversation.to_string()),
            "the conversation is listed with the person's others, though the resource keeps no row for it"
        );
    });
}

/// An agent's send names whom it goes to by name alone: the person the workspace's member list
/// calls that, or the agent it calls that.
#[test]
fn a_send_by_name_reaches_the_member_called_that() {
    use ui::floating_surface::{Recipient, RecipientKind};

    let mut app = crate::ChatApp::new_ephemeral();
    let agent = "a5".repeat(16);
    crate::testing::agent_member(&mut app, &agent, "Reviewer");
    app.workspace_presence_answers = true;
    app.workspace_names
        .insert(MemberId::person("ad".repeat(32)), "Ada".to_owned());
    let named = |kind, name: &str| app.member_named(&Recipient::found(kind, "", name));
    assert_eq!(
        named(RecipientKind::Person, "ada"),
        Ok(MemberId::person("ad".repeat(32)))
    );
    assert_eq!(
        named(RecipientKind::Agent, "reviewer"),
        Ok(MemberId::agent(agent))
    );
    assert!(
        named(RecipientKind::Agent, "Ada").is_err(),
        "a name nobody of that kind has is refused, not sent to an empty conversation"
    );
}

/// The window a box's send floats over the shell opens on the item the send answered, as a
/// window launched to reveal it does: a conversation with an agent is one by its id, listed in
/// the sidebar or not, so the window shows the message that was sent instead of the first channel.
#[test]
fn the_conversation_a_send_answers_opens_in_a_window() {
    use crate::effects::ChatEffect;
    use ui::floating_surface::{Recipient, RecipientKind};

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000f";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (_, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let agent = "a5".repeat(16);
    let sent = bob_window
        .effect(&ChatEffect::Send {
            to: vec![Recipient::found(RecipientKind::Agent, agent, "Ada")],
            attached: Vec::new(),
            text: "please look".into(),
            parent: None,
            body: None,
            opens: Some(true),
        })
        .expect("Bob's box sends");
    let anchor = sent.value.expect("the send answers its conversation")["anchor"]
        .as_str()
        .expect("an item")
        .to_owned();
    let Some((_, conversation)) = ui::tilo_resource::items::item_of_resource::<Chat>(&anchor)
    else {
        panic!("the anchor names an item of the chat: {anchor}");
    };

    // The window opens the item as one launched to reveal it does.
    bob_window.with_chat(|chat| chat.opener()(&anchor));
    bob_window.harness().relayout();
    bob_window.with_chat(|chat| {
        assert_eq!(chat.active_channel, conversation.to_string());
        assert_eq!(
            chat.messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["please look"],
            "it shows the message that was sent"
        );
    });
}

/// A send made in a box answers the conversation it landed in as the item
/// that names it: the shell opens the window on it where the box stood, so the
/// message and the conversation it reached are one gesture there. The window
/// that sent goes on showing what it showed.
#[test]
fn a_box_send_answers_the_conversation_it_landed_in() {
    use crate::effects::ChatEffect;
    use ui::floating_surface::{Recipient, RecipientKind};

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000e";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (_, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let conversation = Chat::direct_id([MemberId::person(ada), MemberId::person(bob)]);

    let sent = bob_window
        .effect(&ChatEffect::Send {
            to: vec![Recipient::found(
                RecipientKind::Conversation,
                conversation.to_string(),
                "Ada",
            )],
            attached: Vec::new(),
            text: "look at this".into(),
            parent: None,
            body: None,
            opens: Some(true),
        })
        .expect("Bob's box sends");
    let value = sent.value.expect("the send says where the message went");
    assert_eq!(
        value["anchor"],
        crate::effects::item_row_id("channels", conversation.as_str()),
        "the send names the conversation as an item of the chat, so the shell opens it"
    );
}

/// What a person's card asks of Chat when they press Message: the row naming
/// their conversation, which opens it with the composer ready.
#[test]
fn a_persons_card_opens_the_conversation_chat_keeps() {
    use crate::effects::ChatEffect;

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000b";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (_, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    // Bob's window has the keyboard, and he is answering in a thread.
    bob_window.with_chat(|chat| {
        ui::widget::Widget::set_focus(chat, true);
        chat.thread_open = Some("m_earlier".to_owned());
        chat.focus = crate::app::Focus::Thread;
        chat.set_composer_focus();
        assert!(chat.composer_thread.is_focused(), "he writes in the thread");
    });
    bob_window
        .effect(&ChatEffect::OpenDirectConversation {
            public_key: ada.clone(),
        })
        .expect("Bob opens the conversation with Ada");
    bob_window.with_chat(|chat| {
        assert!(
            matches!(chat.focus, crate::app::Focus::Main),
            "the conversation opens on its own composer"
        );
        assert!(
            chat.composer_main.is_focused() && !chat.composer_thread.is_focused(),
            "what Bob types next goes to Ada"
        );
    });
    bob_window.with_chat(|chat| {
        let row = chat
            .direct_conversation_rows()
            .into_iter()
            .find(|row| row.id() == format!("chat.conversation:{ada}"))
            .expect("Chat lists the conversation with Ada");
        let resource = row.into_resource();
        assert_eq!(resource.kind, "chat.conversation", "{:?}", resource.id);
        let offered: Vec<&str> = resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for operation in [
            ChatEffect::OPEN_DIRECT_CONVERSATION.id,
            ChatEffect::SEND_DIRECT_MESSAGE.id,
        ] {
            assert!(
                offered.contains(&operation),
                "the row offers {operation} on the conversation: {offered:?}"
            );
        }
    });
}

/// A window whose chat this device does not hold says so, and one that knows
/// no such person says that instead: two different things to tell the person
/// asking.
#[test]
fn a_conversation_that_is_here_and_a_person_that_is_not_are_named_apart() {
    use crate::effects::ChatEffect;

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000b";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (_, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let unknown = "ff".repeat(32);
    let refused = bob_window
        .effect(&ChatEffect::OpenDirectConversation {
            public_key: unknown.clone(),
        })
        .expect_err("a person the workspace does not know");
    assert!(
        refused
            .to_string()
            .contains(&crate::i18n::t("chat-person-unknown")),
        "the window says which person it does not know: {refused}"
    );
    let refused = bob_window
        .effect(&ChatEffect::OpenDirectConversation { public_key: bob })
        .expect_err("this window's own person has no conversation with themself");
    assert!(
        refused
            .to_string()
            .contains(&crate::i18n::t("chat-person-unknown")),
        "this window's own person is no conversation of theirs: {refused}"
    );

    // A window that does not hold the workspace's chat yet answers the
    // person differently, naming what it lacks.
    let empty = crate::ChatApp::new_ephemeral();
    let refused = empty
        .direct_conversation(&unknown)
        .expect_err("a window without the workspace's chat");
    assert!(
        refused.contains(&crate::i18n::t("chat-direct-unavailable")),
        "the window says what it lacks: {refused}"
    );
}

/// An agent Ada admitted sends a message from her window: the message is the agent's, not
/// Ada's, on her device and on Bob's, and names it with her where nobody sees the agent here.
#[test]
fn an_agents_send_is_the_agents_on_every_member() {
    use ui::tilo_resource::Resource as _;

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000e";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (mut ada_window, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let conversation = Chat::direct_id([MemberId::person(ada.clone()), MemberId::person(bob)]);
    let helper = tilo_actions::ActionInvoker::agent("helper")
        .acting_for(ada)
        .named(Some("Helper".into()));

    ada_window
        .call_by(
            helper,
            Chat::KIND,
            "send",
            serde_json::json!({
                "to": [{ "kind": "conversation", "id": conversation.to_string(), "name": "Bob" }],
                "attached": [],
                "text": "hello from the agent",
            }),
        )
        .expect("the agent's send is taken");

    let mine = crate::testing::mounted_rows(&mut ada_window, &conversation.to_string());
    assert_eq!(
        mine.iter()
            .map(|row| (row.text.as_str(), row.author.id()))
            .collect::<Vec<_>>(),
        [("hello from the agent", "helper")],
        "on Ada's device the message is the agent's"
    );
    deliver_messages(&mut ada_window, &mut bob_window);
    let theirs = crate::testing::mounted_rows(&mut bob_window, &conversation.to_string());
    assert_eq!(
        theirs
            .iter()
            .map(|row| (row.text.as_str(), row.author.id()))
            .collect::<Vec<_>>(),
        [("hello from the agent", "helper")],
        "and on Bob's"
    );
    bob_window.harness().relayout();
    assert_eq!(
        bob_window.with_chat(|chat| chat.message_author_name(&theirs[0])),
        Some("Helper".to_owned()),
        "Bob, who never saw the agent here, reads it by its own name"
    );
}

/// Where the window is, as a person reads it: the conversation open, the row
/// the sidebar marks, the place the window bar names, and whether the call's
/// stage fills the window.
#[derive(Debug, PartialEq, Eq)]
struct Shown {
    open: String,
    marked: Option<String>,
    named: Option<String>,
    stage: bool,
}

fn shown(window: &mut Window) -> Shown {
    window.idle();
    let (open, marked, stage) = window
        .with_chat(|chat| {
            let marked = chat.places.shown().and_then(|(_, selected)| selected);
            (
                chat.active_channel.clone(),
                marked,
                chat.call.active && chat.call.expanded,
            )
        })
        .expect("a window");
    let named = ui::window_toolbar::declared()
        .and_then(|toolbar| toolbar.location)
        .map(|location| location.label);
    Shown {
        open,
        marked,
        named,
        stage,
    }
}

/// The sidebar's rows as the window last laid them out, top to bottom: each
/// place's id and where a press lands on it.
fn sidebar_rows(window: &mut Window) -> Vec<(String, vello::kurbo::Point)> {
    window.harness().relayout();
    let mut rows: Vec<_> = window
        .harness()
        .targets()
        .into_iter()
        // A sidebar row is named by its key in the list that holds it, as a keyed list names
        // its rows: the row itself, not the parts inside it.
        .filter(|(name, _)| {
            name.contains("NavList[")
                && name
                    .rsplit('/')
                    .next()
                    .is_some_and(|part| part.starts_with("row["))
        })
        .map(|(_, rect)| rect)
        .collect();
    rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    rows.into_iter()
        .filter_map(|rect| {
            let (id, _) = window
                .with_chat(|chat| chat.places.place_at(rect.center()))
                .flatten()?;
            Some((id, rect.center()))
        })
        .collect()
}

/// Bob's window with every kind of sidebar row: the channel `chat`, the
/// voice channel `voice`, a conversation Ada wrote to him and one he started
/// with Cy.
struct Sidebar {
    window: Window,
    chat: String,
    voice: String,
    with_ada: String,
    with_cy: String,
    ada: String,
    cy: String,
}

impl Sidebar {
    fn new() -> Self {
        const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000b";
        let ada = "ad".repeat(32);
        let bob = "b0".repeat(32);
        let cy = "c1".repeat(32);
        let (mut ada_window, mut window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
        lists_member(&mut window, &cy, "Cy");
        send(&mut ada_window, &bob, "hello from Ada");
        deliver_messages(&mut ada_window, &mut window);
        send(&mut window, &cy, "hello Cy");
        let mut sidebar = Self {
            window,
            chat: String::new(),
            voice: String::new(),
            with_ada: String::new(),
            with_cy: String::new(),
            ada,
            cy,
        };
        sidebar.chat = sidebar.row("c_chat");
        sidebar.voice = sidebar.row("c_voice");
        sidebar.with_ada = sidebar.row(&sidebar.ada.clone());
        sidebar.with_cy = sidebar.row(&sidebar.cy.clone());
        sidebar
    }

    /// The id of the row whose id holds `part`.
    fn row(&mut self, part: &str) -> String {
        let rows = sidebar_rows(&mut self.window);
        rows.iter().find(|(id, _)| id.contains(part)).map_or_else(
            || panic!("the sidebar lists {part}: {rows:?}"),
            |(id, _)| id.clone(),
        )
    }

    /// A press on the row `id`, where the window lays it out now.
    fn press(&mut self, id: &str) {
        let at = sidebar_rows(&mut self.window)
            .into_iter()
            .find(|(row, _)| row == id)
            .map_or_else(|| panic!("the sidebar lists {id}"), |(_, at)| at);
        self.window.harness().click_at(at.x, at.y);
    }

    fn key(&mut self, key: &str) {
        self.window.harness().key(key);
    }

    /// The window shows the conversation of row `id`, marked in the sidebar
    /// and named `name` in the window bar.
    fn opens(&mut self, id: &str, name: &str, what: &str) {
        assert_eq!(
            shown(&mut self.window),
            Shown {
                open: id.strip_prefix("channel:").unwrap_or(id).to_owned(),
                marked: Some(id.to_owned()),
                named: Some(name.to_owned()),
                stage: false,
            },
            "{what}"
        );
    }
}

/// Every kind of row in Chat's sidebar opens what it names on the first
/// press, and the sidebar's mark and the window bar follow at once: a
/// channel, a direct conversation (one written to this person and one they
/// started), and a voice channel, whose call's stage then fills the window.
#[test]
fn every_sidebar_row_opens_on_the_first_press() {
    let mut bob = Sidebar::new();
    let (chat, voice) = (bob.chat.clone(), bob.voice.clone());
    let (with_ada, with_cy) = (bob.with_ada.clone(), bob.with_cy.clone());

    bob.press(&with_ada);
    bob.opens(&with_ada, "Ada", "a conversation written to Bob");
    let texts = bob.window.texts();
    assert!(
        texts.iter().any(|text| text == "hello from Ada"),
        "its messages show: {texts:?}"
    );
    bob.press(&chat);
    bob.opens(&chat, "chat", "a channel");
    bob.press(&with_cy);
    bob.opens(&with_cy, "Cy", "a conversation Bob started");
    bob.press(&with_ada);
    bob.opens(&with_ada, "Ada", "one conversation to the next");

    bob.press(&voice);
    assert_eq!(
        shown(&mut bob.window),
        Shown {
            // The conversation open stays open behind the call.
            open: with_ada
                .strip_prefix("channel:")
                .unwrap_or(&with_ada)
                .to_owned(),
            marked: Some(voice),
            named: Some("voice".to_owned()),
            stage: true,
        },
        "the call chosen fills the window"
    );
    // Bob is listed in the call now, under its channel: the rows below moved.
    bob.press(&with_cy);
    bob.opens(&with_cy, "Cy", "a conversation from the call");
}

/// The arrow keys walk the sidebar's rows, opening each conversation they
/// reach, and pass the voice channel without joining its call.
#[test]
fn the_arrow_keys_walk_past_a_call_to_the_conversations() {
    let mut bob = Sidebar::new();
    let chat = bob.chat.clone();
    let (with_ada, with_cy) = (bob.with_ada.clone(), bob.with_cy.clone());

    bob.press(&chat);
    bob.opens(&chat, "chat", "the channel");
    bob.key("ArrowDown");
    let passing = shown(&mut bob.window);
    assert_eq!(
        passing.marked.as_deref(),
        Some(chat.as_str()),
        "passing the voice channel opens nothing"
    );
    assert!(
        !bob.window
            .with_chat(|chat| chat.call.active)
            .unwrap_or(true),
        "passing the voice channel joins no call"
    );
    bob.key("ArrowDown");
    bob.opens(&with_ada, "Ada", "the key past the voice channel");
    bob.key("ArrowDown");
    bob.opens(&with_cy, "Cy", "the next conversation by key");
    for _ in 0..3 {
        bob.key("ArrowUp");
    }
    bob.opens(&chat, "chat", "up past the voice channel");
}
