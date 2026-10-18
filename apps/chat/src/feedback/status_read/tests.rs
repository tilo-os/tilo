use super::*;
use crate::feedback::TriageUpdate;
use automerge::transaction::Transactable;
use tilo_automerge::ApplyIncoming as _;

#[test]
fn concurrent_full_triage_histories_still_have_a_bounded_public_status_read() {
    let mut doc = crate::testing::demo_chat();
    let channel = crate::testing::channels(&doc)
        .into_iter()
        .find(|channel| channel.kind == crate::model::ChannelKind::Text)
        .unwrap();
    let submission = ui::feedback::Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: ui::feedback::Destination {
            room: "room".into(),
            provider: "fixture-chat".into(),
            channel: channel.id,
        },
        feedback: ui::feedback::ReviewedFeedback {
            text: "The page loses my position".into(),
            context: Vec::new(),
            screenshot: None,
        },
    };
    crate::testing::apply_as(
        &mut doc,
        "visitor",
        crate::testing::submit_feedback(&submission),
    )
    .unwrap();
    doc.commit();
    let mut other = doc.fork();
    for (author, branch) in [("one", &mut doc), ("two", &mut other)] {
        for ts in 1..=128 {
            crate::testing::apply_at(
                branch,
                author,
                ts * 1000,
                crate::resource::Chat::TRIAGE.with(crate::record::chat::Triage {
                    report: ui::tilo_resource::Id::from_raw(submission.id.clone()),
                    update: serde_json::to_value(TriageUpdate::Investigating).unwrap(),
                }),
            )
            .unwrap();
        }
        branch.commit();
    }
    doc.merge_incoming(&mut other).unwrap();
    doc.commit();
    let report = crate::feedback::report(&doc, &submission.id)
        .unwrap()
        .unwrap();
    assert_eq!(report.triage.len(), 256);
    let resource = crate::testing::report_resource(&submission.id, &submission.feedback.text);
    let operation = tilo_actions::door::encode_json(
        std::slice::from_ref(&resource),
        &resource
            .action_address("read-feedback-status")
            .expect("a report publishes its status read"),
        &serde_json::Map::new(),
    )
    .unwrap();
    super::super::receipt_read::validate(&resource, &operation.payload).unwrap();
    let page = project(&doc, &doc.base_heads(), &operation.payload).unwrap();
    assert_eq!(page.content["earlierEntries"], 128);
    let entries = page.content["triage"].as_array().unwrap();
    assert_eq!(entries.len(), 128);
    assert_eq!(entries.first().unwrap()["ts"], 65);
    assert_eq!(entries.last().unwrap()["ts"], 128);
    assert!(entries.iter().any(|entry| entry["author"] == "one"));
    assert!(entries.iter().any(|entry| entry["author"] == "two"));
}
