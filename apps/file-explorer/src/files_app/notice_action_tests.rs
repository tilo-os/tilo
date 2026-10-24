//! A failure that asks the person to try again carries the way to: a
//! shared file that could not be received hands its link to the service
//! again, and a change in a file's history that failed opens its Version
//! history, where it can be made again. Each by pointer and through the
//! semantic action, wide and compact.

use super::*;
use crate::i18n::t;
use ui::theme::Theme;

/// Every way to press a notice's action: wide and compact, by pointer and
/// through the semantic action.
const WAYS: [(f64, bool); 4] = [
    (1280.0, false),
    (1280.0, true),
    (420.0, false),
    (420.0, true),
];

fn harness_in(dir: &std::path::Path) -> ui::headless::FrameHarness {
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

/// The labels of the notice's buttons in a bar `width` wide.
fn buttons(harness: &mut ui::headless::FrameHarness, width: f64) -> Vec<String> {
    harness
        .window_bar_notice(width)
        .expect("a notice is up")
        .buttons
        .into_iter()
        .map(|button| button.name)
        .collect()
}

fn press(harness: &mut ui::headless::FrameHarness, width: f64, semantic: bool) {
    let pressed = if semantic {
        harness.invoke_window_notice_action(width, 0)
    } else {
        harness.click_window_notice_action(width, 0)
    };
    pressed.expect("the notice's action stands in the bar");
    harness.recompose().unwrap();
}

fn wait_for_failure(harness: &mut ui::headless::FrameHarness) -> String {
    tilo_test_env::wait_for("the failure to be said", || {
        harness.recompose().unwrap();
        files(harness).notice_line()
    })
}

/// A shared file that could not be received carries Try again, which
/// hands the received-file service the same link again.
#[test]
fn a_shared_file_that_was_not_received_tries_again() {
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
