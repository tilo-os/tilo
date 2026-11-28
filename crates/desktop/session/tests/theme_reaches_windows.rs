//! A person switches to dark mode with a tilolang app's window open: the window's next frame is
//! drawn in the dark theme, with its content still on it.
//!
//! The session is the real one its device's daemon starts, with its own loop; the app is a real
//! tilolang package in a window process of its own; the switch is the person's action on their
//! settings, as Quick Settings and `tilo do set-dark-mode` ask it. Nothing here steps the session
//! by hand, so a change that reaches a window only when something else happens to wake the
//! session or the app fails here.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "test fixtures: a failed step is the assertion, and the device lives until the \
              session ends"
)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{Device, Person};

/// A tilolang app whose window shows controls, text and lists from its first frame.
const APP: &str = crate::common::people::GALLERY;

/// How long a theme switch may take to reach the window's frame: a person sees it at once.
const SWITCH: Duration = Duration::from_secs(2);

/// The app's window as the device lists it now.
fn window(device: &Device) -> Value {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["app_id"] == APP).cloned())
        .unwrap_or_else(|| panic!("{} lists {APP}'s window", device.label))
}

/// How many frames the app's window has drawn.
fn frames(device: &Device) -> u64 {
    window(device)["frames"].as_u64().unwrap_or(0)
}

/// What the window shows now, as the pixels of the display inside its rect.
struct Look {
    /// The mean luminance of the window's pixels, 0 to 255.
    luminance: f64,
    /// How many of its pixels are not its most common colour: what is drawn on its background.
    drawn: usize,
    pixels: usize,
}

/// The window as the display shows it now.
fn look(device: &Device, name: &str) -> Look {
    let rect = window(device)["rect"].clone();
    let at = |index: usize| {
        rect[index]
            .as_f64()
            .unwrap_or_else(|| panic!("{APP}'s window stands somewhere: {rect}"))
    };
    let path = device.directory().join(format!("{name}.png"));
    let answer = device.ask(&json!({"op": "snapshot", "path": path}));
    assert!(
        answer.get("err").is_none(),
        "{} shows its display: {answer}",
        device.label
    );
    let shot = image::open(&path)
        .unwrap_or_else(|error| panic!("{} reads: {error}", path.display()))
        .to_rgba8();
    // Inside the shell's edge around the window: only what the app drew.
    let inset = 8.0;
    let (x0, y0) = ((at(0) + inset) as u32, (at(1) + inset) as u32);
    let x1 = ((at(0) + at(2) - inset) as u32).min(shot.width());
    let y1 = ((at(1) + at(3) - inset) as u32).min(shot.height());
    let mut colours = std::collections::HashMap::<[u8; 3], usize>::new();
    let mut sum = 0.0;
    let mut pixels = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let [r, g, b, _] = shot.get_pixel(x, y).0;
            sum += 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
            *colours.entry([r, g, b]).or_default() += 1;
            pixels += 1;
        }
    }
    assert!(pixels > 10_000, "{APP}'s window is shown at a size: {rect}");
    let background = colours.values().copied().max().unwrap_or(0);
    Look {
        luminance: sum / pixels as f64,
        drawn: pixels - background,
        pixels,
    }
}

/// Wait until the window has drawn the same number of frames for a while: what it shows is what
/// it settled on.
fn settled(device: &Device) -> u64 {
    let mut last = frames(device);
    let mut since = Instant::now();
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let now = frames(device);
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(800) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "{APP}'s window settles: it is still drawing at frame {now}"
        );
    }
}

/// The person switches dark mode `on` with `switch`, as Quick Settings does, and the window's
/// next frame is drawn within [`SWITCH`] of it: how the window looks once it settled.
fn switched(device: &Device, switch: &tilo_actions::remote::RemoteActionTarget, on: bool) -> Look {
    let before = settled(device);
    let asked = Instant::now();
    let answer = device.act_wait(switch, &json!({ "on": on }));
    assert!(
        answer.get("err").is_none(),
        "the person switches dark mode {on}: {answer}"
    );
    // The window's next frame: drawn within a moment of the switch, without anything else
    // happening on the desktop.
    while frames(device) <= before {
        assert!(
            asked.elapsed() < SWITCH,
            "{APP}'s window drew no frame in the {SWITCH:?} after the person switched dark mode \
             {on}; its session last said:\n{}",
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    settled(device);
    look(device, if on { "dark" } else { "light" })
}

#[test]
fn a_tl_apps_window_draws_its_next_frame_in_the_theme_the_person_switched_to() {
    let ada = Person::new("Ada");
    let mut device = ada.device("Ada's laptop");
    device.install_gallery();
    device.start();
    device.open(APP);
    device.wait_for_window(APP);
    let switch = tilo_test_env::wait_for("the person's dark mode switch", || {
        device.offered("settings/appearance", "set-dark-mode")
    });
    // The person's desktop starts in the light theme, whatever this device starts in.
    let answer = device.act_wait(&switch, &json!({"on": false}));
    assert!(answer.get("err").is_none(), "the light theme: {answer}");
    let light = tilo_test_env::wait_for("the window to settle in the light theme", || {
        settled(&device);
        let light = look(&device, "light");
        (light.luminance > 128.0).then_some(light)
    });
    assert!(
        light.drawn * 50 > light.pixels,
        "the window shows its page before the switch: {} of {} pixels are drawn on its background",
        light.drawn,
        light.pixels
    );

    let dark = switched(&device, &switch, true);
    assert!(
        dark.luminance + 60.0 < light.luminance,
        "the window is drawn in the dark theme: its mean luminance went from {:.0} to {:.0}",
        light.luminance,
        dark.luminance
    );
    assert!(
        dark.drawn * 50 > dark.pixels,
        "the dark window still shows its page: {} of {} pixels are drawn on its background",
        dark.drawn,
        dark.pixels
    );

    // And back: the light theme reaches the window the same way.
    let again = switched(&device, &switch, false);
    assert!(
        again.luminance > dark.luminance + 60.0,
        "the window is drawn in the light theme again: its mean luminance went from {:.0} to \
         {:.0}",
        dark.luminance,
        again.luminance
    );
}
