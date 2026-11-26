//! Settings over a device or session that could not read what a pane is about: the pane says
//! that it could not be read, rather than drawing every field of the reading as the empty value
//! its type starts with — a sound with no outputs, a battery at no per cent, a network that is off.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use support::{open, open_pane, said, set_reading, settle, shows};

/// A window people use: a laptop's own size at scale one.
const LAPTOP: (u32, u32) = (1280, 800);

/// Set a resource of the device to one that says it could not be read, as the session publishes
/// it when the device or its service did not answer.
fn could_not_be_read(window: &tilolang_window::Window, id: &str, ty: &str, why: &str) {
    set_reading(
        window,
        id,
        ty,
        &format!("{{\"{}\":\"{why}\"}}", tilolang_runtime::store::UNREADABLE),
    );
}

/// The Sound pane over a device that could not be read: it says so in one row, in place of the
/// output, input and meter sections it would draw from a reading that answered.
#[test]
fn a_pane_whose_device_could_not_be_read_says_so() {
    let mut window = open(LAPTOP, tilo_ui_base::Theme::light());
    open_pane(&mut window, "sound");
    assert!(
        shows(&window, "Output"),
        "Sound answers first: {:?}",
        said(&window)
    );
    assert!(!shows(&window, "could not be read"));

    could_not_be_read(
        &window,
        "sound",
        "so.tilo.device#Sound",
        "no sound service answered",
    );
    settle(&mut window);

    assert!(
        shows(&window, "Sound could not be read"),
        "the pane says the sound could not be read: {:?}",
        said(&window)
    );
    assert!(
        shows(&window, "No sound service answered"),
        "with the reason the device gave: {:?}",
        said(&window)
    );
    assert!(
        !shows(&window, "Output"),
        "and draws no section over an empty reading: {:?}",
        said(&window)
    );

    // The reading answers again: the pane draws what it says.
    set_reading(&window, "sound", "so.tilo.device#Sound", "{}");
    settle(&mut window);
    assert!(
        shows(&window, "Output"),
        "the pane draws again once the reading answers: {:?}",
        said(&window)
    );
    assert!(!shows(&window, "could not be read"));
}

/// The Power pane reads a resource of its own, beside the battery's: one that could not be read is
/// said in the pane's own words.
#[test]
fn a_pane_says_a_reading_of_its_own_could_not_be_read() {
    let mut window = open(LAPTOP, tilo_ui_base::Theme::light());
    open_pane(&mut window, "power");
    assert!(shows(&window, "Power mode"), "{:?}", said(&window));

    could_not_be_read(
        &window,
        "power",
        "so.tilo.device#Power",
        "the power service did not answer",
    );
    settle(&mut window);

    assert!(
        shows(&window, "Power could not be read"),
        "{:?}",
        said(&window)
    );
    assert!(
        !shows(&window, "Power mode"),
        "its settings are not drawn over the failed reading: {:?}",
        said(&window)
    );
}

/// A pane over the person's own settings, published by the session as well, says the same when
/// they could not be read.
#[test]
fn a_settings_pane_says_it_could_not_be_read() {
    let mut window = open(LAPTOP, tilo_ui_base::Theme::light());
    let settings_pane = "Accessibility";
    open_pane(&mut window, "accessibility");
    assert!(
        shows(&window, "Screen reader"),
        "{settings_pane} answers first: {:?}",
        said(&window)
    );

    could_not_be_read(
        &window,
        "settings/accessibility",
        "so.tilo.settings.sections#Accessibility",
        "the settings service did not answer",
    );
    settle(&mut window);
    assert!(
        shows(&window, "Accessibility could not be read"),
        "{:?}",
        said(&window)
    );
}
