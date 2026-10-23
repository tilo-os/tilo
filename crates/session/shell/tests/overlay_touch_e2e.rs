//! A finger on a shell overlay works the overlay, exactly as the pointer
//! does, and never the window behind it.
//!
//! Quick Settings, the capture tool's selection, mode controls and shutter,
//! and the screenshot editor answer a tap, a drag and a hold the way they
//! answer the pointer; a touch outside a dismissable overlay closes it as a
//! click there does; a touch sequence stays with the surface it started on
//! until it ends.
//!
//! Every test drives the real seat through test control's touchscreen,
//! which uses the DRM backend's contact ownership and the compositor's one
//! touch routing path, with a real Wayland client that takes `wl_touch`
//! mapped under the overlay. Before, the touch-down went to that client
//! while the overlay was up, so the overlay saw either nothing or the same
//! finger the client did.

use super::*;

const PANEL: &str = "e2e-panel";

/// A compositor with a home of its own (screenshots land in it), and a
/// touch-capable client window mapped and at rest.
fn desk_with_a_touch_client(app_id: &str) -> (tempfile::TempDir, Compositor, TestClient) {
    let home = tempfile::Builder::new()
        .prefix("tilo-overlay-touch")
        .tempdir()
        .expect("create the fixture home");
    give_the_desk_an_audio_device(home.path());
    let compositor = Compositor::start_with_home(Some(home.path()));
    let mut client = TestClient::new(compositor.connect(), app_id);
    client.assert_is_tilo_compositor();
    client.map_window(&compositor);
    client.wait_until(
        |state| state.touch_bound,
        "the client did not finish binding wl_touch before injection",
    );
    client.roundtrip();
    // Settles the strip and proves the window takes input where it is drawn.
    let _ = client.pointer_target(&compositor);
    client.clear_touch_and_pointer_trace();
    (home, compositor, client)
}

/// A stand-in `wpctl` on the fixture home's PATH, so Quick Settings has a
/// volume slider to work: it reports the level last set, 0.40 at first.
fn give_the_desk_an_audio_device(home: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).expect("create the fixture tools");
    let level = home.join("volume");
    std::fs::write(&level, "0.40").expect("seed the volume");
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         get-volume) printf 'Volume: %s\\n' \"$(cat '{level}')\" ;;\n\
         set-volume) printf '%s' \"$3\" | tr -d '%' > '{level}' ;;\n\
         esac\n\
         exit 0\n",
        level = level.display()
    );
    let wpctl = bin.join("wpctl");
    std::fs::write(&wpctl, script).expect("write the stand-in wpctl");
    std::fs::set_permissions(&wpctl, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in wpctl runnable");
}

/// Whether `(x, y)` is on one of the client's laid-out surfaces: a touch
/// there would be the client's if no overlay were up.
fn over_the_client(compositor: &Compositor, (x, y): (f64, f64)) -> bool {
    compositor
        .foreign_surface_rects()
        .iter()
        .any(|[sx, sy, sw, sh]| x > *sx && x < sx + sw && y > *sy && y < sy + sh)
}

fn chrome_state(compositor: &Compositor) -> serde_json::Value {
    compositor.control(&serde_json::json!({ "op": "state_dump" }))["state"]["chrome"].clone()
}

/// Wait until the chrome's state dump satisfies `done`.
fn wait_for_chrome(compositor: &Compositor, done: impl Fn(&serde_json::Value) -> bool, what: &str) {
    let started = Instant::now();
    while !done(&chrome_state(compositor)) {
        assert!(
            started.elapsed() < deadline(),
            "{what}.\n{}",
            compositor.debug_state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until a laid-out target `pick` admits exists (or, with `present`
/// false, until none does), and return the first one's rect.
fn wait_for_target(
    compositor: &Compositor,
    pick: impl Fn(&str) -> bool,
    present: bool,
    what: &str,
) -> Option<[f64; 4]> {
    let started = Instant::now();
    loop {
        let found = compositor
            .target_rects(&pick)
            .into_iter()
            .map(|(_, rect)| rect)
            .find(|[_, _, w, h]| *w > 0.0 && *h > 0.0);
        if found.is_some() == present {
            return found;
        }
        assert!(
            started.elapsed() < deadline(),
            "{what}.\n{}",
            compositor.debug_state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn center([x, y, w, h]: [f64; 4]) -> (f64, f64) {
    (x + w / 2.0, y + h / 2.0)
}

/// One finger down at `at`, every edge closed by its frame.
fn finger_down(compositor: &Compositor, (x, y): (f64, f64)) {
    compositor.touch(PANEL, 0, "down", x, y);
    compositor.touch(PANEL, 0, "frame", x, y);
}

/// The finger moved to `to` in `steps` even moves.
fn finger_move(compositor: &Compositor, from: (f64, f64), to: (f64, f64), steps: u32) {
    for step in 1..=steps {
        let t = f64::from(step) / f64::from(steps);
        let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        compositor.touch(PANEL, 0, "motion", x, y);
        compositor.touch(PANEL, 0, "frame", x, y);
    }
}

fn finger_up(compositor: &Compositor, (x, y): (f64, f64)) {
    compositor.touch(PANEL, 0, "up", x, y);
    compositor.touch(PANEL, 0, "frame", x, y);
}

fn tap(compositor: &Compositor, at: (f64, f64)) {
    finger_down(compositor, at);
    finger_up(compositor, at);
}

fn drag(compositor: &Compositor, from: (f64, f64), to: (f64, f64)) {
    finger_down(compositor, from);
    finger_move(compositor, from, to, 8);
    finger_up(compositor, to);
}

/// The window behind received no touch and no click: nothing of the
/// fingers that worked the overlay reached it.
fn assert_the_window_behind_got_nothing(client: &mut TestClient, what: &str) {
    // Test control injects each edge before it answers, so whatever the
    // compositor sent is queued ahead of these syncs' replies.
    client.roundtrip();
    client.roundtrip();
    assert!(
        client.state.touch_events.is_empty(),
        "{what}: the window behind the overlay received the finger: {:?}",
        client.state.touch_events
    );
    let buttons = client
        .state
        .pointer_events
        .iter()
        .filter(|event| **event == "button")
        .count();
    assert_eq!(
        buttons, 0,
        "{what}: the window behind the overlay was clicked: {:?}",
        client.state.pointer_events
    );
}

fn open_quick_settings(compositor: &Compositor) {
    compositor.modifiers(false, false, false, true);
    compositor.key("q");
    compositor.modifiers(false, false, false, false);
    wait_for_overlay(
        compositor,
        "quick_settings",
        "Super+Q did not open Quick Settings",
    );
    compositor.wait_for_layout_to_settle(
        |name| name.starts_with("quick_settings_"),
        "Quick Settings opening",
    );
}

/// Print opens the capture tool; a tap on Selection picks drawing one.
fn open_capture_tool(compositor: &Compositor, client: &mut TestClient) {
    for (op, key) in [("key_down", "PrintScreen"), ("key_up", "PrintScreen")] {
        let response = compositor.control(&serde_json::json!({ "op": op, "key": key }));
        assert!(response.get("ok").is_some(), "{op} Print: {response:?}");
    }
    wait_for_target(
        compositor,
        |name| name == "capture.canvas",
        true,
        "Print did not open the capture tool",
    );
    compositor.wait_for_layout_to_settle(
        |name| name.starts_with("capture."),
        "the capture tool opening",
    );
    let selection = compositor.named_center("capture.target.selection");
    tap(compositor, selection);
    assert_the_window_behind_got_nothing(client, "tapping the capture tool's Selection");
}

/// A finger dragged across the capture canvas, over the window, draws a
/// selection there. Returns the selection's rect.
fn touch_select_an_area(compositor: &Compositor, client: &mut TestClient) -> [f64; 4] {
    let [x, y, w, h] = compositor
        .named_rect("capture.canvas")
        .expect("the capture canvas is laid out");
    let from = (x + w * 0.3, y + h * 0.3);
    let to = (x + w * 0.6, y + h * 0.55);
    assert!(
        over_the_client(compositor, from) && over_the_client(compositor, to),
        "the drag must run over the window to prove the overlay takes it: {from:?} to {to:?}, \
         window {:?}",
        compositor.foreign_surface_rects()
    );
    drag(compositor, from, to);
    let selection = wait_for_target(
        compositor,
        |name| name == "capture.selection.0.move",
        true,
        "a finger dragged across the capture canvas drew no selection",
    )
    .expect("a selection");
    let [sx, sy, sw, sh] = selection;
    assert!(
        (sx - from.0).abs() < 2.0
            && (sy - from.1).abs() < 2.0
            && (sx + sw - to.0).abs() < 2.0
            && (sy + sh - to.1).abs() < 2.0,
        "the selection {selection:?} spans the drag from {from:?} to {to:?}"
    );
    assert_the_window_behind_got_nothing(client, "drawing a capture selection");
    selection
}

/// Tap the shutter and wait for the screenshot's receipt.
fn tap_the_shutter(compositor: &Compositor, client: &mut TestClient) {
    let shutter = compositor.named_center("capture.shutter");
    tap(compositor, shutter);
    wait_for_target(
        compositor,
        |name| name == "capture.canvas",
        false,
        "a tap on the shutter did not take the screenshot",
    );
    wait_for_target(
        compositor,
        |name| name == "capture.edit-screenshot",
        true,
        "the screenshot's receipt never came",
    );
    assert_the_window_behind_got_nothing(client, "tapping the shutter");
}

/// A finger on a Quick Settings slider moves it, as the pointer held down
/// does, and the window under the panel receives nothing.
#[test]
fn a_finger_on_a_quick_settings_slider_moves_it_and_never_the_window_behind() {
    let (_home, compositor, mut client) = desk_with_a_touch_client("org.tilo.test.qs-touch");
    open_quick_settings(&compositor);
    client.clear_touch_and_pointer_trace();

    let [x, y, w, h] = wait_for_target(
        &compositor,
        |name| name == "quick_settings_volume",
        true,
        "Quick Settings never showed its volume slider",
    )
    .expect("the volume slider");
    let from = (x + w * 0.2, y + h / 2.0);
    let to = (x + w - 1.0, y + h / 2.0);
    assert!(
        over_the_client(&compositor, from),
        "the slider must lie over the window to prove the panel takes the finger: {from:?}, \
         window {:?}",
        compositor.foreign_surface_rects()
    );
    finger_down(&compositor, from);
    finger_move(&compositor, from, to, 6);
    let volume = chrome_state(&compositor)["quick_settings_volume"]
        .as_f64()
        .expect("the state dump reports the volume level");
    assert!(volume > 0.9, "the slider follows the finger: {volume}");
    finger_up(&compositor, to);

    assert_eq!(
        chrome_state(&compositor)["overlays"]["quick_settings"].as_bool(),
        Some(true),
        "working a slider keeps Quick Settings open"
    );
    assert_the_window_behind_got_nothing(&mut client, "dragging a Quick Settings slider");
}

#[test]
fn a_tap_outside_quick_settings_closes_it_like_a_click() {
    let (_home, compositor, mut client) = desk_with_a_touch_client("org.tilo.test.qs-dismiss");
    open_quick_settings(&compositor);
    client.clear_touch_and_pointer_trace();

    // The panel's extent: every control it lays out, with a margin.
    let controls = compositor.target_rects(|name| name.starts_with("quick_settings_"));
    assert!(!controls.is_empty(), "Quick Settings lays out its controls");
    let (px0, py0, px1, py1) = controls.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(x0, y0, x1, y1), (_, [x, y, w, h])| {
            (x0.min(*x), y0.min(*y), x1.max(x + w), y1.max(y + h))
        },
    );
    let on_panel = |(x, y): (f64, f64)| {
        x >= px0 - 48.0 && x <= px1 + 48.0 && y >= py0 - 48.0 && y <= py1 + 48.0
    };
    let outside = compositor
        .foreign_surface_rects()
        .iter()
        .flat_map(|&[sx, sy, sw, sh]| {
            [0.1, 0.3, 0.5, 0.7, 0.9]
                .into_iter()
                .flat_map(move |fx| [0.3, 0.6, 0.9].map(|fy| (sx + sw * fx, sy + sh * fy)))
        })
        .find(|&point| !on_panel(point))
        .expect("some of the window lies outside the panel");

    tap(&compositor, outside);
    wait_for_chrome(
        &compositor,
        |chrome| chrome["overlays"]["quick_settings"].as_bool() == Some(false),
        "a tap outside Quick Settings did not close it",
    );
    assert_the_window_behind_got_nothing(&mut client, "tapping outside Quick Settings");
}

/// A finger dragged across the capture tool draws the selection, over the
/// window it covers, and the window receives nothing.
#[test]
fn a_finger_drags_out_a_capture_selection() {
    let (_home, compositor, mut client) = desk_with_a_touch_client("org.tilo.test.capture-drag");
    open_capture_tool(&compositor, &mut client);
    touch_select_an_area(&compositor, &mut client);
}

#[test]
fn tapping_the_capture_shutter_takes_the_screenshot() {
    let (_home, compositor, mut client) = desk_with_a_touch_client("org.tilo.test.capture-shutter");
    open_capture_tool(&compositor, &mut client);
    touch_select_an_area(&compositor, &mut client);
    tap_the_shutter(&compositor, &mut client);
}

/// The screenshot editor takes the finger: a drag on the picture marks it,
/// a tap on Cancel closes it, and the window under it receives nothing.
#[test]
fn a_finger_on_the_screenshot_editor_marks_it_and_never_the_window_behind() {
    let (_home, compositor, mut client) = desk_with_a_touch_client("org.tilo.test.markup-touch");
    open_capture_tool(&compositor, &mut client);
    touch_select_an_area(&compositor, &mut client);
    tap_the_shutter(&compositor, &mut client);

    // The receipt flies from the selection to where it rests.
    compositor.wait_for_layout_to_settle(
        |name| name.starts_with("capture."),
        "the screenshot's receipt coming to rest",
    );
    let edit = compositor.named_center("capture.edit-screenshot");
    tap(&compositor, edit);
    assert_the_window_behind_got_nothing(&mut client, "tapping the receipt's Edit");
    wait_for_chrome(
        &compositor,
        |chrome| chrome["screenshot_markup_strokes"].as_u64() == Some(0),
        "a tap on Edit did not open the screenshot editor",
    );
    compositor.wait_for_layout_to_settle(
        |name| name == "screenshot-markup" || name.starts_with("markup."),
        "the screenshot editor opening",
    );
    client.clear_touch_and_pointer_trace();

    let editor = compositor
        .named_rect("screenshot-markup")
        .expect("the screenshot editor is laid out");
    let (cx, cy) = center(editor);
    let from = (cx - 40.0, cy - 20.0);
    let to = (cx + 40.0, cy + 20.0);
    assert!(
        over_the_client(&compositor, from),
        "the stroke must run over the window to prove the editor takes it: {from:?}, \
         window {:?}",
        compositor.foreign_surface_rects()
    );
    drag(&compositor, from, to);
    wait_for_chrome(
        &compositor,
        |chrome| chrome["screenshot_markup_strokes"].as_u64() == Some(1),
        "a finger dragged across the screenshot drew no mark",
    );
    assert_the_window_behind_got_nothing(&mut client, "marking up the screenshot");

    let cancel = compositor.named_center("markup.cancel");
    tap(&compositor, cancel);
    wait_for_chrome(
        &compositor,
        |chrome| chrome["screenshot_markup_strokes"].is_null(),
        "a tap on Cancel did not close the screenshot editor",
    );
    assert_the_window_behind_got_nothing(&mut client, "tapping the editor's Cancel");
}
