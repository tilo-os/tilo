//! What an app publishes and the session reads back: the summary survives the
//! wire, and an activity the app only half describes still shows.

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
