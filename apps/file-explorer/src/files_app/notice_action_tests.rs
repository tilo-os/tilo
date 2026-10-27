//! A failure that asks the person to try again waits in Needs you with the
//! way on: a shared file that could not be received hands its link to the
//! service again, and a change in a file's history that failed opens its
//! Version history, where it can be made again.

use super::*;
use crate::i18n::t;
use ui::theme::Theme;

/// Every way to press a notice's action: once per width, since the row is
/// the same row at both.
const WAYS: [(f64, bool); 2] = [(1280.0, false), (420.0, true)];

fn harness_in(dir: &std::path::Path) -> ui::headless::FrameHarness {
    ui::attention::clear();
    let mut files = FileExplorerWidget::with_persistence(None, None);
    files.navigate(dir.to_path_buf());
    files.wait_for_archive();
    let mut harness =
        ui::headless::FrameHarness::for_app(ui::State::new(), files, (1280, 720), Theme::dark());
    harness.present_in_window_bar();
    harness
}

fn files(harness: &mut ui::headless::FrameHarness) -> &mut FileExplorerWidget {
    harness.root_mut::<FileExplorerWidget>()
}

/// The label of the row waiting for this app's notice: the one action it
/// carries.
fn buttons(harness: &mut ui::headless::FrameHarness, _width: f64) -> Vec<String> {
    harness.recompose().unwrap();
    ui::attention::items()
        .into_iter()
        .filter(|item| item.owner().app == "so.tilo.files")
        .map(|item| item.action().label.clone())
        .collect()
}

/// Press the waiting row's one action, as the stack does.
fn press(harness: &mut ui::headless::FrameHarness, _width: f64, _semantic: bool) {
    let item = files_waiting().expect("a row waits for the person");
    files(harness)
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
        files(harness).notice_line()
    })
}

/// A shared file that could not be received carries Try again, which
/// hands the received-file service the same link again.
#[test]
fn a_shared_file_that_was_not_received_tries_again() {
    let _serial = crate::files_app::serial();
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness_in(dir.path());
    let link = "tilo://share/example".to_owned();
    for (width, semantic) in WAYS {
        files(&mut harness).admit_received_share(link.clone());
        let first = wait_for_failure(&mut harness);
        assert_eq!(buttons(&mut harness, width), [t("files-notice-try-again")]);
        press(&mut harness, width, semantic);
        assert_eq!(
            files(&mut harness).received_share_link.as_deref(),
            Some(link.as_str()),
            "the same link"
        );
        // The press took the failure down; only the service's answer to
        // the link handed to it again brings it back.
        let again = wait_for_failure(&mut harness);
        assert_eq!(again, first, "asked again, and answered the same");
        files(&mut harness).notice.dismiss();
    }
}

/// A change in a file's history that failed carries Open Version history,
/// which selects the file and opens its history.
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
            w.selection.clear();
            w.history_pending_path = Some(plan.clone());
            *w.history_result.lock().unwrap() = Some(Err(t("files-conflict-request-too-large")));
        }
        let said = wait_for_failure(&mut harness);
        assert_eq!(said, t("files-conflict-request-too-large"));
        assert_eq!(
            buttons(&mut harness, width),
            [t("files-notice-open-history")]
        );
        press(&mut harness, width, semantic);
        let w = files(&mut harness);
        let selected = w
            .selection
            .primary()
            .and_then(|i| w.displayed_entries().get(i).map(|entry| entry.path.clone()));
        assert_eq!(
            selected.as_ref(),
            Some(&plan),
            "the file whose change failed"
        );
        assert!(w.notice_line().is_none(), "the press took the failure down");
    }
}
