//! Scenarios of the window's loading line ([`ui::window_loading`]): a page
//! loading, an app busy, a load finishing, and a window covering the
//! display; stills and every frame for recordings.

#[allow(
    clippy::wildcard_imports,
    reason = "these scenarios are rows of the parent's catalogue, built from its actions and helpers"
)]
use super::*;

/// `Action::LoadingFrames` — split out of [`apply`] to keep that dispatcher
/// short.
pub(super) fn apply_loading_frames<S: SnapshotSink>(
    driver: &mut ShellDriver,
    action: &Action,
    sink: &mut S,
) {
    let Action::LoadingFrames {
        prefix,
        surface_id,
        steps,
        from_ms,
        frames,
        every_ms,
    } = action
    else {
        return;
    };
    #[cfg(not(feature = "test-support"))]
    {
        let _ = (
            driver, sink, prefix, surface_id, steps, from_ms, frames, every_ms,
        );
        eprintln!(
            "scenario: LoadingFrames pins the toolkit's clock, which needs \
             `--features test-support`; skipping"
        );
    }
    #[cfg(feature = "test-support")]
    {
        let Some(tab) = tab_showing_surface(driver, *surface_id) else {
            eprintln!("scenario: no window shows surface {surface_id}");
            return;
        };
        driver.relayout();
        let start = ui::time::now();
        let at = |ms: u64| {
            start
                .checked_add(std::time::Duration::from_millis(ms))
                .unwrap_or(start)
        };
        let mut reported = 0;
        for i in 0..*frames {
            let frame_ms = from_ms.saturating_add(every_ms.saturating_mul(u64::from(i)));
            // Each report lands at its own instant, as the app sent it.
            while let Some(&(ms, loading)) = steps.get(reported) {
                if ms > frame_ms {
                    break;
                }
                ui::time::testing::with_monotonic_time(at(ms), || {
                    driver.host_mut().report_tab_loading_for_tests(tab, loading);
                });
                reported += 1;
            }
            ui::time::testing::with_monotonic_time(at(frame_ms), || {
                driver.pump_apps();
                driver.relayout();
                let name: &'static str = Box::leak(format!("{prefix}-{i:03}").into_boxed_str());
                sink.capture(driver, name);
            });
        }
    }
}

/// A page loading as the browser's engine reports it: busy until the first
/// progress, then its steps, then done.
const PAGE_LOAD: &[(u64, ui::window_loading::WindowLoading)] = &[
    (0, ui::window_loading::WindowLoading::Busy),
    (
        240,
        ui::window_loading::WindowLoading::Progress { fraction: 0.18 },
    ),
    (
        520,
        ui::window_loading::WindowLoading::Progress { fraction: 0.34 },
    ),
    (
        860,
        ui::window_loading::WindowLoading::Progress { fraction: 0.52 },
    ),
    (
        1300,
        ui::window_loading::WindowLoading::Progress { fraction: 0.71 },
    ),
    (
        1750,
        ui::window_loading::WindowLoading::Progress { fraction: 0.9 },
    ),
    (2100, ui::window_loading::WindowLoading::Idle),
];

/// An app busy with no progress known (Files reading a large folder).
const BUSY: &[(u64, ui::window_loading::WindowLoading)] =
    &[(0, ui::window_loading::WindowLoading::Busy)];

/// A load that ends while its bar is part way: it completes, then fades.
const FINISH: &[(u64, ui::window_loading::WindowLoading)] = &[
    (
        0,
        ui::window_loading::WindowLoading::Progress { fraction: 0.62 },
    ),
    (900, ui::window_loading::WindowLoading::Idle),
];

/// The seeded window, settled.
fn loading_window(tabs: &'static [BarTab]) -> Vec<Action> {
    vec![
        Action::SeedWindowBar {
            tabs,
            active: 0,
            width: 700,
        },
        Action::Wait(30),
        Action::Relayout,
    ]
}

pub(super) fn window_loading_browser_scenario() -> Scenario {
    let mut actions = loading_window(&[BAR_BROWSER]);
    actions.push(Action::Snapshot("00-at-rest"));
    actions.push(Action::LoadingFrames {
        prefix: "01-page-loading",
        surface_id: BAR_BROWSER.surface_id,
        steps: PAGE_LOAD,
        from_ms: 400,
        frames: 6,
        every_ms: 300,
    });
    Scenario {
        name: "window-loading-browser",
        description: "A page loading in the browser: the window's loading line under its bar, \
             busy until the engine reports progress, then an accent bar growing from \
             the left with each report.",
        actions,
    }
}

pub(super) fn window_loading_busy_scenario() -> Scenario {
    let mut actions = loading_window(&[BAR_FILES]);
    actions.push(Action::LoadingFrames {
        prefix: "01-busy",
        surface_id: BAR_FILES.surface_id,
        steps: BUSY,
        from_ms: 300,
        frames: 6,
        every_ms: 250,
    });
    Scenario {
        name: "window-loading-busy",
        description: "Files busy reading a large folder, with no progress known: a soft accent \
             highlight sweeping left to right across a faint track under the bar.",
        actions,
    }
}

pub(super) fn window_loading_finish_scenario() -> Scenario {
    let mut actions = loading_window(&[BAR_BROWSER]);
    actions.push(Action::LoadingFrames {
        prefix: "01-finishing",
        surface_id: BAR_BROWSER.surface_id,
        steps: FINISH,
        from_ms: 860,
        frames: 9,
        every_ms: 55,
    });
    Scenario {
        name: "window-loading-finish",
        description: "A load ending part way: the bar completes to the end quickly, then the \
             line fades out and takes no room.",
        actions,
    }
}

/// The browser's window covering the display (Super+F), settled.
fn fullscreen_loading_window() -> Vec<Action> {
    let mut actions = loading_window(&[BAR_BROWSER]);
    actions.extend([
        Action::Modifiers(ModifiersState::SUPER),
        Action::Key(Key::Character("f".into())),
        Action::Modifiers(ModifiersState::empty()),
        Action::Wait(PLACEMENT_SETTLE_FRAMES),
        Action::Relayout,
    ]);
    actions
}

pub(super) fn window_loading_fullscreen_scenario() -> Scenario {
    let mut actions = fullscreen_loading_window();
    actions.push(Action::LoadingFrames {
        prefix: "01-fullscreen",
        surface_id: BAR_BROWSER.surface_id,
        steps: PAGE_LOAD,
        from_ms: 700,
        frames: 3,
        every_ms: 500,
    });
    Scenario {
        name: "window-loading-fullscreen",
        description: "A page loading in a window covering the display: with no bar, the line \
             stands at the top edge and runs edge to edge.",
        actions,
    }
}

pub(super) fn window_loading_browser_motion_scenario() -> Scenario {
    let mut actions = loading_window(&[BAR_BROWSER]);
    actions.push(Action::LoadingFrames {
        prefix: "01-page-loading",
        surface_id: BAR_BROWSER.surface_id,
        steps: PAGE_LOAD,
        from_ms: 0,
        frames: 170,
        every_ms: 16,
    });
    Scenario {
        name: "window-loading-browser-motion",
        description: "Every frame of a page load, at 60 frames a second, for a recording: the \
             delay, the sweep, the growing bar, the finish and the fade.",
        actions,
    }
}

pub(super) fn window_loading_busy_motion_scenario() -> Scenario {
    let mut actions = loading_window(&[BAR_FILES]);
    actions.push(Action::LoadingFrames {
        prefix: "01-busy",
        surface_id: BAR_FILES.surface_id,
        steps: BUSY,
        from_ms: 0,
        frames: 200,
        every_ms: 16,
    });
    Scenario {
        name: "window-loading-busy-motion",
        description: "Every frame of Files busy, at 60 frames a second, for a recording: two \
             passes of the sweep.",
        actions,
    }
}

pub(super) fn window_loading_fullscreen_motion_scenario() -> Scenario {
    let mut actions = fullscreen_loading_window();
    actions.push(Action::LoadingFrames {
        prefix: "01-fullscreen",
        surface_id: BAR_BROWSER.surface_id,
        steps: PAGE_LOAD,
        from_ms: 0,
        frames: 170,
        every_ms: 16,
    });
    Scenario {
        name: "window-loading-fullscreen-motion",
        description: "Every frame of a page load in a window covering the display, for a \
             recording.",
        actions,
    }
}

pub(super) fn own_process_store_window_bar_scenario() -> Scenario {
    Scenario {
        name: "own_process_store_window_bar",
        description: "The App Store as a window of its own on its shell-ui connection, as on a \
             device: its window's bar hosts its search and History, as Files' and Notes' do.",
        actions: vec![
            Action::OwnProcessWindow {
                app: OwnApp::Store,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-store"),
        ],
    }
}
