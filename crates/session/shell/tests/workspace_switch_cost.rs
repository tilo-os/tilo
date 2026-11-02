//! Returning to an unchanged workspace must do no cold construction work.
//!
//! Before retained workspace content, every return built a new surface and
//! resolved every tile again, losing the windows' places and widget caches.
//! Count those production factory calls, not elapsed time or cache decisions.
//! The headless production compositor restores a workspace showing Notes and
//! Files, each the app's own process started through the real launcher, and
//! handles the same Super+PageUp/PageDown chords as a person. Returning keeps
//! every app's window: the same surfaces stay bound into their tabs.

use crate::idle_wakeups::first_party_apps::{FILES, NOTES};
use crate::idle_wakeups::{Desktop, app_window_tab, desktop_with_tabs, tile_of};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    surfaces: u64,
    tiles: u64,
    mounts: u64,
}

impl Work {
    const ZERO: Self = Self {
        surfaces: 0,
        tiles: 0,
        mounts: 0,
    };

    fn since(self, before: Self) -> Self {
        Self {
            surfaces: self.surfaces.checked_sub(before.surfaces).unwrap(),
            tiles: self.tiles.checked_sub(before.tiles).unwrap(),
            mounts: self.mounts.checked_sub(before.mounts).unwrap(),
        }
    }
}

fn state(desktop: &Desktop) -> serde_json::Value {
    let reply = desktop.control(&serde_json::json!({"op": "state_dump"}));
    assert!(reply.get("err").is_none(), "state dump refused: {reply}");
    reply.get("state").unwrap_or(&reply).clone()
}

fn work(state: &serde_json::Value) -> Work {
    let counts = &state["chrome"]["workspace_work"];
    Work {
        surfaces: counts["surface_builds"]
            .as_u64()
            .expect("surface factory count"),
        tiles: counts["tile_resolutions"]
            .as_u64()
            .expect("tile resolver count"),
        mounts: counts["app_mounts"].as_u64().expect("app mount count"),
    }
}

fn switch(desktop: &Desktop, key: &str, to: &serde_json::Value) -> Work {
    for request in [
        serde_json::json!({"op": "modifiers", "ctrl": false, "shift": false, "alt": false, "logo": true}),
        serde_json::json!({"op": "key", "key": key}),
        serde_json::json!({"op": "modifiers", "ctrl": false, "shift": false, "alt": false, "logo": false}),
    ] {
        let reply = desktop.control(&request);
        assert!(reply.get("err").is_none(), "{request} refused: {reply}");
    }
    // The control dispatcher settles layout before reading diagnostic state.
    // Assert the reconciled content, not just the navigation target: no sleep,
    // trace flush interval, animation sampling window, or quiet-time heuristic.
    let state = state(desktop);
    assert_eq!(&state["workspace_tree"]["active"], to);
    assert_eq!(&state["chrome"]["workspace_content"], to);
    work(&state)
}

/// The work counted once `done` holds of it: a mount waits on its room's
/// record opening, which no input announces.
fn settled(desktop: &Desktop, what: &str, done: impl Fn(Work) -> bool) -> Work {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let counted = work(&state(desktop));
        if done(counted) {
            return counted;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "waited for {what}: {counted:?}\n{}",
            desktop.log_tail()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// What the Notes and Files tabs show: each app's window, bound by its
/// surface.
fn app_windows(desktop: &Desktop) -> Vec<serde_json::Value> {
    let state = state(desktop);
    let tree = &state["workspace_tree"];
    ["so.tilo.notes", "so.tilo.file-explorer"]
        .iter()
        .map(|app| {
            let tab = app_window_tab(tree, app)
                .unwrap_or_else(|| panic!("{app}'s window left its tab: {tree}"));
            tile_of(tree, tab)
        })
        .collect()
}

#[test]
fn switching_back_to_a_workspace_with_apps_does_no_construction_work() {
    let mut tree = desktop_with_tabs(&["Notes", "Files"]);
    tree.create_hosted_workspace("Elsewhere");
    let desktop = Desktop::start_with_apps(tree, &[NOTES, FILES]);
    desktop.wait_for_app_window("so.tilo.notes");
    desktop.wait_for_app_window("so.tilo.file-explorer");
    let windows = app_windows(&desktop);
    // The shell's own app (the one its actions are published by) is mounted
    // in the room of the workspace in front once that room's record is open.
    settled(&desktop, "the shell's app in the first room", |work| {
        work.mounts >= 1
    });

    let initial = state(&desktop);
    let tree = &initial["workspace_tree"];
    let home = &tree["active"];
    let elsewhere = tree["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| &workspace["id"])
        .find(|id| *id != home)
        .expect("a second workspace");
    let before = work(&initial);
    assert!(
        before.surfaces > 0 && before.tiles >= 2,
        "the production factories must be observed at startup: {before:?}"
    );

    // The positive control: a workspace never in front builds its surface,
    // and the shell's app is mounted in its room (since ca87a9e7020 the
    // shell publishes its actions as its own app, in the room in front).
    switch(&desktop, "PageDown", elsewhere);
    let cold = settled(&desktop, "the shell's app in the second room", |work| {
        work.mounts > before.mounts
    });
    let cold_delta = cold.since(before);
    eprintln!("WORKSPACE-SWITCH cold={cold_delta:?}");
    assert_eq!(
        cold_delta,
        Work {
            surfaces: 1,
            tiles: 0,
            mounts: 1
        }
    );
    let mut previous = cold;
    let frames = tempfile::tempdir().expect("private switch frame directory");
    let frame = frames.path().join("back.png");
    // Every observation is exact. Repeated rounds cannot average away even
    // one remount or rebuild; a cold empty workspace is the positive control.
    for round in 0..32 {
        let back = switch(&desktop, "PageUp", home);
        let back_delta = back.since(previous);
        eprintln!("WORKSPACE-SWITCH round={round} back={back_delta:?}");
        assert_eq!(
            back_delta,
            Work::ZERO,
            "returning must preserve the apps' windows and widget caches"
        );
        assert_eq!(
            app_windows(&desktop),
            windows,
            "returning must keep each app's window bound into its tab"
        );
        if frame.exists() {
            std::fs::remove_file(&frame).expect("remove the preceding switch frame");
        }
        // Snapshot completion acknowledges a submitted production frame, so
        // layout-only success cannot hide a workspace that never paints.
        let painted = desktop.control(&serde_json::json!({"op": "snapshot", "path": frame}));
        assert!(
            painted.get("err").is_none(),
            "switch frame refused: {painted}"
        );
        assert!(frame.is_file(), "the switch-back frame was not observed");
        let there = switch(&desktop, "PageDown", elsewhere);
        let there_delta = there.since(back);
        eprintln!("WORKSPACE-SWITCH round={round} there={there_delta:?}");
        assert_eq!(
            there_delta,
            Work::ZERO,
            "the visited empty workspace is warm too"
        );
        previous = there;
    }
    assert!(
        !desktop.logged("file lease is busy"),
        "the compositor must own its state writer:\n{}",
        desktop.log_tail()
    );
}
