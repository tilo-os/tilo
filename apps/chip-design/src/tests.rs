#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use ui::framework::testing::TestApp;
use ui::tilo_resource::{ActionRef, Invoke};

use crate::{ChipDesignShell, ChipProject, ProjectAction};

const REPORT: &str = include_str!("../fixtures/storage-report.json");
const IMPORT: ActionRef<ChipProject, String> = ActionRef::new("import-report", &["json"]);
const REMOVE: ActionRef<ChipProject, String> = ActionRef::new("remove-report", &["report"]);

#[test]
fn semantic_import_preserves_provenance_and_exact_storage_and_is_undoable() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.state().reports.is_empty(), "live projects start empty");
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    let state = app.state();
    let id = crate::model::report_id(REPORT);
    let report = state
        .reports
        .get(&id)
        .expect("semantic import published the report");
    assert_eq!(report.model, "Qwen/Qwen3.8-27B");
    assert_eq!(report.revision, "1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0");
    assert_eq!(report.packed_weight_bytes, 13_500_000_000);
    assert!(
        report
            .not_estimated
            .iter()
            .any(|item| item == "coding_quality")
    );
    assert!(app.undo());
    assert!(app.state().reports.is_empty());
}

#[test]
fn repeated_import_is_idempotent_and_remove_uses_the_published_action() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    assert_eq!(app.state().reports.len(), 1);
    app.invoke(REMOVE.with(crate::model::report_id(REPORT)))
        .unwrap();
    assert!(app.state().reports.is_empty());
}

#[test]
fn forged_storage_and_moving_revision_are_refused_on_the_production_door() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (field, value) in [
        ("packed_weight_bytes", serde_json::json!(1)),
        ("revision", serde_json::json!("main")),
    ] {
        let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
        report.as_object_mut().unwrap().insert(field.into(), value);
        // Exercise the typed apply door too: a caller cannot bypass validation
        // by encoding an action instead of using the named declaration.
        let invoke = Invoke::apply(&ProjectAction::ImportReport {
            json: report.to_string(),
        })
        .unwrap();
        assert!(app.invoke(invoke).is_err());
        assert!(app.state().reports.is_empty());
    }
}

#[test]
fn unsupported_cache_stays_unknown_and_is_never_displayed_as_zero() {
    let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
    report.as_object_mut().unwrap().insert("full_attention_cache".into(),
        serde_json::json!({"status":"unsupported", "reason":"specialized cache formula unverified"}));
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(report.to_string())).unwrap();
    assert!(matches!(
        app.state()
            .reports
            .values()
            .next()
            .unwrap()
            .full_attention_cache,
        crate::AttentionCache::Unsupported { .. }
    ));
}

#[test]
fn captures_use_the_production_surface_and_never_seed_live_records() {
    for scenario in ["empty", "storage-report"] {
        let mut app = TestApp::<ChipDesignShell>::capture(Some(scenario)).unwrap();
        assert_eq!(
            app.state().reports.len(),
            usize::from(scenario == "storage-report")
        );
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{scenario}: {audit:#?}");
    }
    assert!(
        TestApp::<ChipDesignShell>::new()
            .unwrap()
            .state()
            .reports
            .is_empty()
    );
}

#[test]
fn consented_file_import_and_cancellation_use_the_framework_job_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("storage-report.json");
    std::fs::write(&path, REPORT).unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
    app.effect(&crate::view::ChipEffect::ImportFile).unwrap();
    app.settle();
    assert_eq!(app.state().reports.len(), 1);
    assert_eq!(recorder.pick_requests().len(), 1);

    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    assert!(app.effect(&crate::view::ChipEffect::ImportFile).is_err());
    app.settle();
    assert_eq!(
        app.state().reports.len(),
        1,
        "cancelled jobs fabricate no evidence"
    );
}

#[test]
fn oversized_annotations_are_refused_before_building_report_widgets() {
    let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
    report.as_object_mut().unwrap().insert(
        "limitations".into(),
        serde_json::json!(vec!["annotation"; 100]),
    );
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let invoke = Invoke::apply(&ProjectAction::ImportReport {
        json: report.to_string(),
    })
    .unwrap();
    assert!(app.invoke(invoke).is_err());
    assert!(app.state().reports.is_empty());
}

#[test]
fn concurrent_first_imports_share_the_initialized_collection_and_both_survive_merge() {
    use ui::automerge::{ROOT, ReadDoc};
    use ui::tilo_actions::{EncodedAction, LocalTransport};
    use ui::tilo_resource::Resource;

    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut peer = app.peer_document();
    let mut second: serde_json::Value = serde_json::from_str(REPORT).unwrap();
    second
        .get_mut("limitations")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(
            "Independent collaborator comparison report."
        ));
    let second = second.to_string();
    let first_id = crate::model::report_id(REPORT);
    let second_id = crate::model::report_id(&second);
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    let action = ProjectAction::ImportReport { json: second };
    let encoded = EncodedAction::encode(ChipProject::ACTION_TYPE, &action).unwrap();
    ui::tilo_actions::dispatch(&mut peer, &action, encoded, &mut LocalTransport::new()).unwrap();
    app.merge_peer(&mut peer).unwrap();
    let state = app.state();
    assert_eq!(state.reports.len(), 2);
    assert!(state.reports.contains_key(&first_id));
    assert!(state.reports.contains_key(&second_id));
    assert_eq!(
        ChipProject::read(&peer).unwrap(),
        state,
        "both replicas converge"
    );
    assert_eq!(
        app.read_document(|doc| doc.get_all(ROOT, "reports").unwrap().len()),
        1,
        "the first imports must not create conflicting map objects"
    );
}

#[test]
fn report_children_publish_inspection_removal_and_typed_reads_on_the_production_door() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    let id = crate::model::report_id(REPORT);
    let child = crate::resource::report_resource_id(&id);
    let surface = app.agent_surface();
    for action in ["inspect", "remove-report"] {
        assert!(
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == child && row["action"] == action),
            "report child offers {action}: {surface}"
        );
    }
    app.call_on(&child, "inspect", serde_json::json!({}))
        .unwrap();
    let content = app
        .call("read", serde_json::json!({"item": child}))
        .unwrap()
        .unwrap();
    assert_eq!(content["model"], "Qwen/Qwen3.8-27B");
    assert_eq!(content["packed_weight_bytes"], 13_500_000_000_u64);
    app.call_on(&child, "remove-report", serde_json::json!({}))
        .unwrap();
    assert!(app.state().reports.is_empty());
    assert!(
        app.call_on(&child, "inspect", serde_json::json!({}))
            .is_err(),
        "a stale child is refused"
    );
}
