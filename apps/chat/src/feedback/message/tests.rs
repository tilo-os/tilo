use automerge::AutoCommit as Doc;
use ui::feedback::{
    Destination, FrameTimings, Hidden, Interaction, Outcome, Part, ReviewedFeedback,
    ReviewedSelection, ReviewedText, ScreenshotRef, Submission, TextKind,
};
use ui::tilo_resource::Id;

use crate::resource::Chat;
use crate::testing::{apply_as, submit_feedback};

fn text(kind: TextKind, parts: &[(&str, Option<Hidden>)]) -> ReviewedText {
    ReviewedText {
        kind,
        parts: parts
            .iter()
            .map(|(text, hidden)| Part {
                text: if hidden.is_some() {
                    String::new()
                } else {
                    (*text).to_owned()
                },
                hidden: *hidden,
            })
            .collect(),
    }
}

fn selection(app_name: &str, path: &str, role: &str) -> ReviewedSelection {
    ReviewedSelection {
        app: "so.tilo.files".into(),
        app_name: app_name.into(),
        build: None,
        path: path.into(),
        role: role.into(),
        kind: None,
        record: None,
        label: None,
        texts: Vec::new(),
        note: None,
        crop: None,
    }
}

fn interaction(at: i64, action: &str, resource: &str, outcome: Outcome) -> Interaction {
    Interaction {
        at,
        app: "so.tilo.files".into(),
        action: action.into(),
        resource: resource.into(),
        label: (action == "save").then(|| "Save the file".into()),
        outcome,
    }
}

/// A report of two picks filed as `visitor:one`, with no summary, the
/// reporter's last actions, its frame timings and, with `picture`, its
/// masked picture.
fn picked(picture: bool) -> (Doc, Submission) {
    let mut doc = Doc::new();
    crate::demo::seed(&mut doc).unwrap();
    let channel = crate::testing::channels(&doc)
        .into_iter()
        .find(|channel| channel.kind != crate::model::ChannelKind::Voice)
        .unwrap();
    let save = ReviewedSelection {
        label: Some(text(TextKind::Interface, &[("Save", None)])),
        note: Some(text(
            TextKind::Content,
            &[
                ("Nothing happens when ", None),
                ("", Some(Hidden::Name)),
                (" clicks it", None),
            ],
        )),
        ..selection("Files", "Toolbar[0]/Button[2]", "Button")
    };
    let row = ReviewedSelection {
        texts: vec![
            text(TextKind::Interface, &[("Modified", None)]),
            text(TextKind::Content, &[("", Some(Hidden::Personal))]),
        ],
        ..selection("", "List[0]/Row[4]", "Row")
    };
    let submission = Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: Destination {
            room: "room".into(),
            provider: "demo-chat".into(),
            channel: channel.id,
        },
        feedback: ReviewedFeedback {
            text: String::new(),
            context: Vec::new(),
            screenshot: picture.then(|| ScreenshotRef {
                hash: "ab".repeat(32),
                width: 640,
                height: 360,
            }),
            selections: vec![save, row],
            interactions: vec![
                interaction(1, "open", "files.folder", Outcome::Done),
                interaction(2, "save", "files.file", Outcome::Refused),
            ],
            timings: Some(FrameTimings {
                frames: vec![8_000, 40_000, 9_000, 52_000],
                budget: 16_000,
            }),
        },
    };
    apply_as(&mut doc, "visitor:one", submit_feedback(&submission)).unwrap();
    (doc, submission)
}

/// The rows `submission`'s channel shows for it.
fn rows(doc: &Doc, submission: &Submission) -> Vec<crate::model::Message> {
    crate::projection::read(doc)
        .message_rows(
            &crate::messages::Messages::default(),
            &submission.destination.channel,
        )
        .into_iter()
        .filter(|row| row.id == crate::feedback::message_id(&submission.id))
        .collect()
}

#[test]
fn a_picked_report_reads_out_its_picks_notes_actions_and_timings() {
    let (doc, submission) = picked(false);
    let rows = rows(&doc, &submission);
    assert_eq!(rows.len(), 1, "one message for the report");
    let row = &rows[0];
    assert_eq!(row.author, "visitor:one");
    let lines: Vec<&str> = row.text.lines().collect();
    assert_eq!(
        lines,
        [
            "1. The \u{201C}Save\u{201D} button in Files",
            "\u{201C}Nothing happens when \u{2039}name\u{203A} clicks it\u{201D}",
            "2. A row",
            "Modified",
            "\u{2039}personal\u{203A}",
            "Just before: open on files.folder, Save the file (refused)",
            "Slow: 2 of the last 4 frames over 16 ms, the slowest 52 ms",
        ]
    );
    assert_eq!(row.body.len(), lines.len(), "a paragraph per line");
    assert!(row.attachment.is_none());
    // The record keeps the structured report the person reviewed, not the
    // sentences read out of it.
    assert_eq!(
        crate::feedback::report(&doc, &submission.id)
            .unwrap()
            .unwrap()
            .submission,
        submission
    );
}

#[test]
fn a_pick_from_an_app_with_no_known_name_is_the_part_alone_never_the_apps_id() {
    let (doc, submission) = picked(false);
    let text = rows(&doc, &submission)[0].text.clone();
    assert!(text.lines().any(|line| line == "2. A row"), "{text}");
    assert!(
        !text.contains("so.tilo.files"),
        "the id only routes: {text}"
    );
}

#[test]
fn the_masked_picture_is_the_report_messages_image() {
    let (doc, submission) = picked(true);
    assert_eq!(
        rows(&doc, &submission)[0].attachment,
        Some(crate::model::Attachment::Image {
            name: super::PICTURE_NAME.into(),
            blob: "ab".repeat(32),
            w: 640,
            h: 360,
        })
    );
}

#[test]
fn members_react_to_a_report_as_to_any_message() {
    let (mut doc, submission) = picked(false);
    let report = Id::from_raw(submission.id.clone());
    let react = || {
        Chat::paths()
            .feedback
            .item(&report)
            .reactions
            .react("\u{1F44D}")
            .into()
    };
    apply_as(&mut doc, "visitor:two", react()).unwrap();
    apply_as(&mut doc, "visitor:three", react()).unwrap();
    let rows = rows(&doc, &submission);
    assert_eq!(rows.len(), 1, "reacting adds no second message");
    assert_eq!(rows[0].reactions.len(), 1);
    assert_eq!(rows[0].reactions[0].emoji, "\u{1F44D}");
    assert_eq!(rows[0].reactions[0].count(), 2);
}

#[test]
fn a_bot_reading_the_room_reads_the_report_as_its_message() {
    let (doc, submission) = picked(true);
    let chat = crate::projection::read(&doc);
    let message = crate::feedback::message_id(&submission.id);
    let none = crate::messages::Messages::default();
    let read = crate::effects::read_room(&chat, &none, Some(&message), None);
    let found = &read["messages"][0];
    assert_eq!(found["id"], message.as_str());
    let value = &found["value"];
    assert!(
        value["text"]
            .as_str()
            .unwrap()
            .starts_with("1. The \u{201C}Save\u{201D} button in Files")
    );
    let latest =
        crate::effects::read_room(&chat, &none, None, Some(&submission.destination.channel));
    assert!(
        latest["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|found| found["id"] == message.as_str()),
        "a channel's latest messages include the report"
    );
}
