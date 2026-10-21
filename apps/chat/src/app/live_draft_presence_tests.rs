//! A draft being typed shows to the others in the channel, and only there.

use super::*;
use ui::PresencePayload;
use ui::permissions::UserId;

fn focused_app(text: &str) -> ChatApp {
    let mut app = ChatApp::new_ephemeral();
    app.active_channel = "c1a2b3c4-0000-4000-8000-00000000c0de".into();
    app.channels = vec![crate::model::Channel {
        id: app.active_channel.clone(),
        category: String::new(),
        name: "general".into(),
        topic: String::new(),
        kind: crate::model::ChannelKind::Text,
        order: 0,
        unread: 0,
        mentions: 0,
        archived: false,
        conversation_with: None,
    }];
    app.rect = Rect::new(0.0, 0.0, 1100.0, 720.0);
    let mut doc = AutoCommit::new();
    app.composer_main.set_key(&app.active_channel);
    app.composer_main.ensure_draft(&doc);
    app.composer_main
        .insert_text(&mut ui::widget::Edits::lend(&mut doc), text);
    app.composer_main.sync(&doc);
    Widget::set_focus(&mut app, true);
    app
}

/// The composer tells the room that its person is writing there, while
/// they are, and never what or where in their draft.
#[test]
fn focused_composer_publishes_only_that_its_person_is_writing() {
    let mut app = focused_app("hello team");
    let key = ChatApp::draft_presence_key(&app.active_channel);
    app.composer_main.select(5, 5);
    assert_eq!(
        ui::presence::capability::presence_payload(&app),
        Some((key.clone(), PresencePayload::Caret { pos: 0 }))
    );
    app.composer_main.select(0, 5);
    assert_eq!(
        ui::presence::capability::presence_payload(&app),
        Some((key, PresencePayload::Caret { pos: 0 })),
        "a selection in the draft is not the room's either"
    );
    Widget::set_focus(&mut app, false);
    assert_eq!(ui::presence::capability::presence_payload(&app), None);
    let mut idle = focused_app("");
    assert_eq!(
        ui::presence::capability::presence_payload(&idle),
        None,
        "an empty composer is not someone writing"
    );
    Widget::set_focus(&mut idle, false);
}

/// Someone writing in the same channel shows under the composer; someone
/// writing in another channel does not.
#[test]
fn someone_writing_here_shows_as_typing() {
    let app = focused_app("hello team");
    let key = ChatApp::draft_presence_key(&app.active_channel);
    assert_eq!(
        crate::render_chat::typing_line(&PresenceCtx::empty(), &key),
        None
    );
    let mut presence = PresenceCtx::empty();
    presence.set_display_name(UserId("ada".into()), "Ada");
    presence.set_caret(&key, UserId("ada".into()), 0);
    assert_eq!(
        crate::render_chat::typing_line(&presence, &key).as_deref(),
        Some("Ada is typing…")
    );
    let mut elsewhere = PresenceCtx::empty();
    elsewhere.set_caret(
        &ChatApp::draft_presence_key("other"),
        UserId("ada".into()),
        0,
    );
    assert_eq!(crate::render_chat::typing_line(&elsewhere, &key), None);
}
