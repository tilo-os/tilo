#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

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

/// Click the control a screen reader announces as `name`, where it is drawn.
fn click(app: &mut TestApp<BackupsShell>, name: &str) {
    let bounds = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .and_then(|(_, node)| node.bounds())
        .unwrap_or_else(|| panic!("{name} is on screen"));
    app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    app.settle();
}

#[test]
fn the_retention_steppers_edit_the_document_as_one_undo_step() {
    let mut app = TestApp::<BackupsShell>::capture(Some("schedules")).unwrap();
    assert_eq!(app.state().policy.daily, 7);
    click(&mut app, "Daily retention increase");
    assert_eq!(app.state().policy.daily, 8);
    // The palette's increment on the same class is the same action.
    let surface = app.agent_surface();
    assert_eq!(
        row(&surface, "backups.retention:ret_daily", "increment")["operation"],
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
        (Some("empty"), "Set up backups"),
        (Some("restore"), "Choose an empty folder for recovery"),
        (Some("unavailable"), "Choose another backup location"),
        (Some("schedules"), "Daily retention increase"),
        (None, "Back up now"),
    ] {
        let mut app = TestApp::<BackupsShell>::capture(scenario).unwrap();
        let tree = app.harness().inspection_json().unwrap();
        assert!(tree.contains(shown), "{scenario:?} shows {shown}: {tree}");
    }
}
