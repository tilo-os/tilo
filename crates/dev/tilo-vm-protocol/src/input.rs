//! Input for headful slots: what `tilo-vm input` asks for, and how key
//! names, text and gamepad buttons map to Linux evdev codes (US layout).
//!
//! The codes are the kernel's `input-event-codes.h`. They travel through
//! per-seat uinput devices (keyboard, pointer, touchscreen, pen, gamepad),
//! so the compositor sees them through the real kernel path (libinput, or
//! evdev for game controllers), exactly like hardware plugged into that seat.

use serde::{Deserialize, Serialize};

/// One step of `tilo-vm input`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputAction {
    /// Press and release each combo in order, e.g. `ctrl+alt+t`, `Return`.
    Key { combos: Vec<String> },
    /// Type text as key strokes on a US layout.
    Type { text: String },
    /// Move the pointer to output pixel coordinates.
    Move { x: u32, y: u32 },
    /// Click a button, optionally after moving to `at`.
    Click {
        button: Button,
        #[serde(default)]
        at: Option<(u32, u32)>,
    },
    /// One finger down and up at touchscreen pixel `x`, `y`.
    TouchTap { x: u32, y: u32 },
    /// One finger down, held for `duration_ms`, then up (long-press).
    TouchHold { x: u32, y: u32, duration_ms: u32 },
    /// One finger down at `from`, moved to `to` over `duration_ms`, up.
    TouchDrag {
        from: (u32, u32),
        to: (u32, u32),
        duration_ms: u32,
    },
    /// Two fingers on a horizontal line through `center`, their distance
    /// going from `from_distance` to `to_distance` pixels (growing zooms in).
    TouchPinch {
        center: (u32, u32),
        from_distance: u32,
        to_distance: u32,
        duration_ms: u32,
    },
    /// Pen touches down at `x`, `y` with `pressure` percent and lifts.
    PenTap { x: u32, y: u32, pressure: u8 },
    /// Pen touches down at `from`, moves to `to` over `duration_ms`, lifts.
    PenDrag {
        from: (u32, u32),
        to: (u32, u32),
        duration_ms: u32,
        pressure: u8,
    },
    /// Press a key combo and keep it down until a `KeyUp` (a held modifier).
    KeyDown { combo: String },
    /// Release what `KeyDown` pressed, in reverse order.
    KeyUp { combo: String },
    /// Plug in the device a checked-in profile describes (bus, ids, name and
    /// capabilities of real hardware). A plugged keyboard profile takes over
    /// `Key`, `Type`, `KeyDown` and `KeyUp` from the default keyboard.
    DevicePlug { profile: String },
    /// Remove a plugged profile device, as unplugging it would.
    DeviceUnplug { profile: String },
    /// `fingers` tap the plugged touchpad's centre and lift.
    TouchpadTap { fingers: u8 },
    /// `fingers` move across the touchpad by `dx_mm`, `dy_mm` millimetres over
    /// `duration_ms`: one finger moves the pointer, two scroll, three swipe.
    TouchpadSwipe {
        fingers: u8,
        dx_mm: i16,
        dy_mm: i16,
        duration_ms: u32,
    },
    /// Press and release the clickpad's physical button.
    TouchpadClick,
    /// Press and release each gamepad combo in order, e.g. `a`, `guide+lb`,
    /// `up`.
    GamepadPress { combos: Vec<String> },
    /// Deflect a stick to `x`, `y` percent (-100..=100, +y is down), hold
    /// it for `duration_ms`, and let it spring back to the centre.
    GamepadStick {
        stick: Stick,
        x: i8,
        y: i8,
        duration_ms: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stick {
    Left,
    Right,
}

impl Stick {
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "left" | "l" => Some(Self::Left),
            "right" | "r" => Some(Self::Right),
            _ => None,
        }
    }

    /// The (x, y) absolute axes of this stick.
    pub fn axes(self) -> (u16, u16) {
        match self {
            Self::Left => (ABS_X, ABS_Y),
            Self::Right => (ABS_RX, ABS_RY),
        }
    }
}

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

/// One control of a gamepad combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadInput {
    /// A button (`BTN_SOUTH` ...).
    Button(u16),
    /// An analogue trigger axis, pulled fully.
    Trigger(u16),
    /// A d-pad direction: hat axis and -1 or 1.
    Hat(u16, i32),
}

/// Gamepad controls by their Xbox-style printed legend (what the desktop's
/// controller bindings name), with the evdev positional names as aliases.
pub const GAMEPAD: &[(&str, GamepadInput)] = &[
    ("a", GamepadInput::Button(0x130)),
    ("south", GamepadInput::Button(0x130)),
    ("b", GamepadInput::Button(0x131)),
    ("east", GamepadInput::Button(0x131)),
    ("x", GamepadInput::Button(0x134)),
    ("west", GamepadInput::Button(0x134)),
    ("y", GamepadInput::Button(0x133)),
    ("north", GamepadInput::Button(0x133)),
    ("lb", GamepadInput::Button(0x136)),
    ("rb", GamepadInput::Button(0x137)),
    ("lt", GamepadInput::Trigger(ABS_Z)),
    ("rt", GamepadInput::Trigger(ABS_RZ)),
    ("select", GamepadInput::Button(0x13a)),
    ("back", GamepadInput::Button(0x13a)),
    ("start", GamepadInput::Button(0x13b)),
    ("guide", GamepadInput::Button(0x13c)),
    ("home", GamepadInput::Button(0x13c)),
    ("l3", GamepadInput::Button(0x13d)),
    ("r3", GamepadInput::Button(0x13e)),
    ("up", GamepadInput::Hat(ABS_HAT0Y, -1)),
    ("down", GamepadInput::Hat(ABS_HAT0Y, 1)),
    ("left", GamepadInput::Hat(ABS_HAT0X, -1)),
    ("right", GamepadInput::Hat(ABS_HAT0X, 1)),
];

/// The gamepad buttons the virtual controller advertises.
pub fn gamepad_buttons() -> impl Iterator<Item = u16> {
    GAMEPAD.iter().filter_map(|(_, input)| match input {
        GamepadInput::Button(code) => Some(*code),
        _ => None,
    })
}

/// The controls of a gamepad combo such as `a`, `guide+lb` or `up`:
/// pressed in order, released in reverse.
///
/// # Errors
/// A part that names no control.
pub fn gamepad_combo(combo: &str) -> Result<Vec<GamepadInput>, String> {
    if combo.is_empty() {
        return Err(combo.to_owned());
    }
    combo
        .split('+')
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            GAMEPAD
                .iter()
                .find(|(name, _)| *name == lower)
                .map(|(_, input)| *input)
                .ok_or_else(|| part.to_owned())
        })
        .collect()
}

/// The names `gamepad_combo` accepts, for error messages.
pub fn gamepad_names() -> String {
    GAMEPAD
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The longest gesture or hold one action may ask for.
pub const MAX_DURATION_MS: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn code(self) -> u16 {
        match self {
            Self::Left => BTN_LEFT,
            Self::Right => BTN_RIGHT,
            Self::Middle => BTN_MIDDLE,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "left" | "1" => Some(Self::Left),
            "right" | "3" => Some(Self::Right),
            "middle" | "2" => Some(Self::Middle),
            _ => None,
        }
    }
}

pub const KEY_LEFTSHIFT: u16 = 42;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
/// The highest key code the virtual keyboard advertises.
pub const KEY_MAX_USED: u16 = 248;

/// Named keys, lowercase. Letters, digits and punctuation go through
/// [`char_stroke`] instead.
const NAMED: &[(&str, u16)] = &[
    ("esc", 1),
    ("escape", 1),
    ("minus", 12),
    ("equal", 13),
    ("backspace", 14),
    ("tab", 15),
    ("leftbrace", 26),
    ("bracketleft", 26),
    ("rightbrace", 27),
    ("bracketright", 27),
    ("enter", 28),
    ("return", 28),
    ("ctrl", 29),
    ("control", 29),
    ("leftctrl", 29),
    ("semicolon", 39),
    ("apostrophe", 40),
    ("grave", 41),
    ("shift", 42),
    ("leftshift", 42),
    ("backslash", 43),
    ("comma", 51),
    ("dot", 52),
    ("period", 52),
    ("slash", 53),
    ("rightshift", 54),
    ("alt", 56),
    ("leftalt", 56),
    ("space", 57),
    ("capslock", 58),
    ("f1", 59),
    ("f2", 60),
    ("f3", 61),
    ("f4", 62),
    ("f5", 63),
    ("f6", 64),
    ("f7", 65),
    ("f8", 66),
    ("f9", 67),
    ("f10", 68),
    ("f11", 87),
    ("f12", 88),
    ("rightctrl", 97),
    ("altgr", 100),
    ("rightalt", 100),
    ("home", 102),
    ("up", 103),
    ("pageup", 104),
    ("pgup", 104),
    ("left", 105),
    ("right", 106),
    ("end", 107),
    ("down", 108),
    ("pagedown", 109),
    ("pgdn", 109),
    ("insert", 110),
    ("delete", 111),
    ("del", 111),
    ("super", 125),
    ("meta", 125),
    ("logo", 125),
    ("win", 125),
    ("leftmeta", 125),
    ("rightmeta", 126),
    ("menu", 127),
    ("compose", 127),
];

const LETTERS: [u16; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45,
    21, 44,
];

/// Punctuation on a US layout: (unshifted, shifted, code).
const PUNCTUATION: &[(char, char, u16)] = &[
    ('1', '!', 2),
    ('2', '@', 3),
    ('3', '#', 4),
    ('4', '$', 5),
    ('5', '%', 6),
    ('6', '^', 7),
    ('7', '&', 8),
    ('8', '*', 9),
    ('9', '(', 10),
    ('0', ')', 11),
    ('-', '_', 12),
    ('=', '+', 13),
    ('[', '{', 26),
    (']', '}', 27),
    (';', ':', 39),
    ('\'', '"', 40),
    ('`', '~', 41),
    ('\\', '|', 43),
    (',', '<', 51),
    ('.', '>', 52),
    ('/', '?', 53),
];

/// One key stroke: the key and whether shift is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    pub code: u16,
    pub shift: bool,
}

/// The stroke that types `c` on a US layout, or `None` for characters a
/// US keyboard cannot type directly.
pub fn char_stroke(c: char) -> Option<Stroke> {
    let plain = |code| Some(Stroke { code, shift: false });
    match c {
        'a'..='z' => plain(letter(c)?),
        'A'..='Z' => Some(Stroke {
            code: letter(c.to_ascii_lowercase())?,
            shift: true,
        }),
        ' ' => plain(57),
        '\n' => plain(28),
        '\t' => plain(15),
        _ => PUNCTUATION.iter().find_map(|(lower, upper, code)| {
            if c == *lower {
                plain(*code)
            } else if c == *upper {
                Some(Stroke {
                    code: *code,
                    shift: true,
                })
            } else {
                None
            }
        }),
    }
}

fn letter(c: char) -> Option<u16> {
    let index = u32::from(c).checked_sub(u32::from('a'))?;
    LETTERS.get(usize::try_from(index).ok()?).copied()
}

/// The strokes that type `text`.
///
/// # Errors
/// The first character a US layout cannot type.
pub fn text_strokes(text: &str) -> Result<Vec<Stroke>, char> {
    text.chars().map(|c| char_stroke(c).ok_or(c)).collect()
}

/// Key codes of a combo such as `ctrl+alt+t`, `super`, `Return` or
/// `shift+/`: pressed in order, released in reverse.
///
/// # Errors
/// A part that names no key.
pub fn combo_codes(combo: &str) -> Result<Vec<u16>, String> {
    if combo.is_empty() {
        return Err(combo.to_owned());
    }
    // A trailing `+` is the plus key itself (`ctrl++`).
    let (head, plus) = match combo.strip_suffix("++") {
        Some(head) => (head, true),
        None => (combo, false),
    };
    let mut codes = Vec::new();
    let parts: Vec<&str> = if head.is_empty() {
        Vec::new()
    } else {
        head.split('+').collect()
    };
    for part in parts {
        codes.extend(key_codes(part).ok_or_else(|| part.to_owned())?);
    }
    if plus {
        codes.extend([KEY_LEFTSHIFT, 13]);
    }
    if codes.is_empty() {
        return Err(combo.to_owned());
    }
    Ok(codes)
}

/// One named key or character; a shifted character yields shift first.
fn key_codes(name: &str) -> Option<Vec<u16>> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, code)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Some(vec![*code]);
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    // In a combo a letter names its key: `ctrl+T` is ctrl+t.
    let stroke = char_stroke(if c.is_ascii_alphabetic() {
        c.to_ascii_lowercase()
    } else {
        c
    })?;
    Some(if stroke.shift {
        vec![KEY_LEFTSHIFT, stroke.code]
    } else {
        vec![stroke.code]
    })
}

/// Validate actions before they travel: every combo and character must map.
///
/// # Errors
/// A sentence naming the first action that cannot be sent.
pub fn validate(actions: &[InputAction]) -> Result<(), String> {
    for action in actions {
        match action {
            InputAction::Key { combos } => {
                if combos.is_empty() {
                    return Err("no key given".into());
                }
                for combo in combos {
                    combo_codes(combo)
                        .map_err(|part| format!("'{part}' in '{combo}' names no key"))?;
                }
            }
            InputAction::Type { text } => {
                text_strokes(text)
                    .map_err(|c| format!("'{c}' cannot be typed on a US keyboard layout"))?;
            }
            InputAction::KeyDown { combo } | InputAction::KeyUp { combo } => {
                combo_codes(combo).map_err(|part| format!("'{part}' in '{combo}' names no key"))?;
            }
            InputAction::DevicePlug { profile } | InputAction::DeviceUnplug { profile } => {
                crate::profile::profile(profile)?;
            }
            InputAction::TouchpadTap { fingers } => touchpad_fingers(*fingers)?,
            InputAction::TouchpadSwipe {
                fingers,
                dx_mm,
                dy_mm,
                duration_ms,
            } => {
                touchpad_fingers(*fingers)?;
                duration(*duration_ms)?;
                if (*dx_mm == 0 && *dy_mm == 0) || dx_mm.abs() > 200 || dy_mm.abs() > 200 {
                    return Err(format!(
                        "a touchpad swipe of {dx_mm},{dy_mm} mm is no movement on a pad under 200 mm"
                    ));
                }
            }
            InputAction::TouchpadClick
            | InputAction::Move { .. }
            | InputAction::Click { .. }
            | InputAction::TouchTap { .. } => {}
            InputAction::TouchHold { duration_ms, .. }
            | InputAction::TouchDrag { duration_ms, .. } => duration(*duration_ms)?,
            InputAction::TouchPinch {
                from_distance,
                to_distance,
                duration_ms,
                ..
            } => {
                duration(*duration_ms)?;
                if *from_distance == 0 || *to_distance == 0 {
                    return Err("a pinch needs the two fingers apart: distances above 0".into());
                }
            }
            InputAction::PenTap { pressure, .. } => self::pressure(*pressure)?,
            InputAction::PenDrag {
                pressure,
                duration_ms,
                ..
            } => {
                self::pressure(*pressure)?;
                duration(*duration_ms)?;
            }
            InputAction::GamepadPress { combos } => {
                if combos.is_empty() {
                    return Err("no gamepad button given".into());
                }
                for combo in combos {
                    gamepad_combo(combo).map_err(|part| {
                        format!(
                            "'{part}' in '{combo}' names no gamepad control ({})",
                            gamepad_names()
                        )
                    })?;
                }
            }
            InputAction::GamepadStick {
                x, y, duration_ms, ..
            } => {
                duration(*duration_ms)?;
                if !(-100..=100).contains(x) || !(-100..=100).contains(y) {
                    return Err(format!(
                        "stick position {x},{y} is outside -100..100 percent"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Fingers a touchpad gesture uses: the profile's pad tracks five, libinput
/// decodes one to three as pointer, scroll and swipe.
fn touchpad_fingers(fingers: u8) -> Result<(), String> {
    if (1..=3).contains(&fingers) {
        Ok(())
    } else {
        Err(format!("{fingers} fingers: a touchpad gesture uses 1 to 3"))
    }
}

fn duration(ms: u32) -> Result<(), String> {
    if ms > MAX_DURATION_MS {
        Err(format!(
            "{ms} ms is longer than the {MAX_DURATION_MS} ms one action may take"
        ))
    } else {
        Ok(())
    }
}

fn pressure(percent: u8) -> Result<(), String> {
    if (1..=100).contains(&percent) {
        Ok(())
    } else {
        Err(format!("pen pressure {percent} is outside 1..100 percent"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamepad_combos_use_printed_legends() {
        assert_eq!(gamepad_combo("a").unwrap(), [GamepadInput::Button(0x130)]);
        assert_eq!(
            gamepad_combo("Guide+LB").unwrap(),
            [GamepadInput::Button(0x13c), GamepadInput::Button(0x136)]
        );
        assert_eq!(
            gamepad_combo("up").unwrap(),
            [GamepadInput::Hat(ABS_HAT0Y, -1)]
        );
        assert_eq!(
            gamepad_combo("rt").unwrap(),
            [GamepadInput::Trigger(ABS_RZ)]
        );
        assert_eq!(gamepad_combo("a+nope"), Err("nope".into()));
        let mut buttons: Vec<u16> = gamepad_buttons().collect();
        buttons.sort_unstable();
        buttons.dedup();
        assert_eq!(buttons.len(), 11);
    }

    #[test]
    fn gesture_validation_names_the_problem() {
        let long = validate(&[InputAction::TouchHold {
            x: 1,
            y: 1,
            duration_ms: 60_000,
        }])
        .unwrap_err();
        assert!(long.contains("10000 ms"), "{long}");
        let pinch = validate(&[InputAction::TouchPinch {
            center: (5, 5),
            from_distance: 0,
            to_distance: 100,
            duration_ms: 300,
        }])
        .unwrap_err();
        assert!(pinch.contains("apart"), "{pinch}");
        let stick = validate(&[InputAction::GamepadStick {
            stick: Stick::Left,
            x: 101,
            y: 0,
            duration_ms: 100,
        }])
        .unwrap_err();
        assert!(stick.contains("-100..100"), "{stick}");
        let button = validate(&[InputAction::GamepadPress {
            combos: vec!["guide+zz".into()],
        }])
        .unwrap_err();
        assert!(
            button.contains("'zz'") && button.contains("start"),
            "{button}"
        );
        assert!(
            validate(&[InputAction::PenTap {
                x: 0,
                y: 0,
                pressure: 0
            }])
            .is_err()
        );
    }

    #[test]
    fn device_and_touchpad_actions_validate_and_round_trip() {
        let plug = InputAction::DevicePlug {
            profile: "lenovo-83ed-touchpad".into(),
        };
        assert!(validate(std::slice::from_ref(&plug)).is_ok());
        assert_eq!(
            serde_json::to_string(&plug).unwrap(),
            r#"{"kind":"device_plug","profile":"lenovo-83ed-touchpad"}"#
        );
        let unknown = validate(&[InputAction::DevicePlug {
            profile: "nope".into(),
        }])
        .unwrap_err();
        assert!(unknown.contains("lenovo-83ed-keyboard"), "{unknown}");
        let swipe = InputAction::TouchpadSwipe {
            fingers: 2,
            dx_mm: 0,
            dy_mm: -30,
            duration_ms: 300,
        };
        assert!(validate(std::slice::from_ref(&swipe)).is_ok());
        let text = serde_json::to_string(&swipe).unwrap();
        assert_eq!(serde_json::from_str::<InputAction>(&text).unwrap(), swipe);
        assert!(validate(&[InputAction::TouchpadTap { fingers: 4 }]).is_err());
        assert!(
            validate(&[InputAction::TouchpadSwipe {
                fingers: 1,
                dx_mm: 0,
                dy_mm: 0,
                duration_ms: 10
            }])
            .is_err()
        );
        assert!(
            validate(&[InputAction::KeyDown {
                combo: "shift".into()
            }])
            .is_ok()
        );
        assert!(
            validate(&[InputAction::KeyUp {
                combo: "nokey".into()
            }])
            .is_err()
        );
    }

    #[test]
    fn new_actions_round_trip_as_tagged_json() {
        let actions = [
            InputAction::TouchPinch {
                center: (400, 300),
                from_distance: 100,
                to_distance: 300,
                duration_ms: 400,
            },
            InputAction::PenDrag {
                from: (1, 2),
                to: (3, 4),
                duration_ms: 200,
                pressure: 60,
            },
            InputAction::GamepadStick {
                stick: Stick::Right,
                x: -100,
                y: 50,
                duration_ms: 250,
            },
            InputAction::GamepadPress {
                combos: vec!["guide+lb".into()],
            },
        ];
        for action in actions {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(serde_json::from_str::<InputAction>(&text).unwrap(), action);
        }
        assert_eq!(
            serde_json::to_string(&InputAction::TouchTap { x: 1, y: 2 }).unwrap(),
            r#"{"kind":"touch_tap","x":1,"y":2}"#
        );
    }

    /// Combos map to evdev codes, text types on a US layout, validation names the problem, and actions
    /// round-trip as tagged JSON.
    #[test]
    fn input_actions_map_validate_and_round_trip() {
        // Phase: combos_map_to_evdev_codes.
        {
            assert_eq!(combo_codes("ctrl+alt+t").unwrap(), [29, 56, 20]);
            assert_eq!(combo_codes("Return").unwrap(), [28]);
            assert_eq!(combo_codes("super").unwrap(), [125]);
            assert_eq!(combo_codes("ctrl+T").unwrap(), [29, 20]);
            assert_eq!(combo_codes("shift+/").unwrap(), [42, 53]);
            assert_eq!(combo_codes("?").unwrap(), [42, 53]);
            assert_eq!(combo_codes("ctrl++").unwrap(), [29, 42, 13]);
            assert_eq!(combo_codes("F12").unwrap(), [88]);
            assert_eq!(combo_codes("ctrl+nokey"), Err("nokey".into()));
            assert!(combo_codes("").is_err());
        }

        // Phase: text_types_on_a_us_layout.
        {
            let strokes = text_strokes("Hi!\n").unwrap();
            assert_eq!(
                strokes,
                [
                    Stroke {
                        code: 35,
                        shift: true
                    },
                    Stroke {
                        code: 23,
                        shift: false
                    },
                    Stroke {
                        code: 2,
                        shift: true
                    },
                    Stroke {
                        code: 28,
                        shift: false
                    },
                ]
            );
            assert_eq!(text_strokes("zé"), Err('é'));
            for c in ('a'..='z')
                .chain('A'..='Z')
                .chain("0123456789 -=[];'`\\,./!@#$%^&*()_+{}:\"~|<>?".chars())
            {
                let stroke = char_stroke(c).unwrap();
                assert!(stroke.code > 0 && stroke.code <= KEY_MAX_USED, "{c}");
            }
        }

        // Phase: validation_names_the_problem.
        {
            assert!(
                validate(&[InputAction::Key {
                    combos: vec!["ctrl+c".into()]
                }])
                .is_ok()
            );
            let error = validate(&[InputAction::Type {
                text: "naïve".into(),
            }])
            .unwrap_err();
            assert!(error.contains('ï'), "{error}");
            assert!(validate(&[InputAction::Key { combos: vec![] }]).is_err());
        }

        // Phase: actions_round_trip_as_tagged_json.
        {
            let action = InputAction::Click {
                button: Button::Right,
                at: Some((10, 20)),
            };
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(text, r#"{"kind":"click","button":"right","at":[10,20]}"#);
            assert_eq!(serde_json::from_str::<InputAction>(&text).unwrap(), action);
        }
    }
}
