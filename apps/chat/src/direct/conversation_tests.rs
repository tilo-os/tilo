//! A direct conversation as the workspace keeps it: the chat's own messages
//! between two of its people, named by them.

use ui::framework::testing::TestApp;

use crate::ChatShell;
use crate::record::{Chat, Participant};
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

/// The conversation two people have is named by the two of them, whichever
/// order they are named in, once.
#[test]
fn a_conversation_is_named_by_its_participants_once() {
    let ada = Participant::person("ad".repeat(32));
    let bob = Participant::person("b0".repeat(32));
    let one = Chat::direct_id([ada.clone(), bob.clone()]);
    let other = Chat::direct_id([bob.clone(), ada.clone(), ada.clone()]);
    assert_eq!(one, other, "one conversation per pair, however named");
    assert_eq!(
        Chat::direct_participants(&one),
        Some(vec![ada, bob]),
        "the id names its participants back"
    );
    assert_eq!(Chat::direct_participants(&"c_general".into()), None);
}

#[test]
fn an_agent_and_a_person_converse_apart_from_the_pair_of_people() {
    let ada = Participant::person("ad".repeat(32));
    let agent = Participant::agent("ag".repeat(32));
    assert_ne!(
        Chat::direct_id([ada.clone(), agent]),
        Chat::direct_id([ada, Participant::person("ag".repeat(32))]),
        "who someone is names the conversation"
    );
}

/// Ada's and Bob's windows in one workspace, each having introduced themself,
/// the record delivered both ways.
fn a_pair_of_windows(workspace: &str, ada: &str, bob: &str) -> (Window, Window) {
    use crate::record::chat::Introduce;

    let mut ada_window = Window::for_person_in(ada, workspace).expect("Ada's window opens");
    let mut bob_window = Window::for_person_in(bob, workspace).expect("Bob's window opens");
    for (window, name, initials, color) in [
        (&mut ada_window, "Ada", "A", 1),
        (&mut bob_window, "Bob", "B", 2),
    ] {
        window
            .invoke(Chat::INTRODUCE.with(Introduce {
                name: name.into(),
                initials: initials.into(),
                color,
            }))
            .expect("each person introduces themself");
    }
    ada_window
        .deliver_changes_to(&mut bob_window)
        .expect("one record");
    bob_window
        .deliver_changes_to(&mut ada_window)
        .expect("one record");
    ada_window.harness().relayout();
    bob_window.harness().relayout();
    (ada_window, bob_window)
}

/// What `window`'s record holds of its conversation with `peer`: each
/// message's text and the person it names as its author.
fn read_conversation(window: &mut Window, peer: &str) -> Vec<(String, String)> {
    let me = window
        .with_chat(|chat| chat.me.id.clone())
        .expect("a window");
    let id = Chat::direct_id([
        Participant::person(me),
        Participant::person(peer.to_owned()),
    ]);
    crate::testing::mounted_rows(window, id.as_str())
        .into_iter()
        .map(|message| (message.text, message.author.as_str().to_owned()))
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
        .deliver_record_to(reader, &crate::parts::part(0))
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
                .any(|row| row.direct && row.participants.iter().any(|key| key.contains(&ada))),
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
    let conversation = Chat::direct_id([
        Participant::person(ada.clone()),
        Participant::person(bob.clone()),
    ]);
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
             conversation opens as a window of the desk"
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
    let conversation =
        Chat::direct_id([Participant::person(ada.clone()), Participant::person(bob)]);
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
    let conversation =
        Chat::direct_id([Participant::person(bob), Participant::agent(agent.clone())]);

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
             conversation opens as a window of the desk, floated where the \
             box stood"
        );
    });
    let rows = crate::testing::mounted_rows(&mut bob_window, &conversation.to_string());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "please look");
    bob_window.harness().relayout();
    bob_window.with_chat(|chat| {
        assert!(
            chat.direct_contacts()
                .iter()
                .any(|row| row.id == conversation.to_string()),
            "the conversation is listed with the person's others, though the record keeps no row for it"
        );
    });
}

/// The window a box's send floats over the desk opens on the item the send answered, as a
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
/// that names it: the desk opens the window on it where the box stood, so the
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
    let conversation = Chat::direct_id([Participant::person(ada), Participant::person(bob)]);

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
        "the send names the conversation as an item of the chat, so the desk opens it"
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
    bob_window
        .effect(&ChatEffect::OpenDirectConversation {
            public_key: ada.clone(),
        })
        .expect("Bob opens the conversation with Ada");
    bob_window.with_chat(|chat| {
        assert!(
            matches!(chat.focus, crate::app::Focus::Main),
            "the conversation opens with its composer ready"
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
        .direct_conversation(&unknown, &Chat::default())
        .expect_err("a window without the workspace's chat");
    assert!(
        refused.contains(&crate::i18n::t("chat-direct-unavailable")),
        "the window says what it lacks: {refused}"
    );
}

/// An agent Ada admitted sends a message from her window: the message is the agent's, not
/// Ada's, on her device and on Bob's.
#[test]
fn an_agents_send_is_the_agents_on_every_member() {
    use ui::tilo_resource::Resource as _;

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000e";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let (mut ada_window, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    let conversation =
        Chat::direct_id([Participant::person(ada.clone()), Participant::person(bob)]);
    let helper = tilo_actions::ActionInvoker::actor("agent:helper")
        .with_kind(tilo_actions::ActorKind::Agent)
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
            .map(|row| (row.text.as_str(), row.author.as_str()))
            .collect::<Vec<_>>(),
        [("hello from the agent", "agent:helper")],
        "on Ada's device the message is the agent's"
    );
    deliver_messages(&mut ada_window, &mut bob_window);
    let theirs = crate::testing::mounted_rows(&mut bob_window, &conversation.to_string());
    assert_eq!(
        theirs
            .iter()
            .map(|row| (row.text.as_str(), row.author.as_str()))
            .collect::<Vec<_>>(),
        [("hello from the agent", "agent:helper")],
        "and on Bob's"
    );
}
