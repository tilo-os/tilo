use super::*;
use crate::effects::FilesEffect;
use crate::resource::FileExplorerShell;
use ui::prelude::TestApp;

#[test]
fn recent_capture_stages_actual_rows_and_effect_outcomes() {
    for scenario in [
        "recent",
        "recent-menu",
        "recent-revealed",
        "recent-forgotten",
    ] {
        let mut app = TestApp::<FileExplorerShell>::capture(Some(scenario)).unwrap();
        app.with_surface(|files: &mut FileExplorerWidget| {
            assert!(
                files.state().status.is_none(),
                "a capture must not hide a failed effect"
            );
            match scenario {
                "recent-revealed" => {
                    assert!(!files.state().is_recent_view());
                    let selected = files.state().selected_index().unwrap();
                    assert_eq!(
                        files.state().entries()[selected].name,
                        "Quarterly report.pdf"
                    );
                    assert!(files.state().entries()[selected].path.exists());
                }
                "recent-forgotten" => {
                    assert!(files.state().is_recent_view());
                    assert_eq!(files.state().entries().len(), 1);
                    assert_eq!(files.state().entries()[0].name, "Notes.txt");
                    assert!(
                        files.state().entries()[0]
                            .path
                            .parent()
                            .unwrap()
                            .join("Quarterly report.pdf")
                            .exists()
                    );
                }
                _ => {
                    assert!(files.state().is_recent_view());
                    assert_eq!(files.state().entries().len(), 2);
                    if scenario == "recent-menu" {
                        assert!(
                            files
                                .chrome()
                                .context_menu
                                .items()
                                .iter()
                                .any(|item| item.label() == Some("Open containing folder"))
                        );
                    }
                }
            }
        })
        .unwrap();
    }
}

fn invoke(app: &mut TestApp<FileExplorerShell>, row: &str, action: &str) {
    app.harness()
        .call_on(row, action, serde_json::json!({}))
        .unwrap();
    app.settle();
}
#[test]
fn recent_resource_reveals_and_forgets_without_changing_files() {
    let root = tempfile::tempdir().unwrap();
    let reports = root.path().join("Reports");
    std::fs::create_dir(&reports).unwrap();
    let target = reports.join("budget.txt");
    let sibling = reports.join("notes.txt");
    std::fs::write(&target, b"budget").unwrap();
    std::fs::write(&sibling, b"notes").unwrap();
    let mut app = TestApp::<FileExplorerShell>::launched(&[root.path().to_str().unwrap()]).unwrap();
    let shared_before = app.state();
    app.with_surface(|files: &mut FileExplorerWidget| {
        files
            .state()
            .note_recent(&sibling, crate::recent::RecentUse::Opened);
        files
            .state()
            .note_recent(&target, crate::recent::RecentUse::Opened);
    })
    .unwrap();
    app.effect(&FilesEffect::Recent).unwrap();
    let row = crate::search_provider::entry_resource_id(&target);
    let published = app
        .with_surface(|files: &mut FileExplorerWidget| files.state().published_row(&row).unwrap())
        .unwrap();
    for expected in ["reveal", "forget-recent"] {
        assert!(
            published.actions.iter().any(|action| action.id == expected),
            "Recent's {expected} must be available through the resource door"
        );
        assert_eq!(
            published
                .actions
                .iter()
                .find(|action| action.id == expected)
                .unwrap()
                .kind,
            ui::tilo_actions::OperationKind::View,
            "Recent navigation and device history do not mutate shared files"
        );
    }
    app.leave_room();
    for action in ["reveal", "forget-recent"] {
        assert!(app.call_on(&row, action, serde_json::json!({})).is_err());
    }
    app.with_surface(|files: &mut FileExplorerWidget| {
        assert!(files.state().is_recent_view());
        assert_eq!(files.state().entries().len(), 2);
    })
    .unwrap();
    app.set_role(ui::permissions::Role::Viewer);
    invoke(&mut app, &row, "reveal");
    app.with_surface(|files: &mut FileExplorerWidget| {
        assert_eq!(files.state().current_path(), reports);
        let selected = files.state().selected_index().unwrap();
        assert_eq!(files.state().entries()[selected].path, target);
    })
    .unwrap();
    app.effect(&FilesEffect::Back).unwrap();
    invoke(&mut app, &row, "forget-recent");
    app.with_surface(|files: &mut FileExplorerWidget| {
        assert!(files.state().published_row(&row).is_none());
        assert_eq!(files.state().entries().len(), 1);
        assert_eq!(files.state().entries()[0].path, sibling);
    })
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"budget");
    assert_eq!(std::fs::read(&sibling).unwrap(), b"notes");
    assert_eq!(
        app.state(),
        shared_before,
        "shared bookmarks and file operations stay unchanged"
    );
    assert_eq!(
        app.settings::<crate::recent::RecentFiles>()
            .get()
            .items()
            .len(),
        1
    );
    assert!(
        app.call_on(&row, "forget-recent", serde_json::json!({}))
            .is_err()
    );
    let mut reopened = app.reopen().unwrap();
    reopened.effect(&FilesEffect::Recent).unwrap();
    reopened
        .with_surface(|files: &mut FileExplorerWidget| {
            assert_eq!(files.state().entries().len(), 1);
            assert_eq!(files.state().entries()[0].path, sibling);
        })
        .unwrap();
}

#[test]
fn retained_recent_actions_refuse_other_views_and_deleted_rows() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("report.txt");
    std::fs::write(&target, b"report").unwrap();
    let mut app = TestApp::<FileExplorerShell>::launched(&[root.path().to_str().unwrap()]).unwrap();
    app.with_surface(|files: &mut FileExplorerWidget| {
        files
            .state()
            .note_recent(&target, crate::recent::RecentUse::Opened);
    })
    .unwrap();
    let row = crate::search_provider::entry_resource_id(&target);
    for effect in [
        FilesEffect::RevealRecent { entry: row.clone() },
        FilesEffect::ForgetRecent { entry: row.clone() },
    ] {
        assert!(
            app.effect(&effect).is_err(),
            "a folder row is not a Recent target"
        );
    }
    assert_eq!(
        app.settings::<crate::recent::RecentFiles>()
            .get()
            .items()
            .len(),
        1
    );
    app.effect(&FilesEffect::Recent).unwrap();
    for action in ["reveal", "forget-recent"] {
        assert!(
            app.call_on(
                &row,
                action,
                serde_json::json!({"entry": root.path().display().to_string()})
            )
            .is_err(),
            "bound targets cannot be replaced by caller paths"
        );
    }
    std::fs::remove_file(&target).unwrap();
    for action in ["reveal", "forget-recent"] {
        assert!(
            app.call_on(&row, action, serde_json::json!({})).is_err(),
            "an unrefreshed but deleted row is stale"
        );
    }
    app.with_surface(|files: &mut FileExplorerWidget| {
        assert!(files.state().is_recent_view());
        files.state().refresh();
        assert!(files.state().published_row(&row).is_none());
    })
    .unwrap();
}
