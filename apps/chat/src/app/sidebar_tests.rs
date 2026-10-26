//! What the sidebar lists: the room's channels first, then the people the
//! person has a direct conversation with, each once and by name.

use super::*;

fn contact(key: &str, name: &str, conversation: bool) -> ui::person_chat::Contact {
    ui::person_chat::Contact {
        public_key: key.into(),
        display_name: name.into(),
        conversation,
    }
}

fn app_in_room() -> ChatApp {
    let doc = crate::testing::demo_doc();
    let mut app = ChatApp::new_ephemeral();
    app.attach_room(&doc);
    app.reload(&doc);
    app
}

fn shown(app: &ChatApp) -> Vec<ui::widget::NavSection> {
    app.places
        .shown()
        .map(|(sections, _)| sections)
        .unwrap_or_default()
}

fn labels(section: &ui::widget::NavSection) -> Vec<String> {
    section
        .items
        .iter()
        .map(|item| item.label.clone())
        .collect()
}

/// A room full of visitors (a public lounge) is not a list of direct
/// messages: the channels open the sidebar, and only the people this
/// person writes with follow, below them, in their own section.
#[test]
fn channels_open_the_sidebar_and_only_conversations_follow() {
    let mut app = app_in_room();
    let mut contacts: Vec<_> = (0..25)
        .map(|i| contact(&format!("{i:064x}"), "", false))
        .collect();
    contacts.push(contact(&format!("{:064x}", 100), "Ada", true));
    contacts.push(contact(&format!("{:064x}", 101), "Swift Lynx", false));
    app.person_contacts = contacts;
    app.show_sidebar();

    let sections = shown(&app);
    let first = sections
        .iter()
        .find(|section| !section.items.is_empty())
        .expect("the sidebar lists something");
    assert!(
        first
            .items
            .iter()
            .all(|item| item.id.starts_with("channel:")),
        "the channels come first: {:?}",
        labels(first)
    );
    let people: Vec<_> = sections
        .iter()
        .flat_map(|section| &section.items)
        .filter(|item| item.id.starts_with("person:"))
        .map(|item| item.label.clone())
        .collect();
    assert_eq!(people, ["Ada"], "only whom they write with");
    let last = sections.last().expect("sections");
    assert_eq!(labels(last), ["Ada"], "direct messages close the list");
    assert_eq!(last.title, crate::i18n::t("chat-direct-messages-section"));
}

/// The conversation open now is listed even before its first message,
/// and a person the room has no name for goes by their tag, never blank.
#[test]
fn the_open_conversation_is_listed_by_a_name_never_blank() {
    let mut app = app_in_room();
    let key = format!("{:064x}", 7);
    app.person_contacts = vec![contact(&key, "", false)];
    app.direct_person = Some(key.clone());
    app.show_sidebar();

    let people: Vec<_> = shown(&app)
        .iter()
        .flat_map(|section| section.items.clone())
        .filter(|item| item.id.starts_with("person:"))
        .map(|item| item.label)
        .collect();
    assert_eq!(people, [crate::identity::unnamed_label(&key)]);
    assert!(!people[0].trim().is_empty());
}
