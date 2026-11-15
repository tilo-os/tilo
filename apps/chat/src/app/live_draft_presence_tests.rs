//! A draft being typed shows to the others in the channel, and only there.

use super::*;
use ui::PresencePayload;
use ui::permissions::MemberId;

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
        direct: false,
        participants: Vec::new(),
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

/// The composer says its person is writing in the conversation while they are: what changes with
/// their typing, never what or where in their draft.
#[test]
fn focused_composer_says_only_that_its_person_is_writing() {
    use ui::presence::capability::{composing_draft, presence_payload};
    let mut app = focused_app("hello team");
    let key = app.composing_key(Focus::Main).expect("the channel is open");
    let (at, typed) = composing_draft(&app).expect("a draft under way");
    assert_eq!(at, key);
    app.composer_main.select(0, 5);
    assert_eq!(composing_draft(&app), Some((key, typed)));
    assert_eq!(
        presence_payload(&app),
        None,
        "no caret in the draft goes out"
    );
    Widget::set_focus(&mut app, false);
    assert_eq!(composing_draft(&app), None, "a composer left is no draft");
    let mut idle = focused_app("");
    assert_eq!(
        composing_draft(&idle),
        None,
        "an empty one is not someone writing"
    );
    Widget::set_focus(&mut idle, false);
    let typed_on = focused_app("hello team!");
    assert_ne!(composing_draft(&typed_on).map(|draft| draft.1), Some(typed));
}

/// Someone writing in the same channel shows under the composer until they stop or go quiet;
/// someone writing in another channel does not.
#[test]
fn someone_writing_here_shows_as_typing() {
    let key = focused_app("").composing_key(Focus::Main).expect("open");
    let (ada, line) = (MemberId::person("ada"), crate::render_chat::typing_line);
    let mut presence = PresenceCtx::empty();
    presence.set_display_name(ada.clone(), "Ada");
    let mut say = |at: &ui::presence::PresenceKey, composing| {
        let payload = PresencePayload::Composing { composing };
        presence.apply_remote_presence(ada.clone(), at.clone(), payload);
        line(&presence, &key)
    };
    assert_eq!(say(&key, true).as_deref(), Some("Ada is typing…"));
    assert_eq!(say(&key, false), None);
    let elsewhere = ui::presence::PresenceKey::resource("so.tilo.chat", "other", "TextChannel");
    assert_eq!(say(&elsewhere, true), None);
    say(&key, true);
    let quiet = ui::time::now() + ui::presence::COMPOSING_LINGER;
    ui::time::testing::with_monotonic_time(quiet, || {
        assert_eq!(line(&presence, &key), None, "someone gone quiet drops out");
    });
}
