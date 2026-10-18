//! The keyboard layout someone chose in Settings is the one their apps get.
//!
//! "My keyboard layout is wrong": the compositor builds the seat's keymap
//! from the saved `input` section when it starts and again whenever the
//! section changes, and every client reads its keymap from the seat. A
//! client here reads the keymap it is sent and looks at what the key in the
//! Y position types: `z` on a German keyboard, `y` on a US one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::io::Read as _;

use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::xdg_toplevel;

/// The last keymap the seat sent, as text.
#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    keymap: Option<String>,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap {
            format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
            fd,
            size,
        } = event
        {
            let mut bytes = Vec::new();
            std::fs::File::from(fd)
                .take(u64::from(size))
                .read_to_end(&mut bytes)
                .unwrap();
            let text = String::from_utf8_lossy(&bytes);
            state.keymap = Some(text.trim_end_matches('\0').to_owned());
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

type Harness = crate::support::wayland::Harness<Client>;

/// What the key in the Y position (`<AD06>`) types first in `keymap`.
fn ad06(keymap: &str) -> Option<String> {
    // The key's block, `key <AD06> { … };`, may span lines and name its
    // groups; its first symbol list is the first group's.
    let key = keymap.find("key <AD06>")?;
    let block = keymap.get(key..)?.split("};").next()?;
    let symbols = block
        .split('[')
        .skip(1)
        .filter_map(|part| part.split(']').next())
        // `symbols[Group1]` or `symbols[1]` names a group, not symbols.
        .find(|inner| {
            let inner = inner.trim();
            !inner.starts_with("Group") && !inner.chars().all(|c| c.is_ascii_digit())
        })?;
    let first = symbols.split(',').next()?.trim();
    // A keymap names a Latin letter's keysym by its name or its code
    // (`z` or `0x7a`).
    let letter = first
        .strip_prefix("0x")
        .and_then(|code| u32::from_str_radix(code, 16).ok())
        .and_then(char::from_u32)
        .map_or_else(|| first.to_owned(), |letter| letter.to_string());
    Some(letter)
}

/// Save `layouts` (the first in use) the way Settings saves them.
fn save_layouts(layouts: &[&str]) {
    let mut input = serde_json::Map::new();
    input.insert("keyboard_layout".into(), layouts[0].into());
    input.insert(
        "keyboard_layouts".into(),
        serde_json::Value::Array(layouts.iter().map(|layout| (*layout).into()).collect()),
    );
    settings_store::update_section("input", input).unwrap();
}

fn keyboard_client() -> Harness {
    let mut h = Harness::new();
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    h.pump();
    h
}

#[test]
fn a_saved_german_layout_is_the_keymap_apps_get_from_the_start() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    save_layouts(&["de"]);
    let h = keyboard_client();
    let keymap = h.client.keymap.as_deref().expect("the seat sent a keymap");
    assert_eq!(
        ad06(keymap).as_deref(),
        Some("z"),
        "the key in the Y position types z with a saved German layout"
    );
}

#[test]
fn changing_the_saved_layout_sends_apps_the_new_keymap() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    save_layouts(&["us"]);
    let mut h = keyboard_client();
    assert_eq!(
        h.client.keymap.as_deref().and_then(ad06).as_deref(),
        Some("y")
    );
    save_layouts(&["de", "us"]);
    h.state.reload_keyboard_settings();
    h.pump();
    assert_eq!(
        h.client.keymap.as_deref().and_then(ad06).as_deref(),
        Some("z"),
        "a German layout chosen in Settings reaches the app at once"
    );
}
