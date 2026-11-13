//! What an app publishes and the session reads back: the summary survives the wire, and an activity the
//! app only half describes still shows.

use super::*;

#[test]
fn an_activity_reads_back_from_the_json_an_app_publishes() {
    let published = Published {
        summary: Some(
            Summary::new(Kind::Timer, "Tea", "4:32")
                .with_id("timer-1")
                .with_countdown(272)
                .with_progress(0.25)
                .with_tone(Tone::Alert)
                .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
                .with_control(Control::new("add-minute", "+1 min", ControlKind::Secondary))
                .with_control(Control::new("close", "End", ControlKind::Close)),
        ),
        overlay: true,
        announce: Some(Announcement {
            id: "due-1".into(),
            title: "Tea is ready".into(),
            body: String::new(),
        }),
    };
    let json = serde_json::to_value(&published).expect("it serialises");
    let back: Published = serde_json::from_value(json).expect("it reads back");
    assert_eq!(back, published);
    let summary = back.summary.expect("the activity");
    assert_eq!(summary.reading(), "Tea  4:32");
    assert_eq!(summary.pill_rank(), (2, 272, "timer-1"));
}

#[test]
fn an_activity_that_names_only_a_title_still_shows() {
    let summary: Summary =
        serde_json::from_value(serde_json::json!({"id": "watch", "title": "Stopwatch"}))
            .expect("a minimal activity");
    assert_eq!(summary.kind, Kind::Other);
    assert_eq!(summary.tone, Tone::Neutral);
    assert_eq!(summary.value, "");
    assert_eq!(summary.reading(), "Stopwatch");
    assert!(summary.controls.is_empty());
    assert!(summary.progress.is_none());
}

#[test]
fn a_due_timer_announces_alone() {
    let published = Published::announcing(Announcement {
        id: "due-2".into(),
        title: String::new(),
        body: String::new(),
    });
    let json = serde_json::to_value(&published).expect("it serialises");
    assert_eq!(
        json,
        serde_json::json!({"announce": {"id": "due-2", "title": "", "body": ""}, "overlay": false}),
        "an ended activity leaves no summary on the wire"
    );
}

#[test]
fn the_pill_rank_puts_a_timer_due_within_a_minute_before_other_timers() {
    let due = Summary::new(Kind::Timer, "Tea", "0:40").with_countdown(40);
    let later = Summary::new(Kind::Timer, "Bread", "9:00").with_countdown(540);
    let call = Summary::new(Kind::Call, "Ada", "");
    assert!(call.pill_rank() < due.pill_rank());
    assert!(due.pill_rank() < later.pill_rank());
    assert!(later.pill_rank() < Summary::new(Kind::Music, "Song", "").pill_rank());
}

/// A card headed with a picture keeps its title under it, and one whose picture is a tile keeps the
/// reading in its hero: what an app says of a screenshot is not what it says of a song.
#[test]
fn a_card_headed_with_a_picture_says_so_in_the_json_it_publishes() {
    let hero =
        Summary::new(Kind::Other, "Screenshot", "").with_hero_artwork("data:image/jpeg;base64,AA");
    let published = serde_json::to_value(&hero).expect("a summary writes");
    assert_eq!(
        published.get("art").and_then(serde_json::Value::as_str),
        Some("hero")
    );
    let tile = Summary::new(Kind::Music, "A song", "").with_artwork("data:image/jpeg;base64,AA");
    assert_eq!(tile.art, Art::Square);
    let old: Summary = serde_json::from_value(serde_json::json!({
        "id": "song-1",
        "title": "A song",
        "artwork": "data:image/jpeg;base64,AA"
    }))
    .expect("an activity of an older app reads back");
    assert_eq!(old.art, Art::Square);
}

#[test]
fn a_recordings_stop_is_the_control_that_warns() {
    let recording = Summary::new(Kind::Stopwatch, "Screen recording", "0:07")
        .with_control(Control::new("stop", "Stop", ControlKind::Destructive).with_icon("square"))
        .with_control(Control::new("pause", "Pause", ControlKind::Secondary).with_icon("pause"))
        .with_control(Control::new("stop", "Stop", ControlKind::Close));
    let offered = recording.action_controls();
    assert_eq!(
        offered.len(),
        2,
        "the close is the card's own, not a row control"
    );
    assert_eq!(offered[0].kind, ControlKind::Destructive);
    assert_eq!(offered[1].kind, ControlKind::Secondary);
}
