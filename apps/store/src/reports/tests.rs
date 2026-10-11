//! The reports through the door the host uses: drafting, typing a note,
//! recording a case, merging and what history says.

use super::*;
use ui::tilo_resource::{Edit, Invoke, Replica};

const NOTES: &str = "so.tilo.notes";

fn edit(reports: &mut Replica<Reports>, edit: &Edit<Reports>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    reports
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn note(reports: &Replica<Reports>) -> String {
    reports.state().unwrap().reports[NOTES].note.clone()
}

/// A draft starts with an empty note, keeps a note it already has, bounds
/// the note, and two people typing in one note merge.
#[test]
fn report_drafts_carry_a_bounded_collaborative_note() {
    let mut reports = Replica::<Reports>::new();
    reports.apply(Reports::DRAFT_REPORT.with(NOTES)).unwrap();
    assert_eq!(reports.state().unwrap().reports[NOTES].status, DRAFT_STATUS);

    edit(
        &mut reports,
        &Reports::note(NOTES).splice(0, 0, "Unsigned binary"),
    )
    .unwrap();
    reports.apply(Reports::DRAFT_REPORT.with(NOTES)).unwrap();
    assert_eq!(
        note(&reports),
        "Unsigned binary",
        "a new draft keeps the note"
    );
    assert!(
        edit(
            &mut reports,
            &Reports::note(NOTES).splice(0, 0, "x".repeat(MAX_REPORT_NOTE_CHARS))
        )
        .is_err(),
        "the note is bounded"
    );
    assert_eq!(note(&reports), "Unsigned binary");
    assert!(reports.apply(Reports::DRAFT_REPORT.with("../x")).is_err());

    let mut theirs = reports.fork();
    edit(
        &mut reports,
        &Reports::note(NOTES).splice(0, 0, "Seen offline: "),
    )
    .unwrap();
    edit(&mut theirs, &Reports::note(NOTES).splice(15, 0, " in 0.2")).unwrap();
    reports.sync_with(&mut theirs).unwrap();
    assert_eq!(note(&reports), "Seen offline: Unsigned binary in 0.2");
    assert_eq!(reports.state().unwrap(), theirs.state().unwrap());
}

/// The registry's case is recorded with its status; a malformed case id is
/// refused, and nobody sets a status or a case by hand.
#[test]
fn a_moderation_case_is_recorded_on_its_report() {
    let mut reports = Replica::<Reports>::new();
    reports.apply(Reports::DRAFT_REPORT.with(NOTES)).unwrap();
    let case = |case_id: &str| {
        Reports::RECORD_MODERATION_CASE.with(reports::RecordModerationCase {
            app_id: NOTES.into(),
            case_id: case_id.into(),
            status: "Open".into(),
        })
    };
    reports.apply(case("case-authoritative")).unwrap();
    let state = reports.state().unwrap();
    let report = &state.reports[NOTES];
    assert_eq!(report.case_id(), Some("case-authoritative"));
    assert_eq!(report.status, "Open");
    assert_eq!(report_row_id(NOTES, report), "case-authoritative");
    assert!(reports.apply(case("case-../../outside")).is_err());
    assert!(
        edit(
            &mut reports,
            &Reports::paths().reports.key(NOTES).status.replace("Closed")
        )
        .is_err(),
        "only the registry's answer changes a status"
    );
}

/// What an agent does with a report reads as the Store's sentences.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<crate::StoreShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Reports as ui::tilo_resource::Resource>::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "draft-report",
        serde_json::json!({"app_id": NOTES}),
    )
    .unwrap();
    app.call_by(
        agent,
        kind,
        "field-edit",
        serde_json::json!({ "edit": Reports::note(NOTES).replace("Unsigned binary") }),
    )
    .unwrap();
    let english = crate::resource::LOCALES
        .iter()
        .find(|(locale, _)| *locale == "en-US")
        .map(|(_, source)| *source)
        .unwrap();
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
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
        [
            "Started a report about so.tilo.notes",
            "Described the concern of a report",
        ]
    );
}
