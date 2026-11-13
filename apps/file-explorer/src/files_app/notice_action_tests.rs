//! A notice's action, pressed from the row waiting in the attention stack.

use super::*;
use crate::i18n::t;
use ui::theme::Theme;

const WAYS: [(f64, bool); 2] = [(1280.0, false), (420.0, true)];

fn harness_in(dir: &std::path::Path) -> ui::headless::FrameHarness {
    ui::attention::clear();
    let files = FileExplorerWidget::with_persistence(None, None);
    files.state().navigate(dir.to_path_buf());
    files.state().wait_for_archive();
    let mut harness =
        ui::headless::FrameHarness::for_app(ui::State::new(), files, (1280, 720), Theme::dark());
    harness.present_in_window_bar();
    harness
}

fn files(
    harness: &mut ui::headless::FrameHarness,
) -> ui::headless::RootMut<'_, FileExplorerWidget> {
    harness.root_mut::<FileExplorerWidget>()
}

fn buttons(harness: &mut ui::headless::FrameHarness, _width: f64) -> Vec<String> {
    harness.recompose().unwrap();
    ui::attention::items()
        .into_iter()
        .filter(|item| item.owner().app == "so.tilo.files")
        .map(|item| item.action().label.clone())
        .collect()
}

fn press(harness: &mut ui::headless::FrameHarness, _width: f64, _semantic: bool) {
    let item = files_waiting().expect("a row waits for the person");
    files(harness)
        .state()
        .needs
        .pressed(ui::attention::Press::Act(item.id()));
    harness.recompose().unwrap();
}

fn files_waiting() -> Option<ui::attention::Item> {
    ui::attention::items()
        .into_iter()
        .find(|item| item.owner().app == "so.tilo.files")
}

fn wait_for_failure(harness: &mut ui::headless::FrameHarness) -> String {
    tilo_test_env::wait_for("the failure to wait for the person", || {
        harness.recompose().unwrap();
        files(harness).state().notice_line()
    })
}

#[test]
fn a_failed_history_change_opens_version_history() {
    let _serial = crate::files_app::serial();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plan.txt"), b"x").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"y").unwrap();
    let mut harness = harness_in(dir.path());
    let plan = dir.path().join("plan.txt");
    for (width, semantic) in WAYS {
        {
            let w = files(&mut harness);
            w.state().selection.clear();
            w.state().history_pending_path = Some(plan.clone());
            *w.state().history_result.lock().unwrap() =
                Some(Err(t("files-conflict-request-too-large")));
        }
        let said = wait_for_failure(&mut harness);
        assert_eq!(said, t("files-conflict-request-too-large"));
        assert_eq!(
            buttons(&mut harness, width),
            [t("files-notice-open-history")]
        );
        press(&mut harness, width, semantic);
        let w = files(&mut harness);
        let selected = {
            let state = w.state();
            state.selection.primary().and_then(|i| {
                state
                    .displayed_entries()
                    .get(i)
                    .map(|entry| entry.path.clone())
            })
        };
        assert_eq!(
            selected.as_ref(),
            Some(&plan),
            "the file whose change failed"
        );
        assert!(
            w.state().notice_line().is_none(),
            "the press took the failure down"
        );
    }
}
