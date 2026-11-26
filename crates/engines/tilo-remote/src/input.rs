//! What a person's pointer and keys mean to a remote desktop: the button mask, the wheel
//! clicks, and the X11 keysym a VNC `KeyEvent` carries.
//!
//! The one vocabulary the toolkit hands a focused component is shared; a VNC server's is not,
//! so the meaning of each key is decided here, where the protocol is, and never in a view.

/// One input a VNC session sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VncInput {
    /// The pointer's position with the buttons held, bit 0 left, 1 middle, 2 right.
    Pointer { button_mask: u8, x: u16, y: u16 },
    /// A key pressed or released, by X11 keysym.
    Key { down: bool, key: u32 },
}

/// A key as a person pressed it, in the vocabulary the toolkit hands a focused component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Named(Named),
}

/// The keys a program reads as an escape sequence rather than as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Enter,
    Space,
    Backspace,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// A function key, numbered from 1.
    Function(u8),
}

/// The VNC buttons the wheel clicks, toward and away.
pub const WHEEL_UP: u8 = 0x08;
pub const WHEEL_DOWN: u8 = 0x10;
pub const WHEEL_LEFT: u8 = 0x20;
pub const WHEEL_RIGHT: u8 = 0x40;

/// The most wheel clicks one wheel event sends a VNC desktop, so a fling cannot fill the
/// session's bounded input queue.
pub const VNC_WHEEL_BURST: usize = 10;

/// The wheel buttons to click on a VNC desktop for a move of `across` and `down` lines, carrying
/// what is left of a click between events: `delta` in logical lines, positive down and to the
/// end.
#[must_use]
pub fn vnc_wheel_clicks(pending: &mut (f64, f64), across: f64, down: f64) -> Vec<u8> {
    let mut clicks = Vec::new();
    for (moved, pending, toward, away) in [
        (down, &mut pending.1, WHEEL_DOWN, WHEEL_UP),
        (across, &mut pending.0, WHEEL_RIGHT, WHEEL_LEFT),
    ] {
        if !moved.is_finite() {
            continue;
        }
        *pending += moved;
        let mut whole = pending.trunc();
        *pending -= whole;
        let button = if whole > 0.0 { toward } else { away };
        for _ in 0..VNC_WHEEL_BURST {
            if whole.abs() < 1.0 {
                break;
            }
            clicks.push(button);
            whole -= whole.signum();
        }
    }
    clicks
}

/// The X11 keysym a VNC `KeyEvent` sends for `key`: a character, or one of the named keys a
/// terminal and a desktop agree on.
#[must_use]
pub fn keysym(key: Key) -> Option<u32> {
    match key {
        Key::Char(character) => {
            let code = u32::from(character);
            Some(if code <= 0xff {
                code
            } else {
                0x0100_0000 | code
            })
        }
        Key::Named(named) => Some(named_keysym(named)),
    }
}

fn named_keysym(named: Named) -> u32 {
    match named {
        Named::Enter => 0xff0d,
        Named::Space => 0x20,
        Named::Backspace => 0xff08,
        Named::Tab => 0xff09,
        Named::Escape => 0xff1b,
        Named::ArrowUp => 0xff52,
        Named::ArrowDown => 0xff54,
        Named::ArrowLeft => 0xff51,
        Named::ArrowRight => 0xff53,
        Named::Home => 0xff50,
        Named::End => 0xff57,
        Named::PageUp => 0xff55,
        Named::PageDown => 0xff56,
        Named::Insert => 0xff63,
        Named::Delete => 0xffff,
        Named::Function(number) => 0xffbe_u32.saturating_add(u32::from(number.saturating_sub(1))),
    }
}

/// The keysyms of the modifiers a person holds, so a remote desktop sees them held as they are
/// over the key they press.
pub mod modifier {
    /// Shift.
    pub const SHIFT: u32 = 0xffe1;
    /// Control.
    pub const CONTROL: u32 = 0xffe3;
    /// Alt.
    pub const ALT: u32 = 0xffe9;
    /// Super, or Meta.
    pub const SUPER: u32 = 0xffeb;
}

/// What a person holds down on a VNC desktop: the keys pressed and not let go, the modifiers the
/// desktop was told are down, the buttons, and where the pointer is. Each change answers the
/// inputs that tell the desktop of it, and [`Held::let_go`] answers the ones that let go of
/// everything, so a key or button held when the screen loses the keyboard is released on the far
/// machine too and nothing stays pressed there.
#[derive(Debug, Default)]
pub struct Held {
    keys: Vec<u32>,
    modifiers: Vec<u32>,
    buttons: u8,
    at: Option<(u16, u16)>,
}

impl Held {
    /// The modifiers (keysyms from [`modifier`]) held now: those the desktop was not told of go
    /// down, those no longer held come up.
    pub fn modifiers(&mut self, down: &[u32]) -> Vec<VncInput> {
        let mut said = Vec::new();
        for &key in &self.modifiers {
            if !down.contains(&key) {
                said.push(VncInput::Key { down: false, key });
            }
        }
        for &key in down {
            if !self.modifiers.contains(&key) {
                said.push(VncInput::Key { down: true, key });
            }
        }
        self.modifiers = down.to_vec();
        said
    }

    /// `key` pressed: it is held until [`Held::release`] or [`Held::let_go`].
    pub fn press(&mut self, key: u32) -> Vec<VncInput> {
        if !self.keys.contains(&key) {
            self.keys.push(key);
        }
        vec![VncInput::Key { down: true, key }]
    }

    /// `key` let go.
    pub fn release(&mut self, key: u32) -> Vec<VncInput> {
        self.keys.retain(|held| *held != key);
        vec![VncInput::Key { down: false, key }]
    }

    /// The pointer moved to `at`, in the desktop's pixels, with the buttons still held.
    pub fn point(&mut self, at: (u16, u16)) -> Vec<VncInput> {
        self.at = Some(at);
        self.pointer(self.buttons)
    }

    /// A button (bit 0 left, 1 middle, 2 right) pressed or let go where the pointer is.
    pub fn button(&mut self, bit: u8, pressed: bool) -> Vec<VncInput> {
        self.buttons = if pressed {
            self.buttons | bit
        } else {
            self.buttons & !bit
        };
        self.pointer(self.buttons)
    }

    /// The wheel's `clicks` ([`vnc_wheel_clicks`]), each pressed and let go over the held buttons.
    #[must_use]
    pub fn wheel(&self, clicks: &[u8]) -> Vec<VncInput> {
        clicks
            .iter()
            .flat_map(|click| {
                [
                    self.pointer(self.buttons | click),
                    self.pointer(self.buttons),
                ]
            })
            .flatten()
            .collect()
    }

    /// Everything held let go: the keys, the modifiers and the buttons.
    pub fn let_go(&mut self) -> Vec<VncInput> {
        let mut said: Vec<VncInput> = std::mem::take(&mut self.keys)
            .into_iter()
            .chain(std::mem::take(&mut self.modifiers))
            .map(|key| VncInput::Key { down: false, key })
            .collect();
        if self.buttons != 0 {
            self.buttons = 0;
            said.extend(self.pointer(0));
        }
        said
    }

    fn pointer(&self, button_mask: u8) -> Vec<VncInput> {
        self.at
            .map(|(x, y)| VncInput::Pointer { button_mask, x, y })
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn keys_map_to_what_a_vnc_desktop_expects;

        phase characters_and_named_keys_carry_their_keysyms;
        {
            assert_eq!(keysym(Key::Char('a')), Some(0x61));
            assert_eq!(keysym(Key::Char('é')), Some(0xe9), "a Latin-1 letter is its own keysym");
            assert_eq!(keysym(Key::Char('λ')), Some(0x0100_03bb));
            for (named, expected) in [
                (Named::Enter, 0xff0d),
                (Named::Backspace, 0xff08),
                (Named::Tab, 0xff09),
                (Named::Escape, 0xff1b),
                (Named::Space, 0x20),
                (Named::ArrowUp, 0xff52),
                (Named::Delete, 0xffff),
                (Named::Insert, 0xff63),
                (Named::Function(1), 0xffbe),
                (Named::Function(12), 0xffc9),
            ] {
                assert_eq!(keysym(Key::Named(named)), Some(expected), "{named:?}");
            }
        }

        phase the_wheel_turns_whole_lines_into_buttons_and_carries_the_rest;
        {
            let mut pending = (0.0, 0.0);
            assert_eq!(vnc_wheel_clicks(&mut pending, 0.0, 2.5), [WHEEL_DOWN, WHEEL_DOWN]);
            assert!(
                (pending.1 - 0.5).abs() < f64::EPSILON,
                "half a line waits for the next event"
            );
            assert_eq!(
                vnc_wheel_clicks(&mut pending, 0.0, -0.75),
                Vec::<u8>::new(),
                "half a line and three quarters against it still make no whole click"
            );
            assert!((pending.1 - -0.25).abs() < f64::EPSILON);
            assert_eq!(
                vnc_wheel_clicks(&mut pending, 0.0, -1.0),
                [WHEEL_UP],
                "a whole click up after it"
            );
            assert!((pending.1 - -0.25).abs() < f64::EPSILON);
            assert_eq!(
                vnc_wheel_clicks(&mut pending, 3.0, 0.0),
                [WHEEL_RIGHT, WHEEL_RIGHT, WHEEL_RIGHT]
            );
        }

        phase a_fling_is_cut_at_ten_clicks_an_event;
        {
            let mut pending = (0.0, 0.0);
            assert_eq!(vnc_wheel_clicks(&mut pending, 0.0, 50.0).len(), VNC_WHEEL_BURST);
            assert_eq!(
                vnc_wheel_clicks(&mut pending, f64::INFINITY, 0.0),
                Vec::<u8>::new(),
                "a move that is no number sends nothing"
            );
        }

        phase what_is_held_when_the_screen_loses_the_keyboard_is_let_go_on_the_desktop;
        {
            let mut held = Held::default();
            assert!(held.point((10, 20)).len() == 1);
            let shift = modifier::SHIFT;
            assert_eq!(held.modifiers(&[shift]), [VncInput::Key { down: true, key: shift }]);
            assert!(held.modifiers(&[shift]).is_empty(), "a modifier is said once");
            let a = keysym(Key::Char('a')).unwrap();
            held.press(a);
            held.press(a);
            held.button(0b001, true);
            assert_eq!(
                held.let_go(),
                [
                    VncInput::Key { down: false, key: a },
                    VncInput::Key { down: false, key: shift },
                    VncInput::Pointer { button_mask: 0, x: 10, y: 20 },
                ],
                "the key, the modifier and the button all come up, once each"
            );
            assert!(held.let_go().is_empty(), "nothing is held after");
            // A key the window took the release of is let go on its own.
            held.press(a);
            assert_eq!(held.release(a), [VncInput::Key { down: false, key: a }]);
            assert!(held.let_go().is_empty());
            // A wheel click is pressed and let go over the buttons still held.
            held.button(0b001, true);
            assert_eq!(
                held.wheel(&[WHEEL_DOWN]),
                [
                    VncInput::Pointer { button_mask: 0b001 | WHEEL_DOWN, x: 10, y: 20 },
                    VncInput::Pointer { button_mask: 0b001, x: 10, y: 20 },
                ]
            );
        }
    }
}
