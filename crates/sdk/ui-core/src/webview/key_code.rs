//! A key's place on the keyboard, named as the DOM names it
//! (`KeyboardEvent.code`), for the keys whose meaning a layout changes: the
//! letters, digits and punctuation. Named keys (Enter, the arrows) mean the
//! same on every layout and travel by name alone.

use winit::keyboard::{KeyCode, PhysicalKey};

/// Every code this module names, so an engine can check a code it is given
/// against the same list.
const CODES: [(KeyCode, &str); 49] = [
    (KeyCode::KeyA, "KeyA"),
    (KeyCode::KeyB, "KeyB"),
    (KeyCode::KeyC, "KeyC"),
    (KeyCode::KeyD, "KeyD"),
    (KeyCode::KeyE, "KeyE"),
    (KeyCode::KeyF, "KeyF"),
    (KeyCode::KeyG, "KeyG"),
    (KeyCode::KeyH, "KeyH"),
    (KeyCode::KeyI, "KeyI"),
    (KeyCode::KeyJ, "KeyJ"),
    (KeyCode::KeyK, "KeyK"),
    (KeyCode::KeyL, "KeyL"),
    (KeyCode::KeyM, "KeyM"),
    (KeyCode::KeyN, "KeyN"),
    (KeyCode::KeyO, "KeyO"),
    (KeyCode::KeyP, "KeyP"),
    (KeyCode::KeyQ, "KeyQ"),
    (KeyCode::KeyR, "KeyR"),
    (KeyCode::KeyS, "KeyS"),
    (KeyCode::KeyT, "KeyT"),
    (KeyCode::KeyU, "KeyU"),
    (KeyCode::KeyV, "KeyV"),
    (KeyCode::KeyW, "KeyW"),
    (KeyCode::KeyX, "KeyX"),
    (KeyCode::KeyY, "KeyY"),
    (KeyCode::KeyZ, "KeyZ"),
    (KeyCode::Digit0, "Digit0"),
    (KeyCode::Digit1, "Digit1"),
    (KeyCode::Digit2, "Digit2"),
    (KeyCode::Digit3, "Digit3"),
    (KeyCode::Digit4, "Digit4"),
    (KeyCode::Digit5, "Digit5"),
    (KeyCode::Digit6, "Digit6"),
    (KeyCode::Digit7, "Digit7"),
    (KeyCode::Digit8, "Digit8"),
    (KeyCode::Digit9, "Digit9"),
    (KeyCode::Minus, "Minus"),
    (KeyCode::Equal, "Equal"),
    (KeyCode::BracketLeft, "BracketLeft"),
    (KeyCode::BracketRight, "BracketRight"),
    (KeyCode::Backslash, "Backslash"),
    (KeyCode::Semicolon, "Semicolon"),
    (KeyCode::Quote, "Quote"),
    (KeyCode::Comma, "Comma"),
    (KeyCode::Period, "Period"),
    (KeyCode::Slash, "Slash"),
    (KeyCode::Backquote, "Backquote"),
    (KeyCode::IntlBackslash, "IntlBackslash"),
    (KeyCode::Space, "Space"),
];

/// The DOM code of the key pressed, when it is one whose meaning the layout
/// decides.
#[must_use]
pub fn dom_code(key: PhysicalKey) -> Option<&'static str> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    CODES
        .iter()
        .find_map(|&(known, name)| (known == code).then_some(name))
}

/// Whether `code` is one [`dom_code`] gives.
#[must_use]
pub fn known(code: &str) -> bool {
    CODES.iter().any(|&(_, name)| name == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_named_by_its_place_not_its_letter() {
        // German layouts print Z on the key a US layout prints Y on.
        assert_eq!(dom_code(PhysicalKey::Code(KeyCode::KeyY)), Some("KeyY"));
        assert_eq!(
            dom_code(PhysicalKey::Code(KeyCode::IntlBackslash)),
            Some("IntlBackslash")
        );
        assert_eq!(dom_code(PhysicalKey::Code(KeyCode::Enter)), None);
        assert!(CODES.iter().all(|&(_, name)| known(name)));
        assert!(!known("Enter"));
    }
}
