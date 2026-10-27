#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

//! Backups on the framework: the retention controls edit the document
//! through the resource's action, and backing up, verifying and restoring
//! are effects every caller reaches the same way, with jobs that run off
//! the UI thread against this device's repository.

use backups::actions::BackupsEffect;
use backups::{BackupLocation, BackupsShell};
use ui::framework::testing::TestApp;

fn row<'a>(surface: &'a serde_json::Value, resource: &str, action: &str) -> &'a serde_json::Value {
    surface
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["resource"] == resource && row["action"] == action)
        .unwrap_or_else(|| panic!("{resource}#{action} is published: {surface:#}"))
}

#[test]
fn the_retention_steppers_edit_the_document_as_one_undo_step() {
    let mut app = TestApp::<BackupsShell>::capture(Some("schedules")).unwrap();
    assert_eq!(app.state().policy.daily, 7);
    app.harness()
        .perform_semantic_named("backups-keep-daily", None, &ui::a11y::Action::Increment)
        .expect("the daily retention spinbutton exposes increase");
    app.settle();
    assert_eq!(app.state().policy.daily, 8);
    // The stepper's change is the daily count's field edit, which an agent
    // makes in one call as well.
    let surface = app.agent_surface();
    assert_eq!(
        row(&surface, "backups.workspace", "field-edit")["operation"],
        "Document"
    );
    assert!(app.undo());
    assert_eq!(app.state().policy.daily, 7);
}

#[test]
fn an_unconfigured_device_offers_setup_and_refuses_to_back_up() {
    let mut app = TestApp::<BackupsShell>::new().unwrap();
    let surface = app.agent_surface();
    assert_eq!(
        row(&surface, "backups.workspace", "setup")["operation"],
        "Effect"
    );
    assert_eq!(
        row(&surface, "backups.workspace", "backup-now")["operation"],
        "Effect"
    );
    let refused = app.effect(&BackupsEffect::BackupNow).unwrap_err();
    assert!(
        matches!(&refused, ui::tilo_actions::DoorError::Unavailable { code, .. } if code == "not_configured"),
        "{refused:?}"
    );
}

#[test]
fn backups_run_as_jobs_against_this_devices_repository_and_follow_the_policy() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("documents");
    let destination = temp.path().join("drive");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(source.join("letter.txt"), b"dear backup").unwrap();
    // Both backups below fall in one hour whenever the test runs: the
    // retention policy buckets snapshots by the UTC hour they were taken
    // in, and a running clock may cross into the next one between them.
    let half_past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_792_841_400);
    ui::time::freeze_wall(half_past);

    let mut app = TestApp::<BackupsShell>::new().unwrap();
    // The location is this device's setting, not the document.
    app.settings::<BackupLocation>()
        .set(&BackupLocation {
            source: Some(source),
            destination: Some(destination),
        })
        .unwrap();
    app.settle();

    let first = app.effect(&BackupsEffect::BackupNow).unwrap();
    let first = first.value.unwrap();
    assert_eq!(first["files"], 1);
    let progress = app.effect_progress();
    assert!(
        progress.iter().any(|update| matches!(
            update,
            ui::tilo_actions::InvocationUpdate::Progress { phase, .. } if phase == "copying"
        )),
        "{progress:?}"
    );
    app.settle();
    let id = first["snapshot"].as_str().unwrap().to_owned();

    // The new snapshot is a live row, offering what this device can do with it.
    let snapshot = format!("backups.snapshot:{id}");
    let surface = app.agent_surface();
    assert_eq!(row(&surface, &snapshot, "verify")["operation"], "Effect");
    assert_eq!(row(&surface, &snapshot, "open")["operation"], "View");
    let verified = app
        .effect(&BackupsEffect::Verify {
            snapshot_id: id.clone(),
        })
        .unwrap();
    assert_eq!(verified.value.unwrap()["files"], 1);
    app.effect(&BackupsEffect::ShowSnapshot {
        snapshot_id: id.clone(),
    })
    .unwrap();
    assert!(
        app.effect(&BackupsEffect::ShowSnapshot {
            snapshot_id: "0".into(),
        })
        .is_err()
    );

    // A second backup within the same hour replaces the first: the policy
    // keeps the newest snapshot of each hour.
    let second = app
        .effect(&BackupsEffect::BackupNow)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(second["removed"], serde_json::json!([id]));
    app.settle();
    let surface = app.agent_surface();
    assert!(
        !surface
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["resource"] == snapshot.as_str()),
        "the pruned snapshot left the catalog"
    );
    let kept = format!("backups.snapshot:{}", second["snapshot"].as_str().unwrap());
    row(&surface, &kept, "restore");
    // Nothing of it is the document.
    assert_eq!(
        app.state().policy,
        backups::model::RetentionPolicy::default()
    );
}

#[test]
fn every_scenario_stages_its_screen() {
    for (scenario, shown) in [
        (Some("empty"), "Protect your important files"),
        (Some("restore"), "Choose folder…"),
        (Some("unavailable"), "Choose another location…"),
        (Some("schedules"), "Daily snapshots to keep"),
        (None, "Snapshots"),
    ] {
        let mut app = TestApp::<BackupsShell>::capture(scenario).unwrap();
        let tree = app.harness().inspection_json().unwrap();
        assert!(tree.contains(shown), "{scenario:?} shows {shown}: {tree}");
    }
    // The overview's one main action is the window bar's.
    let _app = TestApp::<BackupsShell>::capture(None).unwrap();
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    assert!(
        declared
            .actions
            .iter()
            .any(|action| action.name == "Back up now"),
        "{declared:?}"
    );
}

#[test]
fn an_agent_sets_a_retention_class_in_one_call_and_history_says_so() {
    use backups::Backups;
    let mut app = TestApp::<BackupsShell>::new().unwrap();
    let policy = Backups::paths().policy;
    app.call(
        "field-edit",
        serde_json::json!({ "edit": policy.monthly.set(24) }),
    )
    .unwrap();
    assert_eq!(app.state().policy.monthly, 24);
    assert!(
        app.call(
            "field-edit",
            serde_json::json!({ "edit": policy.weekly.set(5000) })
        )
        .is_err(),
        "a class keeps at most 999"
    );
    let catalogue = tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .history()
        .into_iter()
        .filter(|entry| entry.what.is_some())
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(sentences, ["Keeping 24 monthly snapshots"]);
}

#[test]
fn two_devices_changing_different_classes_keep_both() {
    use backups::Backups;
    use ui::prelude::{Invoke, Replica};
    let mut left = Replica::<Backups>::new();
    let mut right = left.fork();
    let policy = Backups::paths().policy;
    left.apply(Invoke::edit(&policy.daily.set(3)).unwrap())
        .unwrap();
    right
        .apply(Invoke::edit(&policy.hourly.set(48)).unwrap())
        .unwrap();
    left.sync_with(&mut right).unwrap();
    let merged = left.state().unwrap().policy;
    assert_eq!((merged.daily, merged.hourly), (3, 48));
    assert_eq!(left.state().unwrap(), right.state().unwrap());
}
