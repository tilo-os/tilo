//! The window bar's sidebar button over the places.

use super::*;

/// The window bar's sidebar button flips the places on every click, in a
/// window wide enough to dock them and in one so narrow they are a screen
/// of their own.
#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    let dir = tempfile::tempdir().unwrap();
    for viewport in ui::framework::testing::SIDEBAR_BUTTON_WINDOWS {
        let mut files = FileExplorerWidget::with_persistence(None, None);
        files.navigate(dir.path().to_path_buf());
        files.wait_for_archive();
        let mut harness = ui::headless::FrameHarness::for_app(
            ui::State::new(),
            files,
            viewport,
            ui::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness
            .check_window_sidebar_button()
            .unwrap_or_else(|error| panic!("{viewport:?}: {error}"));
    }
}
