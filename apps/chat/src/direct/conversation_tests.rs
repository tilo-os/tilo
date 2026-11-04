use crate::composer::{ConversationKey, DraftComposer};
use automerge::{AutoCommit, transaction::Transactable as _};

fn person(workspace: &str, public_key: &str) -> ConversationKey {
    ConversationKey::Person {
        workspace: workspace.into(),
        public_key: public_key.into(),
    }
}

#[test]
fn registered_person_effect_ids_match_the_native_broker_protocol() {
    use crate::effects::ChatEffect;

    assert_eq!(
        ChatEffect::OPEN_DIRECT_CONVERSATION.id,
        "open-direct-conversation"
    );
    assert_eq!(
        ChatEffect::READ_DIRECT_CONVERSATION.id,
        "read-direct-conversation"
    );
    assert_eq!(ChatEffect::SEND_DIRECT_MESSAGE.id, "send-direct-message");
}

/// A private draft is held under a key naming the workspace's scope, so the
/// same conversation always finds the words typed in it.
#[test]
fn a_private_draft_is_kept_under_a_key_naming_the_workspace() {
    let key = crate::app::ChatApp::person_conversation_key("peer");
    assert_eq!(key.draft_key(), "person:9:workspace:4:peer");
}

#[test]
fn accepted_person_send_cannot_clear_a_newer_editor_revision() {
    let mut workspace = AutoCommit::new();
    let before = workspace.get_heads();
    let key = person("workspace", "peer");
    let mut composer = DraftComposer::shared();
    composer.set_conversation_draft(&key);
    composer.insert_text(&mut ui::widget::Edits::lend(&mut workspace), "First");
    let sent_revision = composer.conversation_revision(&key).unwrap();
    composer.insert_text(&mut ui::widget::Edits::lend(&mut workspace), " more");
    assert!(!composer.clear_conversation_revision(&key, sent_revision));
    assert_eq!(composer.private_plain().as_deref(), Some("First more"));
    let current_revision = composer.conversation_revision(&key).unwrap();
    assert!(composer.clear_conversation_revision(&key, current_revision));
    assert_eq!(composer.private_plain().as_deref(), Some(""));
    assert_eq!(workspace.get_heads(), before);
    assert_eq!(workspace.pending_ops(), 0);
}

#[test]
fn private_person_typing_never_mutates_the_workspace_document() {
    let mut workspace = AutoCommit::new();
    let before = workspace.get_heads();
    let mut composer = DraftComposer::shared();
    composer.set_conversation_draft(&person("workspace", "certified-person-key"));
    composer.insert_text(&mut ui::widget::Edits::lend(&mut workspace), "Private body");
    assert_eq!(composer.private_plain().as_deref(), Some("Private body"));
    assert_eq!(workspace.get_heads(), before);
    assert_eq!(workspace.pending_ops(), 0);
}

#[test]
fn person_workspaces_retain_distinct_private_editors() {
    let mut workspace = AutoCommit::new();
    let mut composer = DraftComposer::shared();
    let first = person("one", "same-key");
    composer.set_conversation_draft(&first);
    composer.insert_text(
        &mut ui::widget::Edits::lend(&mut workspace),
        "First person draft",
    );
    composer.set_conversation_draft(&person("two", "same-key"));
    assert_eq!(composer.private_plain().as_deref(), Some(""));
    composer.set_conversation_draft(&first);
    assert_eq!(
        composer.private_plain().as_deref(),
        Some("First person draft")
    );
    composer.clear_conversation_draft(&first);
    assert_eq!(composer.private_plain().as_deref(), Some(""));
}

#[test]
fn private_person_editor_survives_retained_surface_store_handoff() {
    let mut workspace = AutoCommit::new();
    let key = person("workspace", "certified-person-key");
    let mut attached = DraftComposer::shared();
    attached.set_conversation_draft(&key);
    attached.insert_text(
        &mut ui::widget::Edits::lend(&mut workspace),
        "Unsent private text",
    );
    let mut floating = DraftComposer::shared();
    floating.share_private_store_from(&attached);
    floating.set_conversation_draft(&key);
    assert_eq!(
        floating.private_plain().as_deref(),
        Some("Unsent private text")
    );
    assert_eq!(attached.private_plain(), floating.private_plain());
    let before = workspace.get_heads();
    floating.insert_text(
        &mut ui::widget::Edits::lend(&mut workspace),
        " and more from the floating surface",
    );
    assert_eq!(attached.private_plain(), floating.private_plain());
    assert!(
        floating
            .private_plain()
            .unwrap()
            .contains("floating surface")
    );
    floating.clear_conversation_draft(&key);
    assert_eq!(attached.private_plain().as_deref(), Some(""));
    assert_eq!(workspace.get_heads(), before);
    assert_eq!(workspace.pending_ops(), 0);
}

/// Ada's and Bob's windows in one workspace, each having introduced themself,
/// the pair's record delivered both ways.
fn a_pair_of_windows(
    workspace: &str,
    ada: &str,
    bob: &str,
) -> (
    ui::framework::testing::TestApp<crate::ChatShell>,
    ui::framework::testing::TestApp<crate::ChatShell>,
) {
    use crate::record::chat::Introduce;

    let mut ada_window =
        ui::framework::testing::TestApp::<crate::ChatShell>::for_person_in(ada, workspace)
            .expect("Ada's window opens");
    let mut bob_window =
        ui::framework::testing::TestApp::<crate::ChatShell>::for_person_in(bob, workspace)
            .expect("Bob's window opens");
    for (window, name, initials, color) in [
        (&mut ada_window, "Ada", "A", 1),
        (&mut bob_window, "Bob", "B", 2),
    ] {
        window
            .invoke(crate::Chat::INTRODUCE.with(Introduce {
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

/// What a window reads of its conversation with `peer`: each message's text,
/// and the person it names as its author.
fn read_conversation(
    window: &mut ui::framework::testing::TestApp<crate::ChatShell>,
    peer: &str,
) -> Vec<(String, String)> {
    use crate::effects::ChatEffect;

    let read = window
        .effect(&ChatEffect::ReadDirectConversation {
            public_key: peer.into(),
        })
        .expect("the window reads the conversation");
    let read = read.value.expect("the conversation as a value");
    read["messages"]
        .as_array()
        .expect("the messages")
        .iter()
        .map(|message| {
            (
                message["text"]
                    .as_str()
                    .expect("a message's text")
                    .to_owned(),
                message["author"]
                    .as_str()
                    .expect("a message's author")
                    .to_owned(),
            )
        })
        .collect()
}

/// Two people, a window each in one workspace, talk through the ordinary
/// record their pair shares: what one's window sends, the other's window
/// reads, and the conversation is the record `direct/<pair>` every app of
/// the workspace opens — Messages' Tilo lane included.
#[test]
fn two_peoples_chat_talks_in_the_pairs_record() {
    use crate::effects::ChatEffect;
    use crate::testing::MountedChat;

    const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000b";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let ada_key = ui::person_chat::person_key(&ada);
    let bob_key = ui::person_chat::person_key(&bob);
    let (mut ada_window, mut bob_window) = a_pair_of_windows(WORKSPACE, &ada, &bob);
    ada_window.with_chat(|chat| {
        assert_eq!(chat.direct_people.len(), 1, "Ada's window names Bob");
        assert_eq!(chat.direct_people[0].root, bob);
    });

    // The conversation is an ordinary record of the workspace, named by the
    // two people in it: the same record Messages' Tilo lane opens.
    let pair = ui::person_chat::direct_pair_id(WORKSPACE, &ada, &bob).expect("a pair");
    let record = format!("direct/{pair}");
    let part = ui::record::RecordId::parse(&record)
        .expect("a record id")
        .part(0);

    bob_window
        .effect(&ChatEffect::OpenDirectConversation {
            public_key: ada_key.clone(),
        })
        .expect("Bob opens the conversation with Ada");
    bob_window.with_chat(|chat| {
        assert!(
            matches!(chat.focus, crate::app::Focus::Main),
            "the conversation opens with its composer ready"
        );
    });
    // What a person's card asks of Chat when they press Message: the action
    // Chat publishes on the row naming their conversation, so the desktop's
    // ask is one Chat answers.
    bob_window.with_chat(|chat| {
        let row = chat
            .person_conversation_rows()
            .into_iter()
            .find(|row| row.id() == format!("chat.person:{ada_key}"))
            .expect("Chat lists the conversation with Ada");
        let resource = row.into_resource();
        assert_eq!(resource.kind, "chat.person", "{:?}", resource.id);
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == ChatEffect::OPEN_DIRECT_CONVERSATION.id),
            "the row offers {}: {:?}",
            ChatEffect::OPEN_DIRECT_CONVERSATION.id,
            resource.actions.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
    });
    let sent = ada_window
        .effect(&ChatEffect::SendDirectMessage {
            public_key: bob_key.clone(),
            text: "hello from Ada".into(),
        })
        .expect("Ada's window sends");
    assert_eq!(
        sent.value
            .as_ref()
            .and_then(|value| value["accepted"].as_bool()),
        Some(true),
        "the pair's record accepts Ada's message: {:?}",
        sent.value
    );
    assert!(
        ada_window
            .shared_record::<ui::direct::DirectConversation>(ui::direct::OWNER, &record)
            .is_some(),
        "the conversation is the ordinary record {record}"
    );

    // Bob's device opens the conversation, and the workspace hands it the
    // part the messages are in.
    assert!(
        bob_window
            .shared_record::<ui::direct::parts::DirectPart>(ui::direct::OWNER, part.as_str())
            .is_some(),
        "Bob's window holds the conversation's part"
    );
    assert!(
        ada_window
            .deliver_record_to(&mut bob_window, &part)
            .expect("Bob takes the part in"),
        "Bob's conversation changed when the part arrived"
    );
    let messages = read_conversation(&mut bob_window, &ada_key);
    assert_eq!(messages.len(), 1, "Bob reads what Ada sent");
    assert_eq!(messages[0].0, "hello from Ada");
    assert_eq!(messages[0].1, ada_key, "the message says Ada wrote it");

    // Bob answers, and Ada reads the answer in the same record.
    let replied = bob_window
        .effect(&ChatEffect::SendDirectMessage {
            public_key: ada_key.clone(),
            text: "hello from Bob".into(),
        })
        .expect("Bob's window sends");
    assert_eq!(
        replied
            .value
            .as_ref()
            .and_then(|value| value["accepted"].as_bool()),
        Some(true)
    );
    assert!(
        ada_window
            .shared_record::<ui::direct::parts::DirectPart>(ui::direct::OWNER, part.as_str())
            .is_some(),
        "Ada's window holds the conversation's part"
    );
    assert!(
        bob_window
            .deliver_record_to(&mut ada_window, &part)
            .expect("Ada takes the part in"),
        "Ada's conversation changed when the part arrived"
    );
    let said: Vec<String> = read_conversation(&mut ada_window, &bob_key)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(
        said,
        ["hello from Ada", "hello from Bob"],
        "both people's messages are in the one conversation"
    );
}
