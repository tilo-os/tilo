use serde_json::json;
use ui::framework::testing::TestApp;

use super::*;
use crate::ChatShell;
use crate::effects::ChatEffect;
use crate::testing::chain;

fn app() -> TestApp<ChatShell> {
    TestApp::new().expect("Chat opens")
}

fn deliver(app: &mut TestApp<ChatShell>, arguments: serde_json::Value) {
    app.call_on(&chain(), "deliver-message", arguments)
        .expect("the delivery is admitted");
}

/// `channel`'s messages as the record's parts hold them, each by its id.
fn held(app: &mut TestApp<ChatShell>, channel: &str) -> Vec<(String, crate::record::Message)> {
    crate::testing::mounted_messages(app)
        .iter()
        .filter(|(_, message)| message.channel.as_str() == channel)
        .map(|(id, message)| (id.as_str().to_owned(), message.clone()))
        .collect()
}

fn a_channel_made_under_a_chosen_id_takes_posts_there_and_is_made_once() {
    let mut app = app();
    app.call(
        "create-channel-with-id",
        json!({"id": "launch-workspace", "name": "launch"}),
    )
    .expect("the channel is made");
    // Naming it again leaves it as it is.
    app.call(
        "create-channel-with-id",
        json!({"id": "launch-workspace", "name": "renamed"}),
    )
    .expect("a repeated step is harmless");
    let chat = app.state();
    let made = chat
        .channels
        .get(&Id::from_raw("launch-workspace"))
        .expect("under the id given");
    assert_eq!(made.name, "launch");
    assert!(
        app.call(
            "create-channel-with-id",
            json!({"id": "no spaces", "name": "x"})
        )
        .is_err()
    );
    crate::testing::mounted_post(&mut app, "launch-workspace", "first post");
    assert_eq!(held(&mut app, "launch-workspace")[0].1.text, "first post");
}

fn a_delivered_message_shows_its_authors_name_to_readers_and_stays_the_deliverers() {
    let mut app = app();
    deliver(
        &mut app,
        json!({"channel": "c_chat", "author": "Dana from Support", "body": "The fix is live",
               "ts": 1_700_000_000}),
    );
    let id = delivered_id(
        &Id::from_raw("c_chat"),
        "Dana from Support",
        1_700_000_000,
        "The fix is live",
    );
    let (_, message) = held(&mut app, "c_chat")
        .into_iter()
        .find(|(held, _)| held == id.as_str())
        .expect("the delivered message is held under the id its delivery derives");
    assert_eq!(
        shown_author(&message, &|id: &str| id.to_owned()),
        "Dana from Support"
    );
    assert_eq!(message.ts, 1_700_000_000);
    assert_ne!(
        message.author.0, "Dana from Support",
        "the author stays whoever delivered it"
    );
}

fn delivering_the_same_message_at_the_same_time_again_changes_nothing() {
    let mut app = app();
    let delivery = json!({"channel": "c_chat", "author": "Dana", "body": "hello",
                          "ts": 1_700_000_000});
    deliver(&mut app, delivery.clone());
    deliver(&mut app, delivery);
    let count = held(&mut app, "c_chat")
        .iter()
        .filter(|(_, message)| message.text == "hello")
        .count();
    assert_eq!(count, 1);
}

fn a_delivery_to_a_voice_channel_or_without_an_author_is_refused() {
    let mut app = app();
    assert!(
        app.call_on(
            &chain(),
            "deliver-message",
            json!({"channel": "c_voice", "author": "Dana", "body": "hi"})
        )
        .is_err()
    );
    assert!(
        app.call_on(
            &chain(),
            "deliver-message",
            json!({"channel": "c_chat", "author": " ", "body": "hi"})
        )
        .is_err()
    );
}

fn export_content_keeps_the_channels_transcript_as_content() {
    let mut app = app();
    deliver(
        &mut app,
        json!({"channel": "c_chat", "author": "Dana", "body": "Release at noon",
               "ts": 1_700_000_000}),
    );
    let output = app
        .effect(&ChatEffect::ExportContent {
            channel: "c_chat".into(),
            format: None,
        })
        .expect("the transcript is kept");
    let value = output.value.expect("an answer");
    assert_eq!(value["format"], "md");
    let address = value["content"].as_str().unwrap().to_owned();
    assert_eq!(output.resources, vec![address.clone()]);
    let hash = ui::tilo_actions::content_hash(&address).unwrap();
    let markdown = String::from_utf8(app.content().read(hash).unwrap()).unwrap();
    assert!(markdown.starts_with("# #chat\n"), "{markdown}");
    assert!(
        markdown.contains("- **Dana** (2023-11-14T22:13:20Z): Release at noon"),
        "{markdown}"
    );

    let json_value = app
        .effect(&ChatEffect::ExportContent {
            channel: "c_chat".into(),
            format: Some("json".into()),
        })
        .unwrap()
        .value
        .unwrap();
    let hash = ui::tilo_actions::content_hash(json_value["content"].as_str().unwrap())
        .unwrap()
        .to_owned();
    let exported: serde_json::Value =
        serde_json::from_slice(&app.content().read(&hash).unwrap()).unwrap();
    let last = exported["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["authorName"], "Dana");
    assert_eq!(last["date"], "2023-11-14T22:13:20Z");
    assert!(
        app.effect(&ChatEffect::ExportContent {
            channel: "nowhere".into(),
            format: None,
        })
        .is_err()
    );
    assert!(
        app.effect(&ChatEffect::ExportContent {
            channel: "c_chat".into(),
            format: Some("pdf".into()),
        })
        .is_err()
    );
}

/// A bridge's deliveries into a channel, and a channel's exported transcript.
/// Each case was a test of its own, on a fixture of its own; a failure
/// names the cases that failed.
#[test]
fn deliveries_and_exports_go_through_the_chats_own_actions() {
    crate::testing::cases!(
        a_channel_made_under_a_chosen_id_takes_posts_there_and_is_made_once,
        a_delivered_message_shows_its_authors_name_to_readers_and_stays_the_deliverers,
        delivering_the_same_message_at_the_same_time_again_changes_nothing,
        a_delivery_to_a_voice_channel_or_without_an_author_is_refused,
        export_content_keeps_the_channels_transcript_as_content,
    );
}

fn delivered_ids_derive_from_what_the_delivery_says() {
    let channel = Id::from_raw("c_chat");
    let id = delivered_id(&channel, "Dana", 5, "hi");
    assert_eq!(id, delivered_id(&channel, " Dana ", 5, "hi"));
    assert_ne!(id, delivered_id(&channel, "Dana", 6, "hi"));
    assert_ne!(id, delivered_id(&channel, "Dana", 5, "hi!"));
}

fn rfc3339_prints_unix_seconds_in_utc() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z", "before the epoch");
    assert_eq!(
        rfc3339(86_399),
        "1970-01-01T23:59:59Z",
        "a day's last second"
    );
    assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
}

/// The id a delivery derives and the time a transcript prints.
/// Each case was a test of its own, on a fixture of its own; a failure
/// names the cases that failed.
#[test]
fn delivered_ids_and_times_are_derived_the_same_everywhere() {
    crate::testing::cases!(
        delivered_ids_derive_from_what_the_delivery_says,
        rfc3339_prints_unix_seconds_in_utc,
    );
}
