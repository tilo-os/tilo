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

#[test]
fn every_toolbar_control_shares_the_address_fields_height_and_centre_line() {
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

#[test]
fn a_tablet_width_window_keeps_a_readable_address_field() {
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

#[test]
fn the_find_bar_controls_share_the_find_fields_height_and_centre_line() {
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
        .wait_for_named("browser.find.input", std::time::Duration::from_secs(2))
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
