//! The record through the door the host uses: the scan history, the
//! capture handoff, merging and what history says.

use super::*;
use ui::framework::testing::TestApp;
use ui::tilo_resource::Replica;

fn capture(id: &str, label: &str) -> SharedCapture {
    SharedCapture {
        id: id.into(),
        media_id: format!("{}{:064x}", crate::media::MEDIA_PREFIX, 7),
        kind: CaptureKind::Photo,
        ts: 42,
        w: 64,
        h: 48,
        dur_ms: 0,
        label: label.into(),
        facing: Facing::Back,
    }
}

/// Scans go to the front, a code scanned again moves there, and the
/// history is capped.
#[test]
fn scans_are_newest_first_deduped_and_capped() {
    let mut record = Replica::<Record>::new();
    for raw in ["https://a", "https://b", "https://a"] {
        record.apply(Record::RECORD_SCAN.with(raw)).unwrap();
    }
    let scans = record.state().unwrap().scans;
    assert_eq!(
        scans
            .iter()
            .map(|scan| scan.raw.as_str())
            .collect::<Vec<_>>(),
        ["https://a", "https://b"]
    );
    for index in 0..(MAX_SCANS + 10) {
        record
            .apply(Record::RECORD_SCAN.with(format!("code-{index}").as_str()))
            .unwrap();
    }
    assert_eq!(record.state().unwrap().scans.len(), MAX_SCANS);
    assert!(record.apply(Record::RECORD_SCAN.with("")).is_err());
}

/// A capture is handed off only with verified bytes and sane metadata, and
/// the record's fields change only through its verbs.
#[test]
fn a_capture_is_handed_off_with_verified_metadata_only() {
    let mut record = Replica::<Record>::new();
    record
        .apply(Record::REGISTER_CAPTURE.with(capture("IMG_1.jpg", "Harbour")))
        .unwrap();
    assert_eq!(record.state().unwrap().recent.unwrap().label, "Harbour");
    let mut forged = capture("IMG_2.jpg", "Forged");
    forged.media_id = "/home/someone/IMG_2.jpg".into();
    assert!(
        record.apply(Record::REGISTER_CAPTURE.with(forged)).is_err(),
        "a path is never a capture's identity"
    );
    let mut empty = capture("IMG_3.jpg", "Empty");
    empty.w = 0;
    assert!(record.apply(Record::REGISTER_CAPTURE.with(empty)).is_err());
    assert!(
        ui::tilo_resource::Invoke::edit(&Record::paths().scans.set(Vec::new()))
            .ok()
            .and_then(|invoke| record.apply(invoke).ok())
            .is_none(),
        "the scan history changes only by scanning"
    );
}

/// Two devices scanning at once converge on one history; the capture
/// handed off last stays whole.
#[test]
fn concurrent_scans_and_handoffs_converge() {
    let mut ours = Replica::<Record>::new();
    let mut theirs = ours.fork();
    ours.apply(Record::RECORD_SCAN.with("ours")).unwrap();
    ours.apply(Record::REGISTER_CAPTURE.with(capture("IMG_ours.jpg", "Ours")))
        .unwrap();
    theirs.apply(Record::RECORD_SCAN.with("theirs")).unwrap();
    theirs
        .apply(Record::REGISTER_CAPTURE.with(capture("IMG_theirs.jpg", "Theirs")))
        .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let merged = ours.state().unwrap();
    assert_eq!(merged, theirs.state().unwrap());
    let recent = merged.recent.unwrap();
    assert!(
        (recent.id == "IMG_ours.jpg" && recent.label == "Ours")
            || (recent.id == "IMG_theirs.jpg" && recent.label == "Theirs"),
        "one handoff wins whole"
    );
}

/// What an agent records reads as Camera's sentences.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<crate::CameraShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Record as ui::tilo_resource::Resource>::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "record-scan",
        serde_json::json!({"raw": "https://tilo.test"}),
    )
    .unwrap();
    app.call_by(
        agent,
        kind,
        "register-capture",
        serde_json::json!({"capture": serde_json::to_value(capture("IMG_9.jpg", "Pier")).unwrap()}),
    )
    .unwrap();
    let source = crate::LOCALES[0].1;
    let catalogue = tilo_history::Catalogue::new("en-US", &[source]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        ["Scanned https://tilo.test", "Handed off a new capture"]
    );
}
