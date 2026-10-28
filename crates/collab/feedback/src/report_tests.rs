use super::*;

fn text(kind: TextKind, text: &str) -> PickedText {
    PickedText {
        kind,
        text: text.into(),
        area: None,
    }
}

/// A chat message row: who wrote it, what they wrote, and the app's own
/// "Reply" label.
fn message_pick() -> Pick {
    Pick {
        app: "so.tilo.chat".into(),
        app_name: "Chat".into(),
        build: Some("54f21f7ad9".into()),
        path: "BrowserShell/DesktopSurface/Chat/MessageRow[3]".into(),
        role: "ListItem".into(),
        kind: None,
        caption: String::new(),
        label: None,
        record: Some("chat.workspace".into()),
        area: Area {
            x: 100.0,
            y: 200.0,
            width: 400.0,
            height: 60.0,
        },
        texts: vec![
            text(TextKind::Content, "Ada Lovelace"),
            text(TextKind::Content, "Thanks Ada, mail me at ada@example.org"),
            text(TextKind::Interface, "Reply"),
        ],
    }
}

fn draft() -> ReportDraft {
    ReportDraft {
        picks: vec![message_pick()],
        notes: vec!["This row overlaps the next one".into()],
        summary: "The timeline jumps".into(),
        names: vec!["Ada Lovelace".into()],
        ..ReportDraft::default()
    }
}

fn shown(review: &ReportReview) -> String {
    serde_json::to_string(&review.feedback).unwrap()
}

#[test]
fn content_is_hidden_by_default_and_interface_copy_is_shown() {
    let review = draft().review(None).unwrap();
    let json = shown(&review);
    assert!(!json.contains("Ada"), "{json}");
    assert!(!json.contains("example.org"), "{json}");
    let selection = &review.feedback.selections[0];
    assert_eq!(selection.texts[2].plain(), "Reply");
    assert_eq!(selection.texts[0].plain(), "\u{2039}personal\u{203A}");
    assert_eq!(
        selection.note.as_ref().unwrap().plain(),
        "This row overlaps the next one"
    );
    assert_eq!(review.feedback.text, "The timeline jumps");
    assert_eq!(selection.app, "so.tilo.chat");
    assert_eq!(selection.build.as_deref(), Some("54f21f7ad9"));
    assert_eq!(selection.record.as_deref(), Some("chat.workspace"));
    assert!(
        review
            .redactions
            .iter()
            .filter(|redaction| redaction.basis == Basis::Content)
            .all(|redaction| redaction.hidden && redaction.revealable)
    );
}

#[test]
fn showing_content_keeps_names_and_addresses_in_it_hidden() {
    let mut draft = draft();
    let message = draft
        .review(None)
        .unwrap()
        .redactions
        .into_iter()
        .find(|redaction| redaction.value().starts_with("Thanks"))
        .unwrap();
    draft.toggle(&message.key);
    let review = draft.review(None).unwrap();
    let selection = &review.feedback.selections[0];
    assert_eq!(
        selection.texts[1].plain(),
        "Thanks \u{2039}name\u{203A}, mail me at \u{2039}email\u{203A}",
        "the name of someone here and the address stay hidden"
    );
    let json = shown(&review);
    assert!(!json.contains("Ada"), "{json}");
    // The shown message is still listed, to hide it again.
    let listed = review
        .redactions
        .iter()
        .find(|redaction| redaction.key == message.key)
        .unwrap();
    assert!(!listed.hidden);
    draft.toggle(&message.key);
    assert_eq!(
        draft.review(None).unwrap().feedback.selections[0].texts[1].plain(),
        "\u{2039}personal\u{203A}"
    );
}

#[test]
fn a_name_the_reporter_shows_is_sent_and_credentials_never_are() {
    let mut draft = draft();
    draft.summary = "Failed with token=supersecret123 for Ada Lovelace".into();
    let review = draft.review(None).unwrap();
    assert!(!review.feedback.text.contains("supersecret123"));
    assert!(!review.feedback.text.contains("Ada"));
    for redaction in &review.redactions {
        if redaction.pick.is_none() {
            draft.toggle(&redaction.key);
        }
    }
    let review = draft.review(None).unwrap();
    assert!(
        review.feedback.text.contains("Ada Lovelace"),
        "{}",
        review.feedback.text
    );
    assert!(
        !review.feedback.text.contains("supersecret123"),
        "a credential cannot be shown: {}",
        review.feedback.text
    );
}

#[test]
fn the_model_adds_redactions_but_cannot_remove_the_floor() {
    let mut draft = draft();
    let message = draft
        .review(None)
        .unwrap()
        .redactions
        .into_iter()
        .find(|redaction| redaction.value().starts_with("Thanks"))
        .unwrap();
    draft.toggle(&message.key);
    draft.picks[0]
        .texts
        .push(text(TextKind::Content, "Meet at Baker Street 221b"));
    let reveal = Reveal {
        node: "p0.t3".into(),
        value: "Meet at Baker Street 221b".into(),
    };
    draft.toggle(&reveal);
    let request = draft.request().unwrap();
    let (prompt, view) = model_prompt(&request, Exposure::Placeholders);
    assert!(
        !prompt.contains("Lovelace"),
        "a remote model never sees declared names: {prompt}"
    );
    assert!(
        prompt.contains("Baker Street"),
        "shown content is reviewed: {prompt}"
    );
    let index = view
        .iter()
        .position(|text| text.shown.contains("Baker Street"))
        .unwrap();
    let answer = format!(
        "Sure. {{\"hide\":[{{\"text\":{index},\"quote\":\"Baker Street 221b\",\"category\":\"address\"}},{{\"text\":99,\"quote\":\"x\"}}]}}"
    );
    let proposals = model_proposals(&view, &answer).unwrap();
    assert_eq!(proposals.redactions.len(), 1);
    let review = draft.review(Some(proposals)).unwrap();
    let selection = &review.feedback.selections[0];
    assert_eq!(
        selection.texts[3].plain(),
        "Meet at \u{2039}address\u{203A}"
    );
    assert!(
        selection.texts[1].plain().contains("\u{2039}email\u{203A}"),
        "the floor's redactions stand beside the model's"
    );
    assert!(
        review
            .redactions
            .iter()
            .any(|redaction| redaction.basis == Basis::Model && redaction.hidden)
    );
    assert!(model_proposals(&view, "no json here").is_err());
}

#[test]
fn a_quote_into_a_placeholder_is_refused() {
    let draft = draft();
    let request = draft.request().unwrap();
    let (_, view) = model_prompt(&request, Exposure::Placeholders);
    let index = view.iter().position(|text| text.node == "p0.t1").unwrap();
    let answer = format!("{{\"hide\":[{{\"text\":{index},\"quote\":\"name\"}}]}}");
    assert!(
        model_proposals(&view, &answer)
            .unwrap()
            .redactions
            .is_empty()
    );
}

#[test]
fn the_submission_carries_structure_and_the_slow_trace_only_when_asked() {
    let mut draft = draft();
    draft.interactions = vec![Interaction {
        at: 1_700_000_000_000,
        app: "so.tilo.chat".into(),
        action: "post".into(),
        resource: "chat.workspace".into(),
        label: Some("Post a message".into()),
        outcome: Outcome::Done,
    }];
    draft.timings = Some(FrameTimings {
        frames: vec![8_000, 40_000, 9_000],
        budget: 16_666,
    });
    let quiet = draft.review(None).unwrap();
    assert!(quiet.feedback.timings.is_none());
    assert_eq!(quiet.feedback.interactions.len(), 1);
    draft.slow = true;
    let slow = draft.review(None).unwrap();
    assert_eq!(slow.feedback.timings.as_ref().unwrap().slow_frames(), 1);
    let plain = slow.feedback.plain();
    assert!(plain.contains("1. Chat"), "{plain}");
    assert!(plain.contains("Slow: 1 of the last 3 frames"), "{plain}");
    assert!(!plain.contains("Ada"), "{plain}");
    let submission = crate::Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: crate::Destination {
            room: "room".into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "c_feedback".into(),
        },
        feedback: slow.feedback,
    };
    submission.validate().unwrap();
    let back: crate::Submission =
        serde_json::from_str(&serde_json::to_string(&submission).unwrap()).unwrap();
    assert_eq!(back, submission);
    // A report about picks needs no summary.
    draft.summary.clear();
    let mut submission = submission;
    submission.feedback = draft.review(None).unwrap().feedback;
    submission.validate().unwrap();
}

#[test]
fn an_empty_draft_or_too_many_picks_is_refused() {
    assert!(ReportDraft::default().review(None).is_err());
    let mut draft = draft();
    draft.picks = vec![message_pick(); MAX_PICKS + 1];
    assert!(draft.review(None).is_err());
}

#[test]
fn bounding_a_pick_keeps_its_texts_within_the_report() {
    let mut pick = message_pick();
    pick.texts = (0..40)
        .map(|index| text(TextKind::Content, &format!("{index} {}", "x".repeat(400))))
        .collect();
    pick.bound();
    assert!(pick.texts.len() <= MAX_PICK_TEXTS);
    let total: usize = pick
        .texts
        .iter()
        .map(|text| text.text.chars().count())
        .sum();
    assert!(total <= MAX_PICK_CHARS, "{total}");
    let mut draft = draft();
    draft.picks = vec![pick; MAX_PICKS];
    draft.notes = vec!["n".repeat(MAX_NOTE_CHARS); MAX_PICKS];
    draft.summary = "s".repeat(MAX_SUMMARY_CHARS);
    draft
        .review(None)
        .expect("the largest draft fits the review's bounds");
}

#[test]
fn the_path_never_names_content() {
    let mut pick = message_pick();
    pick.path = "Files/Row[Ada Lovelace]".into();
    let review = ReportDraft {
        picks: vec![pick],
        ..ReportDraft::default()
    }
    .review(None)
    .unwrap();
    assert!(!review.feedback.selections[0].path.contains("Ada"));
}

#[test]
fn crops_are_masked_where_content_is_and_stacked_into_one_picture() {
    let crop = |width: u32, height: u32| CropCapture {
        width,
        height,
        rgba: vec![200; usize::try_from(width * height * 4).unwrap()],
        sensitive: vec![crate::PixelRect {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        }],
    };
    let (picture, places) = compose_crops(vec![crop(4, 3), crop(3, 2)]).unwrap();
    assert_eq!(
        places[0],
        CropRef {
            x: 0,
            y: 0,
            width: 4,
            height: 3
        }
    );
    assert_eq!(places[1].y, 3 + 16);
    let pixels = image::load_from_memory_with_format(picture.png(), image::ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    assert_eq!(pixels.dimensions(), (4, 3 + 16 + 2));
    assert_eq!(
        pixels.get_pixel(3, 20).0,
        [0; 4],
        "the ground beside a narrower crop is clear"
    );
    assert_eq!(pixels.get_pixel(1, 1).0, [0x9a, 0xa0, 0xa8, 0xff]);
    assert_eq!(pixels.get_pixel(0, 0).0, [200; 4]);
    let mut outside = crop(3, 2);
    outside.sensitive[0].x = 2;
    assert!(compose_crops(vec![outside]).is_err());
    let mut review = draft().review(None).unwrap();
    attach_crops(&mut review.feedback, picture.reference(), &places);
    assert_eq!(review.feedback.selections[0].crop, Some(places[0]));
}

#[test]
fn a_pick_from_an_app_with_no_known_name_never_reads_out_the_apps_id() {
    let mut draft = draft();
    draft.picks[0].app_name = String::new();
    let plain = draft.review(None).unwrap().feedback.plain();
    assert!(plain.contains("1. "), "{plain}");
    assert!(
        !plain.contains("so.tilo.chat"),
        "the id only routes: {plain}"
    );
}
