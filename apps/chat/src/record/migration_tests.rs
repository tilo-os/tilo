//! Chat records and message parts written before their values were model
//! types (each one JSON string) open as the record and the part, with
//! nothing they held lost.

use ui::automerge::ROOT;
use ui::serde_json::json;
use ui::tilo_resource::{Replica, Slot, evolution};

use crate::agent_stream::AgentFinish;
use crate::model::{AgentReasoning, AgentRoute, AgentStepOutcome, AgentWorkState, Attachment};
use crate::parts::ChatPart;
use crate::record::{Chat, Revision, ShownAs, Transition};

/// `doc` as it was at storage `version`.
fn at_version(doc: &mut ui::tilo_resource::Doc, version: u64) {
    Slot::Key(&ROOT, evolution::VERSION_KEY)
        .put_scalar(doc, ui::automerge::ScalarValue::Uint(version))
        .unwrap();
}

fn submission() -> ui::feedback::Submission {
    ui::feedback::Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: ui::feedback::Destination {
            room: "room".into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "c_feedback".into(),
        },
        feedback: ui::feedback::ReviewedFeedback {
            text: "The sidebar flickers".into(),
            context: Vec::new(),
            screenshot: None,
            ..Default::default()
        },
    }
}

/// A chat record of version 4: a filed report's submission, its triage
/// update and a moderation decision's reports and take-backs as JSON text.
#[test]
fn a_chat_record_of_version_four_opens_with_its_reports_and_decisions() {
    let mut replica = Replica::<Chat>::new();
    let doc = replica.doc_mut();
    evolution::fixture(
        doc,
        &ROOT,
        &json!({
            "feedback": {
                "f1": {
                    "k": "a",
                    "v": {
                        "submission": ui::serde_json::to_string(&Some(submission())).unwrap(),
                        "author": "ada",
                        "ts": 1,
                        "triage": {
                            "t1": {
                                "k": "a",
                                "v": {
                                    "author": "mod",
                                    "ts": 2,
                                    "update": r#"{"status":"investigating"}"#,
                                },
                            },
                        },
                    },
                },
            },
            "moderation": {
                "decisions": {
                    "d1": {
                        "id": "d1",
                        "person": "bob",
                        "reports": r#"["r1"]"#,
                        "outcome": "ban",
                        "reasoning": "spam",
                        "model": "",
                        "reverted": "[]",
                        "by": "mod",
                        "at_ms": 3,
                    },
                },
            },
        }),
    )
    .unwrap();
    at_version(doc, 4);
    doc.commit();
    let mut old = doc.fork();
    assert!(evolution::read_checked::<Chat>(&old).is_err(), "unmigrated");
    assert!(evolution::migrate::<Chat>(&mut old).unwrap());
    let chat = evolution::read_checked::<Chat>(&old).unwrap();
    let report = chat.feedback.values().next().unwrap();
    assert_eq!(report.submission, Some(submission()));
    assert_eq!(
        report.triage.values().next().unwrap().update,
        Some(crate::feedback::TriageUpdate::Investigating)
    );
    let decision = chat.moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.reports, ["r1"]);
    assert!(decision.reverted.is_empty());
}

/// The version-1 chat document holding one message with `body` and every
/// value that version stored as JSON text.
fn version_one_message(body: &[tilo_document::ClipBlock]) -> ui::serde_json::Value {
    json!({
        "messages": {
            "m1": {
                "k": "a",
                "v": {
                    "channel": "c_general",
                    "author": "ada",
                    "ts": 10,
                    "text": "hi @Bo",
                    "body": ui::serde_json::to_string(body).unwrap(),
                    "edited": true,
                    "history": r#"[{"ts":5,"text":"hey"}]"#,
                    "reactions": {},
                    "mentions": r#"[{"user":"u_bo","start":3,"len":3,"is_agent":false}]"#,
                    "reference": "null",
                    "attachment": r#"{"File":{"name":"a.txt","blob":"ff","size":5}}"#,
                    "pinned": false,
                    "appearance": r#"{"name":"Bo"}"#,
                    "work": {
                        "state": r#"{"state":{"state":"taken"},"since":11}"#,
                        "reasoning": r#"{"route":"floor","why":"its words"}"#,
                        "stream": {
                            "run": "run-1",
                            "sequence": 2,
                            "reasoning": "",
                            "progress": "",
                            "finish": r#""complete""#,
                            "detail": "",
                        },
                        "trace": {
                            "s1": {
                                "id": "s1",
                                "sequence": 1,
                                "resource": "notes.page:1",
                                "label": "Rename",
                                "outcome": r#"{"outcome":"refused","why":"no such page"}"#,
                            },
                        },
                    },
                },
            },
        },
    })
}

/// A part of the chat's messages of version 1: a message's paragraphs,
/// earlier texts, mentions, attachment, shown name and an agent's work as
/// JSON text.
#[test]
fn a_message_part_of_version_one_opens_with_every_message_value() {
    let body = vec![tilo_document::ClipBlock {
        kind: "paragraph".into(),
        text: "hi @Bo".into(),
        marks: Vec::new(),
        level: None,
        ordered: None,
        align: None,
        attrs: std::collections::BTreeMap::new(),
    }];
    let mut replica = Replica::<ChatPart>::new();
    let doc = replica.doc_mut();
    evolution::fixture(doc, &ROOT, &version_one_message(&body)).unwrap();
    Slot::Key(&ROOT, evolution::VERSION_KEY)
        .remove(doc)
        .unwrap();
    doc.commit();
    let mut old = doc.fork();
    assert!(
        evolution::read_checked::<ChatPart>(&old).is_err(),
        "unmigrated"
    );
    assert!(evolution::migrate::<ChatPart>(&mut old).unwrap());
    let part = evolution::read_checked::<ChatPart>(&old).unwrap();
    let message = part.messages.values().next().unwrap();
    assert_eq!(message.body, body);
    assert_eq!(
        message.history,
        [Revision {
            ts: 5,
            text: "hey".into()
        }]
    );
    assert_eq!(message.mentions[0].user, "u_bo");
    assert_eq!(message.reference, None);
    assert_eq!(
        message.attachment,
        Some(Attachment::File {
            name: "a.txt".into(),
            blob: "ff".into(),
            size: 5,
        })
    );
    assert_eq!(
        message.appearance,
        Some(ShownAs {
            name: "Bo".into(),
            avatar: None,
            persona: None,
        })
    );
    let work = message.work.as_ref().unwrap();
    assert_eq!(
        work.state,
        Some(Transition {
            state: AgentWorkState::Taken,
            since: 11,
        })
    );
    assert_eq!(
        work.reasoning,
        Some(AgentReasoning {
            route: AgentRoute::Floor,
            why: "its words".into(),
        })
    );
    assert_eq!(
        work.stream.as_ref().unwrap().finish,
        Some(AgentFinish::Complete)
    );
    assert_eq!(
        work.trace.iter().next().unwrap().outcome,
        Some(AgentStepOutcome::Refused {
            why: "no such page".into(),
        })
    );
}

#[test]
fn a_chat_kept_flat_before_the_model_opens_with_its_channels_and_categories() {
    use crate::model::ChannelKind;
    for version in [5, 1] {
        let mut doc = crate::testing::chat_kept_flat(version);
        assert!(evolution::migrate::<Chat>(&mut doc).unwrap());
        let chat = evolution::read_checked::<Chat>(&doc).unwrap();
        assert_eq!(
            chat.categories
                .iter()
                .map(|(id, category)| (id.to_string(), category.name.clone()))
                .collect::<Vec<_>>(),
            [
                ("cat_home_text".to_owned(), "Channels".to_owned()),
                ("cat_home_voice".to_owned(), "Voice".to_owned()),
            ],
            "version {version}"
        );
        let mut channels: Vec<_> = chat
            .channels
            .iter()
            .map(|(id, channel)| {
                (
                    id.to_string(),
                    channel.name.clone(),
                    channel.category.as_ref().map(ToString::to_string),
                    channel.kind,
                )
            })
            .collect();
        channels.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            channels,
            [
                (
                    "c_general".to_owned(),
                    "general".to_owned(),
                    Some("cat_home_text".to_owned()),
                    ChannelKind::Text
                ),
                (
                    "c_lounge".to_owned(),
                    "Lounge".to_owned(),
                    Some("cat_home_voice".to_owned()),
                    ChannelKind::Voice
                ),
            ],
            "version {version}"
        );
        let (_, categories) = ui::automerge::ReadDoc::get(&doc, ROOT, "categories")
            .unwrap()
            .unwrap();
        let (_, text) = ui::automerge::ReadDoc::get(&doc, &categories, "cat_home_text")
            .unwrap()
            .unwrap();
        assert!(
            ui::automerge::ReadDoc::get(&doc, &text, "name")
                .unwrap()
                .is_some(),
            "the flat fields stay"
        );
    }
}

#[test]
fn a_name_the_item_holds_is_kept_over_its_flat_one() {
    let mut doc = crate::testing::chat_kept_flat(5);
    let (_, categories) = ui::automerge::ReadDoc::get(&doc, ROOT, "categories")
        .unwrap()
        .unwrap();
    let (_, text) = ui::automerge::ReadDoc::get(&doc, &categories, "cat_home_text")
        .unwrap()
        .unwrap();
    let (_, held) = ui::automerge::ReadDoc::get(&doc, &text, "v")
        .unwrap()
        .unwrap();
    ui::tilo_resource::write_key(&mut doc, &held, "name", &"Text".to_owned()).unwrap();
    doc.commit();
    assert!(evolution::migrate::<Chat>(&mut doc).unwrap());
    let chat = evolution::read_checked::<Chat>(&doc).unwrap();
    assert_eq!(
        chat.categories
            .get(&ui::tilo_resource::Id::from_raw("cat_home_text"))
            .map(|category| category.name.as_str()),
        Some("Text")
    );
}

/// Everything stored under `key` of the document's root, as JSON.
fn stored(doc: &ui::tilo_resource::Doc, key: &str) -> Option<ui::serde_json::Value> {
    use ui::automerge::{ObjType, ReadDoc as _, Value};
    fn json_of(
        doc: &ui::tilo_resource::Doc,
        value: Value<'_>,
        id: &ui::automerge::ObjId,
    ) -> ui::serde_json::Value {
        match value {
            Value::Object(ObjType::Text) => json!({ "$text": doc.text(id).unwrap() }),
            Value::Object(ObjType::List) => (0..doc.length(id))
                .map(|index| {
                    let (item, item_id) = doc.get(id, index).unwrap().unwrap();
                    json_of(doc, item, &item_id)
                })
                .collect(),
            Value::Object(_) => doc
                .keys(id)
                .map(|key| {
                    let (item, item_id) = doc.get(id, key.as_str()).unwrap().unwrap();
                    (key.clone(), json_of(doc, item, &item_id))
                })
                .collect::<ui::serde_json::Map<_, _>>()
                .into(),
            Value::Scalar(scalar) => json!(format!("{scalar:?}")),
        }
    }
    let (found, id) = doc.get(ROOT, key).unwrap()?;
    Some(json_of(doc, found, &id))
}

#[test]
fn a_chat_of_version_one_or_two_keeps_every_message_draft_and_read_count() {
    use ui::tilo_resource::Resource as _;
    let body = vec![tilo_document::ClipBlock {
        kind: "paragraph".into(),
        text: "hi @Bo".into(),
        marks: Vec::new(),
        level: None,
        ordered: None,
        align: None,
        attrs: std::collections::BTreeMap::new(),
    }];
    let mut record = version_one_message(&body);
    record["messages"]["m2"] = json!({
        "k": "b",
        "v": { "channel": "c_general", "author": "bo", "ts": 20, "text": "hello" },
    });
    record["drafts"] = json!({ "c_general": { "$text": "half a thought" } });
    record["read"] = json!({ "ada": { "c_general": 2 }, "bo": { "c_general": 1 } });
    let kept_flat = json!({
        "messages": {
            "c_general": [
                { "id": "m1", "author": "ada", "ts": 10, "text": { "$text": "first" } },
                { "id": "m2", "author": "bo", "ts": 20, "text": { "$text": "second" } },
            ],
        },
    });
    for (version, shape) in [(2, &record), (1, &record), (1, &kept_flat)] {
        let mut replica = Replica::<Chat>::new();
        let doc = replica.doc_mut();
        evolution::fixture(doc, &ROOT, shape).unwrap();
        at_version(doc, version);
        doc.commit();
        let mut old = doc.fork();
        let messages = stored(&old, "messages");
        let drafts = stored(&old, "drafts");
        assert!(evolution::migrate::<Chat>(&mut old).unwrap());
        assert_eq!(evolution::stored_version(&old), Chat::VERSION);
        assert!(evolution::read_checked::<Chat>(&old).is_ok());
        assert!(messages.is_some());
        assert_eq!(stored(&old, "messages"), messages, "version {version}");
        assert_eq!(stored(&old, "drafts"), drafts, "version {version}");
        if shape.get("read").is_some() {
            assert_eq!(
                stored(&old, "read_counts"),
                Some(json!({
                    "ada": { "c_general": "Int(2)" },
                    "bo": { "c_general": "Int(1)" },
                }))
            );
        }
    }
}

/// Each kept message: its id, author, time and text.
fn kept(doc: &ui::tilo_resource::Doc) -> Vec<(String, String, i64, String)> {
    crate::record::kept_messages(doc)
        .iter()
        .map(|(id, message)| {
            (
                id.to_string(),
                message.author.0.clone(),
                message.ts,
                message.text.clone(),
            )
        })
        .collect()
}

#[test]
fn the_messages_a_chat_kept_read_under_their_own_ids_authors_and_times() {
    let expected = [
        ("m1".to_owned(), "ada".to_owned(), 10, "first".to_owned()),
        (
            "m3".to_owned(),
            "ada".to_owned(),
            15,
            "elsewhere".to_owned(),
        ),
        (
            "m2".to_owned(),
            crate::testing::BO.to_owned(),
            20,
            "second".to_owned(),
        ),
    ];
    for (version, flat) in [(2, false), (1, false), (1, true)] {
        let mut doc = crate::testing::chat_with_kept_messages(version, flat);
        assert!(evolution::migrate::<Chat>(&mut doc).unwrap());
        assert_eq!(kept(&doc), expected, "version {version}, flat {flat}");
        assert!(evolution::migrate::<Chat>(&mut doc).is_ok());
        assert_eq!(kept(&doc), expected, "opened again");
    }
}

#[test]
fn two_devices_upgrading_the_same_chat_merge_into_one_copy_of_each_kept_message() {
    let mut first = crate::testing::chat_with_kept_messages(2, false);
    let mut second = first.fork();
    assert!(evolution::migrate::<Chat>(&mut first).unwrap());
    assert!(evolution::migrate::<Chat>(&mut second).unwrap());
    tilo_automerge::ApplyIncoming::merge_incoming(&mut first, &mut second).unwrap();
    let ids: Vec<String> = kept(&first).into_iter().map(|(id, ..)| id).collect();
    assert_eq!(ids, ["m1", "m3", "m2"]);
}
