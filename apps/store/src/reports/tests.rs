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

/// A draft names a well-formed app identity, trimmed; anything else is
/// refused and makes no report.
#[test]
fn a_report_draft_names_only_a_well_formed_app() {
    // An identity is at most 255 characters.
    let longest = "a".repeat(255);
    let too_long = "a".repeat(256);
    for (app_id, drafted) in [
        (NOTES, Some(NOTES)),
        ("  so.tilo.notes  ", Some(NOTES)),
        ("org.gnome.Chess-2", Some("org.gnome.Chess-2")),
        (longest.as_str(), Some(longest.as_str())),
        (too_long.as_str(), None),
        ("", None),
        ("   ", None),
        ("so.tilo/notes", None),
        ("so.tilo notes", None),
        ("so.tilo_notes", None),
        ("so.tilo.n\u{f6}tes", None),
        ("so.tilo.notes\0", None),
    ] {
        let mut reports = Replica::<Reports>::new();
        let result = reports
            .apply(Reports::DRAFT_REPORT.with(app_id))
            .map(drop)
            .map_err(|error| error.to_string());
        let state = reports.state().unwrap();
        let found: Vec<(&str, &str)> = state
            .reports
            .iter()
            .map(|(app, report)| (app.as_str(), report.status.as_str()))
            .collect();
        let expected: Vec<(&str, &str)> =
            drafted.into_iter().map(|app| (app, DRAFT_STATUS)).collect();
        assert_eq!(found, expected, "{app_id:?} gave {result:?}");
        assert_eq!(
            result.is_ok(),
            drafted.is_some(),
            "{app_id:?} gave {result:?}"
        );
    }
}

/// A case record is taken whole or refused whole: its app, its case id and
/// its status are each bounded, and a refused record leaves the report as
/// it was.
#[test]
fn a_moderation_case_record_is_bounded_field_by_field() {
    // A case id and a status are each at most 128 characters.
    let longest_case = "c".repeat(128);
    let too_long_case = "c".repeat(129);
    let longest_status = "s".repeat(128);
    let too_long_status = "s".repeat(129);
    let too_long_app = "a".repeat(256);
    for (app_id, case_id, status, recorded) in [
        (NOTES, "case-1", "Open", Some("case-1")),
        (NOTES, "case:2026.11-7", "Open", Some("case:2026.11-7")),
        (NOTES, "  case-1  ", "Open", Some("case-1")),
        ("  so.tilo.notes  ", "case-1", "Open", Some("case-1")),
        (
            NOTES,
            longest_case.as_str(),
            "Open",
            Some(longest_case.as_str()),
        ),
        (NOTES, too_long_case.as_str(), "Open", None),
        (NOTES, "", "Open", None),
        (NOTES, "   ", "Open", None),
        (NOTES, "case 1", "Open", None),
        (NOTES, "case/1", "Open", None),
        (NOTES, "case_1", "Open", None),
        (NOTES, "c\u{e4}se-1", "Open", None),
        (NOTES, "case-1", longest_status.as_str(), Some("case-1")),
        (NOTES, "case-1", too_long_status.as_str(), None),
        (NOTES, "case-1", "", None),
        ("", "case-1", "Open", None),
        ("../x", "case-1", "Open", None),
        (too_long_app.as_str(), "case-1", "Open", None),
    ] {
        let mut reports = Replica::<Reports>::new();
        reports.apply(Reports::DRAFT_REPORT.with(NOTES)).unwrap();
        let before = reports.state().unwrap();
        let record = Reports::RECORD_MODERATION_CASE.with(reports::RecordModerationCase {
            app_id: app_id.into(),
            case_id: case_id.into(),
            status: status.into(),
        });
        let result = reports
            .apply(record)
            .map(drop)
            .map_err(|error| error.to_string());
        let row = format!("{app_id:?} {case_id:?} {status:?} gave {result:?}");
        let after = reports.state().unwrap();
        if let Some(case) = recorded {
            assert!(result.is_ok(), "{row}");
            assert_eq!(after.reports.len(), 1, "{row}");
            let report = &after.reports[NOTES];
            assert_eq!(report.case_id(), Some(case), "{row}");
            assert_eq!(report.status, status, "{row}");
        } else {
            assert!(result.is_err(), "{row}");
            assert_eq!(after, before, "{row}");
        }
    }
}

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
    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Started a report about so.tilo.notes",
            "Described the concern of a report",
        ]
    );
}
