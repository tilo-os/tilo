//! One person is one colour: an app says the colour its person chose where it
//! says who they are, and every person a member list names is painted in the
//! colour that list gives them.

use super::*;

use crate::permissions::UserId;
use crate::presence::identity_rgb;
use crate::tokens::PresenceHue;

/// Settings holding exactly `document`, for this test's thread.
fn settings(document: serde_json::Value) -> settings_store::SourceScope {
    let serde_json::Value::Object(document) = document else {
        unreachable!("a settings document is an object")
    };
    settings_store::scoped_source(Some(std::sync::Arc::new(
        settings_store::HostDocument::new(document, |_| Ok(())),
    )))
}

fn app() -> App {
    App::new(State::new(), Box::new(crate::widget::Text::new("t")))
}

/// The colour the person chose is announced where the app says who it is, so
/// their own face, caret and byline wear it from the first frame — and their
/// windows take it too.
#[test]
fn an_app_announces_the_colour_its_person_chose() {
    let _settings = settings(serde_json::json!({"identity": {"colour": "violet"}}));
    let app = app();
    let me = app.local_author().clone();
    assert_eq!(app.presence_ctx.hue_for(&me), PresenceHue::Violet);
    assert_eq!(
        app.presence_ctx.color_for(&me),
        identity_rgb(PresenceHue::Violet)
    );
    assert_eq!(
        crate::local_identity::own_colour(),
        Some(PresenceHue::Violet),
        "the windows' accent is the one colour as well"
    );
}

/// A person who chose none keeps the colour their identity derives, which is
/// what every other surface derives for them unsaid.
#[test]
fn a_person_who_chose_none_keeps_the_colour_their_identity_derives() {
    let _settings = settings(serde_json::json!({}));
    let app = app();
    let me = app.local_author().clone();
    assert_eq!(
        app.presence_ctx.hue_for(&me),
        PresenceHue::of_identity(&me.0)
    );
    assert_eq!(crate::local_identity::own_colour(), None);
}

/// The colour the member list announces is the one the app paints that person
/// in, for their face, their caret and their byline alike.
#[test]
fn an_app_paints_a_person_in_the_colour_the_member_list_announces() {
    let _settings = settings(serde_json::json!({}));
    let mut app = app();
    app.apply_collab_context(vec![collab_types::Member::of_person(
        &collab_types::Person::new("bo", "Bo").wearing(PresenceHue::Teal),
        None,
    )]);
    assert_eq!(app.presence_ctx.hue_of("bo"), PresenceHue::Teal);
    assert_eq!(
        app.presence_ctx.color_for(&UserId("bo".into())),
        identity_rgb(PresenceHue::Teal)
    );
    assert_eq!(
        app.presence_ctx.hue_of("ada"),
        PresenceHue::of_identity("ada"),
        "someone the list names no colour for keeps their identity's"
    );
}
