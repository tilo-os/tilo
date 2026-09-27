#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "headless geometry fixture asserts setup failures directly"
)]

//! The toolbar row's geometry, read off the retained widgets through the
//! production frame: every control shares the address field's height and
//! centre line, and a tablet-width window still leaves the address field
//! readable.

use std::collections::HashMap;

use browser::BrowserWidget;
use ui::headless::FrameHarness;
use vello::kurbo::Rect;

const URL_INPUT: &str = "browser.url_input";

fn targets_at(width: u32, height: u32) -> HashMap<String, Rect> {
    let mut widget = BrowserWidget::new_headless();
    // The same seeding the `--shots` routes use: the responsive tier has to be
    // known before the first retained layout pass.
    widget.debug_set_viewport_width(width);
    let harness = FrameHarness::new_with_theme(
        ui::State::new(),
        Box::new(widget),
        (width, height),
        ui::theme::Theme::dark(),
    );
    harness.targets()
}

/// Toolbar control alignment and responsive tiers.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn toolbar_controls_align_and_follow_width_and_text_tiers() {
    // Phase `every_toolbar_control_shares_the_address_fields_height_and_centre_line`.
    {
        let targets = targets_at(1200, 800);
        let url = targets[URL_INPUT];
        for id in [
            "browser.back",
            "browser.reload",
            "browser.forward",
            "browser.site.info",
            "browser.site.shield",
            "browser.bookmark",
            "browser.history-pause",
            "browser.library",
            "browser.pip",
            "browser.devtools",
            "browser.find",
            "browser.zoom.reset",
            "browser.private-window",
            "browser.settings",
        ] {
            let rect = targets
                .get(id)
                .unwrap_or_else(|| panic!("{id} is on the 1200px toolbar"));
            assert!(
                (rect.height() - url.height()).abs() < 0.5,
                "{id} is {} tall beside a {} tall address field",
                rect.height(),
                url.height()
            );
            assert!(
                (rect.center().y - url.center().y).abs() < 0.5,
                "{id} centre {} is off the address field's centre {}",
                rect.center().y,
                url.center().y
            );
        }
    }

    // Phase `a_tablet_width_window_keeps_a_readable_address_field`.
    {
        let targets = targets_at(768, 1024);
        let url = targets[URL_INPUT];
        assert!(
            url.width() >= 200.0,
            "the address field is starved at tablet width: {}",
            url.width()
        );
        // The commands with another route leave the row; navigation, both site
        // launchers (site information has no action route yet), library, private
        // window and settings stay.
        for id in ["browser.bookmark", "browser.history-pause", "browser.pip"] {
            assert!(
                !targets.contains_key(id),
                "{id} should yield its width to the address field below 1000px"
            );
        }
        for id in [
            "browser.back",
            "browser.forward",
            "browser.reload",
            "browser.site.info",
            "browser.site.shield",
            "browser.library",
            "browser.private-window",
            "browser.settings",
        ] {
            assert!(targets.contains_key(id), "{id} stays on the tablet toolbar");
        }
    }

    // Phase `the_roomy_tier_starts_at_a_thousand_pixels`.
    // The optional commands join the row at exactly the roomy breakpoint: one
    // pixel under it they are gone, at it they are back.
    {
        let optional = ["browser.bookmark", "browser.history-pause", "browser.pip"];
        let under = targets_at(999, 700);
        let at = targets_at(1000, 700);
        for id in optional {
            assert!(!under.contains_key(id), "{id} is still on a 999px toolbar");
            assert!(at.contains_key(id), "{id} is missing from a 1000px toolbar");
        }
        assert!(
            under[URL_INPUT].width() >= 200.0,
            "the 999px address field is starved: {}",
            under[URL_INPUT].width()
        );
    }

    // Phase `labelled_toolbar_controls_grow_with_the_text_and_the_tiers_follow`.
    // At a 2× text scale a labelled control's face grows with its label, and
    // the responsive tiers are judged on the width the text leaves: a 1200px
    // window at 2× lays out as a 600px one, keeping the address field readable
    // instead of squeezing every 1× command onto the row.
    {
        let base = targets_at(1200, 800);
        let large = targets_with_theme(1200, 800, ui::theme::Theme::dark().with_text_scale(2.0));
        for id in ["browser.site.info", "browser.private-window"] {
            let (b, l) = (base[id], large[id]);
            assert!(
                l.width() >= 1.9 * b.width(),
                "{id} is {} wide at 2x against {} at 1x — its label outgrows its face",
                l.width(),
                b.width()
            );
            assert!(
                (l.height() - large[URL_INPUT].height()).abs() < 0.5,
                "{id} leaves the row's one control height at 2x"
            );
        }
        for id in [
            "browser.bookmark",
            "browser.history-pause",
            "browser.pip",
            "browser.devtools",
        ] {
            assert!(
                !large.contains_key(id),
                "{id} stays on a 1200px toolbar at 2x, where it is a 600px toolbar to the eye"
            );
        }
        assert!(
            large[URL_INPUT].width() >= 200.0,
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
    // Phase `the_find_bar_controls_share_the_find_fields_height_and_centre_line`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_set_viewport_width(1200);
        let mut harness = FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(widget),
            (1200, 800),
            ui::theme::Theme::dark(),
        );
        // Open the find bar the way a person does: through the toolbar's Find
        // command, whose effect a following frame's sync turns into the bar.
        harness
            .perform_named("browser.find", &ui::a11y::Action::Activate)
            .expect("the Find command is on the 1200px toolbar");
        harness
            .wait_for_named("browser.find.input", tilo_test_env::hang_bound())
            .expect("activating Find composes the find bar");
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
                (rect.height() - field.height()).abs() < 0.5,
                "{id} is {} tall beside a {} tall find field",
                rect.height(),
                field.height()
            );
            assert!(
                (rect.center().y - field.center().y).abs() < 0.5,
                "{id} centre {} is off the find field's centre {}",
                rect.center().y,
                field.center().y
            );
        }
    }

    // Phase `the_download_status_shares_the_toolbar_rows_height_and_centre_line`.
    // A download in progress adds its status to the row without breaking the
    // row's one control height and centre line, and without starving the
    // address field.
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
        let url = targets[URL_INPUT];
        let status = targets["browser.download-status"];
        assert!(
            (status.height() - url.height()).abs() < 0.5,
            "the download status is {} tall beside a {} tall address field",
            status.height(),
            url.height()
        );
        assert!(
            (status.center().y - url.center().y).abs() < 0.5,
            "the download status centre {} is off the address field's centre {}",
            status.center().y,
            url.center().y
        );
        assert!(
            url.width() >= 200.0,
            "the address field is starved once a download is showing: {}",
            url.width()
        );
        // The optional commands yield to the status first; Find and the zoom
        // readout keep their tier, whatever is downloading.
        for id in ["browser.bookmark", "browser.history-pause", "browser.pip"] {
            assert!(
                !targets.contains_key(id),
                "{id} should yield its width to the download status at 1200px"
            );
        }
        for id in ["browser.devtools", "browser.find", "browser.zoom.reset"] {
            assert!(
                targets.contains_key(id),
                "{id} must not depend on whether a download is running"
            );
        }
    }
}

fn targets_with_theme(width: u32, height: u32, theme: ui::theme::Theme) -> HashMap<String, Rect> {
    let mut widget = BrowserWidget::new_headless();
    widget.debug_set_viewport_width(width);
    let harness =
        FrameHarness::new_with_theme(ui::State::new(), Box::new(widget), (width, height), theme);
    harness.targets()
}
