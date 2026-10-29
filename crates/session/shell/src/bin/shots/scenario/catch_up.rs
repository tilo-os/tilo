//! Recording the catch-up walkthrough: the workspace's activity opens,
//! the summary scrolls, a group of an agent's changes opens, its before and
//! after is compared, and the work is undone.
//!
//! The window the recording drives is the Activity app's own, not a stand-in
//! drawn for the video, so what the frames show is what the app draws.

use super::{Action, OwnApp, Scenario, ShellDriver};

/// The walkthrough, for the recording to run.
pub fn scenario() -> Scenario {
    #[cfg(feature = "test-support")]
    Scenario {
        name: "catch_up_walkthrough",
        description: "Catching back up: the workspace's activity opens, the summary scrolls, an agent's group of changes opens, its before and after is compared, and the work is undone.",
        actions: vec![
            Action::OwnProcessWindow {
                app: OwnApp::Activity,
                width: 780,
            },
            Action::Wait(6),
            Action::Relayout,
            Action::Snapshot("01-whats-new"),
            // The summary is longer than the window: read on down it.
            Action::OwnProcessScroll {
                surface: 9_303,
                dy: -240.0,
            },
            Action::Wait(4),
            Action::Relayout,
            Action::Snapshot("02-scrolled"),
            // The first card is what Ada and her agent changed.
            Action::OwnProcessClick {
                surface: 9_303,
                target: "activity-brief-0",
            },
            Action::Wait(6),
            Action::Relayout,
            Action::Snapshot("03-group-open"),
            // Its changes, in History, side by side.
            Action::OwnProcessClick {
                surface: 9_303,
                target: "activity-brief-0-history",
            },
            Action::Wait(6),
            Action::Relayout,
            Action::OwnProcessClick {
                surface: 9_303,
                target: "activity-offer-detail",
            },
            Action::Wait(6),
            Action::Relayout,
            Action::Snapshot("04-compare"),
            Action::OwnProcessClick {
                surface: 9_303,
                target: "activity-offer-undo",
            },
            Action::Wait(6),
            Action::Relayout,
            Action::Snapshot("05-undone"),
        ],
    }
}

/// Click `target` in the app window on `surface`, naming what the window
/// does offer when it has no such target.
pub fn click(driver: &mut ShellDriver, surface: u64, target: &str) {
    if !driver.click_in_own_process_app(surface, target) {
        let mut names: Vec<String> = driver
            .own_process_app(surface)
            .map(|app| app.targets().into_keys().collect())
            .unwrap_or_default();
        names.sort();
        eprintln!("scenario: the app window has no {target:?} to click; it offers {names:?}");
    }
}

/// Turn the wheel `dy` notches in the app window on `surface`.
pub fn scroll(driver: &mut ShellDriver, surface: u64, dy: f32) {
    if !driver.scroll_in_own_process_app(surface, dy) {
        eprintln!("scenario: the app window would not scroll");
    }
}

#[cfg(test)]
mod tests {
    use super::super::{OWN_ACTIVITY, SnapshotSink, lookup, run};
    use super::*;

    /// A sink that also records what the Activity window shows at each
    /// snapshot, so a walkthrough test can prove each step landed.
    struct ActivitySink {
        seen: Vec<(String, Vec<String>)>,
    }
    impl SnapshotSink for ActivitySink {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            let shown = driver
                .own_process_app(OWN_ACTIVITY.surface_id)
                .map(|app| app.targets().into_keys().collect())
                .unwrap_or_default();
            self.seen.push((name.to_string(), shown));
        }
    }

    /// The catch-up walkthrough shows the summary, opens a group of an
    /// agent's changes, compares what they changed and undoes them: the
    /// window it drives is the Activity app's own, so every frame is the
    /// app's, not a stand-in drawn for the video.
    #[cfg(feature = "test-support")]
    #[test]
    fn catch_up_walkthrough_drives_the_activity_window() {
        let mut d = ShellDriver::new((1600, 900));
        let mut sink = ActivitySink { seen: Vec::new() };
        let scenario = lookup("catch_up_walkthrough").expect("catch_up_walkthrough in catalogue");
        run(&mut d, &scenario, &mut sink);
        let names: Vec<&str> = sink.seen.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "01-whats-new",
                "02-scrolled",
                "03-group-open",
                "04-compare",
                "05-undone"
            ],
            "the walkthrough captures every step it takes"
        );
        let shown = |beat: &str, target: &str| {
            sink.seen
                .iter()
                .find(|(name, _)| name == beat)
                .is_some_and(|(_, shown)| shown.iter().any(|name| name == target))
        };
        // The frame it opens on is the summary itself: the narrative, the
        // first group of changes, and the offer to open it in History.
        for target in [
            "activity-narrative",
            "activity-brief-0",
            "activity-brief-0-history",
        ] {
            assert!(
                shown("01-whats-new", target),
                "the walkthrough opens on the summary ({target:?})"
            );
        }
        // The last frame is the group's own page in History: its changes,
        // and the offers to compare or undo them.
        for target in [
            "activity-change-0",
            "activity-offer-detail",
            "activity-offer-undo",
        ] {
            assert!(
                shown("05-undone", target),
                "the walkthrough ends on the group's changes in History ({target:?})"
            );
        }
        // The window paints the app, not an empty ground.
        let snapshot = shell::shell::foreign::store()
            .snapshot(OWN_ACTIVITY.surface_id)
            .expect("the window published its frame");
        let distinct: std::collections::HashSet<u32> = snapshot
            .rgba
            .chunks_exact(4)
            .map(|pixel| u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]))
            .collect();
        assert!(
            distinct.len() > 100,
            "the window draws the app's own pixels, not one flat colour"
        );
    }
}
