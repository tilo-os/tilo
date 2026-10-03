#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "headless geometry fixture asserts setup failures directly"
)]

//! The toolbar row's geometry, read off the retained widgets through the
//! production frame: every control sits on the address field's centre line,
//! and a narrow window moves commands into the overflow menu instead of
//! starving the address field.

use std::collections::HashMap;

use browser::BrowserWidget;
use ui::headless::FrameHarness;
use vello::kurbo::Rect;

const URL_INPUT: &str = "browser.url_input";

fn targets_at(width: u32, height: u32) -> HashMap<String, Rect> {
    targets_with_theme(width, height, ui::theme::Theme::dark())
}

fn targets_with_theme(width: u32, height: u32, theme: ui::theme::Theme) -> HashMap<String, Rect> {
    let mut widget = BrowserWidget::new_headless();
    // The same seeding the `--shots` routes use: the responsive tier has to be
    // known before the first retained layout pass.
    widget.debug_set_viewport_width(width);
    let harness =
        FrameHarness::new_with_theme(ui::State::new(), Box::new(widget), (width, height), theme);
    harness.targets()
}

fn assert_on_centre_line(targets: &HashMap<String, Rect>, ids: &[&str]) {
    let url = targets[URL_INPUT];
    for id in ids {
        let rect = targets
            .get(*id)
            .unwrap_or_else(|| panic!("{id} is on the toolbar"));
        assert!(
            (rect.center().y - url.center().y).abs() < 0.5,
            "{id} centre {} is off the address field's centre {}",
            rect.center().y,
            url.center().y
        );
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= targets["browser.root"].x1,
            "{id} stays inside the window"
        );
    }
}

/// Toolbar control alignment and responsive tiers.
#[test]
fn toolbar_controls_align_and_move_into_the_overflow_menu_when_narrow() {
    const ROW: [&str; 6] = [
        "browser.back",
        "browser.forward",
        "browser.reload",
        "browser.site.info",
        "browser.bookmark",
        "browser.library",
    ];

    // Phase `every_toolbar_control_shares_the_address_fields_centre_line`.
    {
        let targets = targets_at(1200, 800);
        assert_on_centre_line(&targets, &ROW);
        for id in ROW {
            assert!(
                (targets[id].width() - targets[id].height()).abs() < 0.5,
                "{id} is a square icon button"
            );
        }
        // The overflow commands are menu rows, not row controls.
        for id in [
            "browser.devtools",
            "browser.find",
            "browser.zoom.reset",
            "browser.pip",
            "browser.history-pause",
            "browser.private-window",
            "browser.settings",
        ] {
            assert!(!targets.contains_key(id), "{id} lives in the overflow menu");
        }
    }

    // Phase `a_tablet_width_window_keeps_every_primary_command_and_a_readable_address_field`.
    {
        let targets = targets_at(768, 1024);
        for id in ROW {
            assert!(targets.contains_key(id), "{id} stays on the tablet toolbar");
        }
        assert!(
            targets[URL_INPUT].width() >= 400.0,
            "the address field is starved at tablet width: {}",
            targets[URL_INPUT].width()
        );
    }

    // Phase `a_compact_window_keeps_navigation_and_moves_the_rest_to_the_menu`.
    {
        let targets = targets_at(390, 800);
        assert_on_centre_line(
            &targets,
            &["browser.back", "browser.forward", "browser.reload"],
        );
        for id in ["browser.site.info", "browser.bookmark", "browser.library"] {
            assert!(
                !targets.contains_key(id),
                "{id} joins the overflow menu in a compact window"
            );
        }
        assert!(
            targets[URL_INPUT].width() >= 150.0,
            "the compact address field is starved: {}",
            targets[URL_INPUT].width()
        );
    }

    // Phase `the_compact_tier_is_judged_on_the_width_the_text_leaves`.
    // A 1100px window at 2x text lays out as the 550px window it is to the
    // eye: the same commands move into the menu and the address field stays
    // readable.
    {
        let large = targets_with_theme(1100, 800, ui::theme::Theme::dark().with_text_scale(2.0));
        assert!(
            !large.contains_key("browser.bookmark"),
            "bookmark joins the menu at 2x text"
        );
        assert!(
            large[URL_INPUT].width() >= 150.0,
            "the 2x address field is starved: {}",
            large[URL_INPUT].width()
        );
    }
}

/// Find bar and download status alignment.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn find_bar_and_download_status_share_the_toolbar_row_geometry() {
    // Phase `the_find_bar_controls_share_the_find_fields_centre_line`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_set_viewport_width(1200);
        let mut harness = FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(widget),
            (1200, 800),
            ui::theme::Theme::dark(),
        );
        // Open the find bar the way a person does: with the Find shortcut,
        // which a following frame's sync turns into the bar.
        harness.set_modifiers(&["ctrl".to_owned()]);
        harness.key("f");
        harness.set_modifiers(&[]);
        harness
            .wait_for_named("browser.find.input", tilo_test_env::hang_bound())
            .expect("Ctrl+F composes the find bar");
        let targets = harness.targets();
        let field = targets["browser.find.input"];
        for id in [
            "browser.find.previous",
            "browser.find.next",
            "browser.find.close",
        ] {
            let rect = targets
                .get(id)
                .unwrap_or_else(|| panic!("{id} is on the find bar"));
            assert!(
                (rect.center().y - field.center().y).abs() < 0.5,
                "{id} centre {} is off the find field's centre {}",
                rect.center().y,
                field.center().y
            );
        }
    }

    // Phase `the_download_status_shares_the_toolbar_rows_centre_line`.
    // A download in progress adds its status to the row without breaking the
    // row's centre line, and without starving the address field.
    {
        use ui::webview::{FrameSlot, WebViewCommand, WebViewEngine, WebViewEvent};

        /// The test engine, with one download announced on the first drain.
        struct Downloading {
            inner: std::sync::Arc<ui::webview::test_engine::TestEngine>,
            pending: std::sync::Mutex<Vec<WebViewEvent>>,
        }
        impl WebViewEngine for Downloading {
            fn send(&self, cmd: WebViewCommand) {
                self.inner.send(cmd);
            }
            fn frames(&self) -> FrameSlot {
                self.inner.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                let mut events = self.inner.drain_events();
                events.append(&mut self.pending.lock().expect("event queue"));
                events
            }
        }

        let engine = std::sync::Arc::new(Downloading {
            inner: ui::webview::test_engine::TestEngine::new(),
            pending: std::sync::Mutex::new(vec![WebViewEvent::NativeDownloadRequested {
                id: 7,
                url: "https://example.test/archive.tar".into(),
                name: "archive.tar".into(),
            }]),
        });
        let mut widget = BrowserWidget::with_engine(engine, true);
        widget.debug_set_viewport_width(1200);
        let mut harness = FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(widget),
            (1200, 800),
            ui::theme::Theme::dark(),
        );
        harness
            .wait_for_named("browser.download-status", tilo_test_env::hang_bound())
            .expect("a requested download puts its status on the toolbar");
        let targets = harness.targets();
        assert_on_centre_line(&targets, &["browser.download-status"]);
        assert!(
            targets[URL_INPUT].width() >= 400.0,
            "the address field is starved once a download is showing: {}",
            targets[URL_INPUT].width()
        );
        for id in [
            "browser.back",
            "browser.forward",
            "browser.reload",
            "browser.bookmark",
            "browser.library",
        ] {
            assert!(
                targets.contains_key(id),
                "{id} must not depend on whether a download is running"
            );
        }
    }
}
