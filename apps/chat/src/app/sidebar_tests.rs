//! What the sidebar lists: the workspace's channels first, then the people the
//! person has a direct conversation with, each once and by name.

use super::*;
use tilo_actions::policy::MemberId;

/// A direct conversation row as the record projects it, named `name`.
fn conversation(app: &ChatApp, key: &str, name: &str) -> crate::model::Channel {
    let id = crate::record::Chat::direct_id([
        MemberId::person(app.me.id.as_str()),
        MemberId::person(key),
    ]);
    crate::model::Channel {
        id: id.to_string(),
        category: String::new(),
        name: name.into(),
        topic: String::new(),
        kind: crate::model::ChannelKind::Text,
        order: 0,
        unread: 0,
        mentions: 0,
        archived: false,
        direct: true,
        participants: vec![
            MemberId::person(app.me.id.as_str()).key(),
            MemberId::person(key).key(),
        ],
    }
}

fn app_in_workspace() -> ChatApp {
    let doc = crate::testing::demo_doc();
    let mut app = ChatApp::new_ephemeral();
    app.attach_workspace(&doc);
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

/// A workspace full of visitors (a public lounge) is not a list of direct
/// messages: the channels open the sidebar, and only the people this
/// person writes with follow, below them, in their own section.
#[test]
fn channels_open_the_sidebar_and_only_conversations_follow() {
    let mut app = app_in_workspace();
    let me = app.me.id.clone();
    let key = |i: u64| format!("{i:064x}");
    let row = |i: u64, name: &str| {
        let id = crate::record::Chat::direct_id([
            MemberId::person(me.clone()),
            MemberId::person(key(i)),
        ]);
        crate::model::Channel {
            id: id.to_string(),
            category: String::new(),
            name: name.into(),
            topic: String::new(),
            kind: crate::model::ChannelKind::Text,
            order: 0,
            unread: 0,
            mentions: 0,
            archived: false,
            direct: true,
            participants: Vec::new(),
        }
    };
    let ada = row(100, "Ada");
    app.channels.extend((0..25).map(|i| row(i, "")));
    app.channels.push(row(101, "Swift Lynx"));
    app.direct_held.insert(ada.id.clone());
    app.channels.push(ada);
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
        .last()
        .expect("sections")
        .items
        .iter()
        .map(|item| item.label.clone())
        .collect();
    assert_eq!(people, ["Ada"], "only whom they write with");
    let last = sections.last().expect("sections");
    assert_eq!(labels(last), ["Ada"], "direct messages close the list");
    assert_eq!(last.title, crate::i18n::t("chat-direct-messages-section"));
}

/// The conversation open now is listed even before its first message,
/// and a person the workspace has no name for goes by their tag, never blank.
#[test]
fn the_open_conversation_is_listed_by_a_name_never_blank() {
    let mut app = app_in_workspace();
    let key = format!("{:064x}", 7);
    let row = conversation(&app, &key, &crate::identity::unnamed_label(&key));
    app.active_channel = row.id.clone();
    app.channels.push(row);
    app.show_sidebar();

    let people: Vec<_> = shown(&app)
        .last()
        .expect("the direct messages section")
        .items
        .iter()
        .map(|item| item.label.clone())
        .collect();
    assert_eq!(people, [crate::identity::unnamed_label(&key)]);
    assert!(!people[0].trim().is_empty());
    assert_ne!(people[0], key, "never the raw key");
}
