//! Returning to an unchanged workspace must do no cold construction work.
//!
//! Before retained workspace content, every return built a new surface and
//! resolved every tile again, remounting applications and losing widget caches.
//! Count those production factory calls, not elapsed time or cache decisions.
//! The headless production compositor restores Notes, opens Terminal through
//! its action, and handles the same Super+PageUp/PageDown chords as a person.

use crate::idle_wakeups::{Desktop, notes_desktop};

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

#[test]
fn switching_back_to_a_workspace_with_apps_does_no_construction_work() {
    let mut tree = notes_desktop();
    tree.append_empty_workspace("Elsewhere");
    let desktop = Desktop::start_with(tree);
    desktop.wait_for("the restored Notes notebook", |targets| {
        targets.iter().any(|name| name == "notes.first_note")
    });
    let reply = desktop.control(&serde_json::json!({
        "op": "actions_invoke", "provider": "actions", "id": "window.new.terminal",
    }));
    assert!(reply.get("err").is_none(), "Terminal refused: {reply}");
    desktop.wait_for("the Terminal", |targets| {
        targets.iter().any(|name| name == "terminal")
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
        before.surfaces > 0 && before.tiles >= 2 && before.mounts > 0,
        "the production factories must be observed at startup: {before:?}"
    );

    let cold = switch(&desktop, "PageDown", elsewhere);
    let cold_delta = cold.since(before);
    eprintln!("WORKSPACE-SWITCH cold={cold_delta:?}");
    assert_eq!(
        cold_delta,
        Work {
            surfaces: 1,
            tiles: 0,
            mounts: 0
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
            "returning must preserve mounted apps and widget caches"
        );
        desktop.wait_for("both retained applications", |targets| {
            targets.iter().any(|name| name == "notes.first_note")
                && targets.iter().any(|name| name == "terminal")
        });
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
