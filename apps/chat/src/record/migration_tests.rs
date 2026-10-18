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
    evolution::fixture(
        doc,
        &ROOT,
        &json!({
            "messages": {
                "m1": {
                    "k": "a",
                    "v": {
                        "channel": "c_general",
                        "author": "ada",
                        "ts": 10,
                        "text": "hi @Bo",
                        "body": ui::serde_json::to_string(&body).unwrap(),
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
        }),
    )
    .unwrap();
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
