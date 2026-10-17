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

#[test]
fn agent_draft_keys_keep_the_existing_route_encoding() {
    assert_eq!(
        ConversationKey::Agent {
            workspace: "room".into(),
            route: "task:review:7".into(),
        }
        .draft_key(),
        "agent:4:room:13:task:review:7"
    );
}

#[test]
fn accepted_person_send_cannot_clear_a_newer_editor_revision() {
    let mut room = AutoCommit::new();
    let before = room.get_heads();
    let key = person("workspace", "peer");
    let mut composer = DraftComposer::shared();
    composer.set_conversation_draft(&key);
    composer.insert_text(&mut ui::widget::Edits::lend(&mut room), "First");
    let sent_revision = composer.conversation_revision(&key).unwrap();
    composer.insert_text(&mut ui::widget::Edits::lend(&mut room), " more");
    assert!(!composer.clear_conversation_revision(&key, sent_revision));
    assert_eq!(composer.private_plain().as_deref(), Some("First more"));
    let current_revision = composer.conversation_revision(&key).unwrap();
    assert!(composer.clear_conversation_revision(&key, current_revision));
    assert_eq!(composer.private_plain().as_deref(), Some(""));
    assert_eq!(room.get_heads(), before);
    assert_eq!(room.pending_ops(), 0);
}

#[test]
fn private_person_typing_never_mutates_the_room_document() {
    let mut room = AutoCommit::new();
    let before = room.get_heads();
    let mut composer = DraftComposer::shared();
    composer.set_conversation_draft(&person("workspace", "certified-person-key"));
    composer.insert_text(&mut ui::widget::Edits::lend(&mut room), "Private body");
    assert_eq!(composer.private_plain().as_deref(), Some("Private body"));
    assert_eq!(room.get_heads(), before);
    assert_eq!(room.pending_ops(), 0);
}

#[test]
fn person_workspace_and_agent_routes_retain_distinct_private_editors() {
    let mut room = AutoCommit::new();
    let mut composer = DraftComposer::shared();
    let first = person("one", "same-key");
    composer.set_conversation_draft(&first);
    composer.insert_text(
        &mut ui::widget::Edits::lend(&mut room),
        "First person draft",
    );
    composer.set_conversation_draft(&person("two", "same-key"));
    assert_eq!(composer.private_plain().as_deref(), Some(""));
    composer.set_agent_draft("one", "same-key");
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
    let mut room = AutoCommit::new();
    let key = person("workspace", "certified-person-key");
    let mut attached = DraftComposer::shared();
    attached.set_conversation_draft(&key);
    attached.insert_text(
        &mut ui::widget::Edits::lend(&mut room),
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
    let before = room.get_heads();
    floating.insert_text(
        &mut ui::widget::Edits::lend(&mut room),
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
    assert_eq!(room.get_heads(), before);
    assert_eq!(room.pending_ops(), 0);
}
