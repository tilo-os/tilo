//! Key codes from libinput into the toolkit's key events, through a keymap
//! compiled for the layout the host was given, with key repeat.

use std::time::{Duration, Instant};

use winit::event::{ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, ModifiersState, PhysicalKey, SmolStr};
use winit::platform::scancode::PhysicalKeyExtScancode as _;
use xkbcommon::xkb;

/// The layout to compile, as the desktop's keyboard settings name it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// An xkb layout name (`us`, `de`, `fr`); empty is the system default.
    pub layout: String,
    /// One of the layout's variants (`nodeadkeys`); empty is the layout.
    pub variant: String,
}

/// How long a key is held before it repeats, and how often it then does.
const REPEAT_DELAY: Duration = Duration::from_millis(500);
const REPEAT_INTERVAL: Duration = Duration::from_millis(33);

/// Evdev key codes are xkb's less eight.
const EVDEV_OFFSET: u32 = 8;

pub struct Keyboard {
    keymap: xkb::Keymap,
    state: xkb::State,
    modifiers: ModifiersState,
    /// The key held that repeats, and when it next does.
    repeat: Option<(u32, Instant)>,
}

impl Keyboard {
    /// A keyboard on `layout`, else on the default layout, else `None` when
    /// no keymap compiles (the xkb data is missing).
    pub fn new(layout: &Layout) -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            layout.layout.as_str(),
            layout.variant.as_str(),
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .or_else(|| {
            tracing::warn!(
                ?layout,
                "keyboard layout does not compile; using the default"
            );
            xkb::Keymap::new_from_names(&context, "", "", "", "", None, 0)
        })?;
        let state = xkb::State::new(&keymap);
        Some(Self {
            keymap,
            state,
            modifiers: ModifiersState::empty(),
            repeat: None,
        })
    }

    /// The events key `code` going down or up makes: the modifiers when they
    /// change, then the key itself.
    pub fn key(&mut self, code: u32, pressed: bool, now: Instant) -> Vec<WindowEvent> {
        let keycode = xkb::Keycode::new(code.saturating_add(EVDEV_OFFSET));
        let mut events = Vec::with_capacity(2);
        // The key's meaning is read before the state takes it in, as a
        // compositor does: Shift's own press is not shifted.
        let event = self.event(code, keycode, pressed, false);
        self.state.update_key(
            keycode,
            if pressed {
                xkb::KeyDirection::Down
            } else {
                xkb::KeyDirection::Up
            },
        );
        events.push(event);
        let modifiers = self.read_modifiers();
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            events.push(WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
        }
        if pressed && self.keymap.key_repeats(keycode) {
            self.repeat = now.checked_add(REPEAT_DELAY).map(|at| (code, at));
        } else if self.repeat.is_some_and(|(held, _)| held == code) {
            self.repeat = None;
        }
        events
    }

    /// When the held key next repeats, if one does.
    pub fn repeat_deadline(&self) -> Option<Instant> {
        self.repeat.map(|(_, at)| at)
    }

    /// The repeats due by `now`.
    pub fn repeats(&mut self, now: Instant) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        while let Some((code, at)) = self.repeat
            && at <= now
        {
            let keycode = xkb::Keycode::new(code.saturating_add(EVDEV_OFFSET));
            events.push(self.event(code, keycode, true, true));
            // A host that slept through many repeats sends one, not a burst.
            let from = if now.saturating_duration_since(at) > REPEAT_INTERVAL {
                now
            } else {
                at
            };
            self.repeat = from.checked_add(REPEAT_INTERVAL).map(|next| (code, next));
        }
        events
    }

    /// Every key is up: a device went away, or the host lost the input.
    pub fn release_all(&mut self) -> Vec<WindowEvent> {
        self.repeat = None;
        self.state = xkb::State::new(&self.keymap);
        if self.modifiers.is_empty() {
            return Vec::new();
        }
        self.modifiers = ModifiersState::empty();
        vec![WindowEvent::ModifiersChanged(Modifiers::from(
            ModifiersState::empty(),
        ))]
    }

    fn read_modifiers(&self) -> ModifiersState {
        let active = |name: &str| {
            self.state
                .mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE)
        };
        let mut modifiers = ModifiersState::empty();
        modifiers.set(ModifiersState::SHIFT, active(xkb::MOD_NAME_SHIFT));
        modifiers.set(ModifiersState::CONTROL, active(xkb::MOD_NAME_CTRL));
        modifiers.set(ModifiersState::ALT, active(xkb::MOD_NAME_ALT));
        modifiers.set(ModifiersState::SUPER, active(xkb::MOD_NAME_LOGO));
        modifiers
    }

    fn event(&self, code: u32, keycode: xkb::Keycode, pressed: bool, repeat: bool) -> WindowEvent {
        let keysym = self.state.key_get_one_sym(keycode);
        let (logical_key, location) = key_for(keysym, || self.state.key_get_utf8(keycode));
        let unshifted = self
            .keymap
            .key_get_syms_by_level(keycode, self.state.key_get_layout(keycode), 0)
            .first()
            .copied()
            .unwrap_or(keysym);
        let (key_without_modifiers, _) = key_for(unshifted, || xkb::keysym_to_utf8(unshifted));
        let state = if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        let text = pressed
            .then(|| self.state.key_get_utf8(keycode))
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
            .map(SmolStr::new);
        let event = KeyEvent::synthetic(
            RawKeyEvent {
                physical_key: PhysicalKey::from_scancode(code),
                state,
            },
            logical_key,
            text.clone(),
            location,
            repeat,
            text,
            key_without_modifiers,
        );
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }
}

/// The key `keysym` is: a named key when winit names it, else the text it
/// types.
fn key_for(
    keysym: xkb::Keysym,
    text: impl FnOnce() -> String,
) -> (Key, winit::keyboard::KeyLocation) {
    let (key, location) = winit::platform::scancode::keysym_to_key(keysym.raw());
    if !matches!(key, Key::Unidentified(_)) {
        return (key, location);
    }
    let text = text();
    if text.is_empty() || text.chars().any(char::is_control) {
        (key, location)
    } else {
        (Key::Character(SmolStr::new(text)), location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::NamedKey;

    /// evdev codes from linux/input-event-codes.h.
    const KEY_A: u32 = 30;
    const KEY_Q: u32 = 16;
    const KEY_ENTER: u32 = 28;
    const KEY_LEFTSHIFT: u32 = 42;

    fn keyboard(layout: &str) -> Option<Keyboard> {
        Keyboard::new(&Layout {
            layout: layout.to_owned(),
            variant: String::new(),
        })
    }

    fn typed(events: &[WindowEvent]) -> Vec<(Key, Option<SmolStr>)> {
        events
            .iter()
            .filter_map(|event| match event {
                WindowEvent::KeyboardInput { event, .. } => {
                    Some((event.logical_key.clone(), event.text.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn keys_type_through_the_layout_with_shift_and_named_keys() {
        // A machine without the xkb data compiles no keymap; nothing to check.
        let Some(mut keyboard) = keyboard("us") else {
            return;
        };
        let now = Instant::now();
        assert_eq!(
            typed(&keyboard.key(KEY_A, true, now)),
            [(Key::Character("a".into()), Some("a".into()))]
        );
        keyboard.key(KEY_A, false, now);
        let shift = keyboard.key(KEY_LEFTSHIFT, true, now);
        assert!(shift.iter().any(|event| matches!(
            event,
            WindowEvent::ModifiersChanged(modifiers) if modifiers.state().shift_key()
        )));
        assert_eq!(
            typed(&keyboard.key(KEY_A, true, now)),
            [(Key::Character("A".into()), Some("A".into()))]
        );
        keyboard.key(KEY_A, false, now);
        keyboard.key(KEY_LEFTSHIFT, false, now);
        assert_eq!(
            typed(&keyboard.key(KEY_ENTER, true, now)),
            [(Key::Named(NamedKey::Enter), None)]
        );
    }

    #[test]
    fn the_layout_given_decides_what_a_key_types() {
        let Some(mut keyboard) = keyboard("fr") else {
            return;
        };
        // AZERTY: the key where QWERTY has Q types A.
        assert_eq!(
            typed(&keyboard.key(KEY_Q, true, Instant::now())),
            [(Key::Character("a".into()), Some("a".into()))]
        );
    }

    #[test]
    fn a_held_key_repeats_after_the_delay_and_stops_when_released() {
        let Some(mut keyboard) = keyboard("us") else {
            return;
        };
        let start = Instant::now();
        keyboard.key(KEY_A, true, start);
        assert!(
            keyboard
                .repeats(start + Duration::from_millis(100))
                .is_empty()
        );
        let repeated = keyboard.repeats(start + REPEAT_DELAY);
        assert_eq!(repeated.len(), 1);
        assert!(matches!(
            &repeated[0],
            WindowEvent::KeyboardInput { event, .. } if event.repeat
        ));
        keyboard.key(KEY_A, false, start + REPEAT_DELAY);
        assert_eq!(keyboard.repeat_deadline(), None);
    }
}
