//! A GPU that dies under a running desktop does not take the desktop with it.
//!
//! A laptop's GPU firmware once died mid-session: the compositor's renderer
//! lost its device, and every device it opened after that was born lost. It
//! kept reopening the dead GPU, nothing was shown again, and only the cursor
//! moved. Now the renderer gives a GPU that cannot be brought back up, draws
//! in software, and tells the person.
//!
//! This kills the GPU under a real headless compositor whose simulated panel
//! presents through the DRM presenter's renderer (`TILO_HEADLESS_VBLANK_HZ`),
//! and asserts on the production path that input still reaches the desktop,
//! frames are presented again, and the person is told once.

use std::time::Duration;

use serde_json::{Value, json};
use shell::shell::state::WorkspaceTree;

use crate::support::desktop::Desktop;

/// The presenter's running count of presented frames, from the last
/// `paint-stats` line the compositor printed.
fn presented(desktop: &Desktop) -> u64 {
    desktop
        .recent_log(usize::MAX)
        .lines()
        .rev()
        .filter(|line| line.contains("paint-stats:"))
        .find_map(|line| {
            let count = line.split("present=").nth(1)?;
            count.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or(0)
}

/// Switch to the next or previous workspace with the keyboard, as a person does.
fn switch_workspace(desktop: &Desktop, key: &str) {
    for request in [
        json!({"op": "modifiers", "ctrl": false, "shift": false, "alt": false, "logo": true}),
        json!({"op": "key", "key": key}),
        json!({"op": "modifiers", "ctrl": false, "shift": false, "alt": false, "logo": false}),
    ] {
        desktop.ok(&request);
    }
}

fn graphics_notices(desktop: &Desktop) -> Vec<Value> {
    desktop.chrome()["notifications"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|notice| notice["title"].as_str() == Some("Graphics stopped responding"))
        .cloned()
        .collect()
}

#[test]
fn a_gpu_that_dies_mid_session_gives_way_to_software_and_the_person_is_told() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.append_empty_workspace("Elsewhere");
    let desktop = Desktop::start_with_env((640, 400), &tree, &[("TILO_HEADLESS_VBLANK_HZ", "60")]);
    let home = desktop.active_workspace();
    desktop.wait_for("the simulated panel to present", || presented(&desktop) > 0);

    desktop.ok(&json!({"op": "gpu_fault", "fault": "dead"}));
    // Frames to suffer it: the GPU dies under the next one.
    switch_workspace(&desktop, "PageDown");
    assert_ne!(
        desktop.active_workspace(),
        home,
        "input reaches the desktop while its GPU dies"
    );
    let given_up = "rendering in software from now on";
    desktop.wait_for_with(
        "the renderer to give the dead GPU up",
        || desktop.recent_log(40),
        || desktop.recent_log(usize::MAX).contains(given_up),
    );

    let before = presented(&desktop);
    for key in ["PageUp", "PageDown", "PageUp"] {
        switch_workspace(&desktop, key);
        desktop.pause(Duration::from_millis(300));
    }
    assert_eq!(
        desktop.active_workspace(),
        home,
        "input keeps reaching the desktop drawn in software"
    );
    desktop.wait_for_with(
        "frames presented by the software renderer",
        || format!("presented {} (before {before})", presented(&desktop)),
        || presented(&desktop) > before,
    );

    let notices = graphics_notices(&desktop);
    assert_eq!(
        notices.len(),
        1,
        "the person is told once that the desktop keeps working, slower: {notices:?}"
    );
    let log = desktop.recent_log(usize::MAX);
    assert_eq!(
        log.matches("the renderer's GPU device was lost").count(),
        1,
        "the device that showed frames is replaced once; its replacement, born lost, gives \
         the GPU up instead of being reopened, and the software renderer is never lost:\n{}",
        desktop.recent_log(60)
    );
    assert_eq!(
        log.matches(given_up).count(),
        1,
        "the GPU is given up once:\n{}",
        desktop.recent_log(60)
    );
}
