//! The mouse wheel, as the control API hands it in: turned at a point, by pixels or by notches,
//! with the modifiers held while it turns (Ctrl and a wheel zoom a canvas).

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde_json::Value as Json;
use tilo_compositor::{Compositor, InputEvent};
use tilo_session_core::surface::Scroll;

use super::{KEY_ALT, KEY_CTRL, KEY_SHIFT, KEY_SUPER, answer, number, refusal};

/// A wheel turned: `dy` up (positive) and `dx` across, in logical pixels, or in wheel notches with
/// `"unit": "notch"` (each a line of the client's own), at `x`, `y` when the request names
/// a point (the pointer goes there first, as a hand moves the mouse before it turns the wheel),
/// with `ctrl`, `shift`, `alt` and `logo` held while it turns: Ctrl and a wheel is how a person
/// zooms a canvas. A modifier the request holds that a `modifiers` request already holds stays
/// held after; one it pressed for the turn alone is let go.
pub(super) fn wheel(
    request: &Json,
    compositor: &mut Compositor,
    held: &RefCell<BTreeSet<u32>>,
) -> Json {
    // A notch up is the wire's positive `dy`; the compositor speaks the protocol's axis, which
    // counts down as positive.
    let x = -number(request, "dx").unwrap_or(0.0);
    let y = -number(request, "dy").unwrap_or(0.0);
    let scroll = match request.get("unit").and_then(Json::as_str) {
        None | Some("pixel") => Scroll::Pixels(x, y),
        Some("notch") => Scroll::Notches(x, y),
        Some(other) => {
            return refusal(
                "parse_error",
                &format!("`scroll`'s `unit` is `pixel` or `notch`, not `{other}`"),
            );
        }
    };
    match (number(request, "x"), number(request, "y")) {
        (Some(x), Some(y)) => {
            if let Err(error) = compositor.input(InputEvent::PointerMotion { x, y }) {
                return refusal("input_failed", &error.to_string());
            }
        }
        (None, None) => {}
        _ => return refusal("parse_error", "`scroll` takes both `x` and `y`, or neither"),
    }
    let pressed: Vec<u32> = [
        (KEY_CTRL, "ctrl"),
        (KEY_SHIFT, "shift"),
        (KEY_ALT, "alt"),
        (KEY_SUPER, "logo"),
    ]
    .into_iter()
    .filter(|(code, name)| {
        request.get(*name).and_then(Json::as_bool) == Some(true) && !held.borrow().contains(code)
    })
    .map(|(code, _)| code)
    .collect();
    let mut result = pressed.iter().try_for_each(|&keycode| {
        compositor.input(InputEvent::Key {
            keycode,
            pressed: true,
        })
    });
    if result.is_ok() {
        result = compositor.input(InputEvent::PointerAxis(scroll));
    }
    // What the turn pressed is let go whatever came of it, so no key stays down after a refusal.
    for &keycode in pressed.iter().rev() {
        let released = compositor.input(InputEvent::Key {
            keycode,
            pressed: false,
        });
        if result.is_ok() {
            result = released;
        }
    }
    answer(result)
}
