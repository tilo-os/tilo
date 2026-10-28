//! The chord that turns the screen reader on and off on every screen: Ctrl +
//! Super + Enter, and on a device without a keyboard both volume keys held
//! for three seconds (docs/security/sign-in.md §3.4).
//!
//! One recognizer for every host, so no screen can lack the chord or read it
//! differently: toolkit apps feed it winit's key events ([`ChordKey::from_winit`]),
//! the compositor and the KMS/libinput host feed it xkbcommon keysyms
//! ([`ChordKey::from_keysym`]). It is pure: the host passes the time of
//! each event and arms a timer for [`ScreenReaderChord::deadline`], then
//! calls [`ScreenReaderChord::tick`] when it fires.

use crate::time::{Duration, Instant};

/// How long both volume keys are held before the screen reader toggles.
pub const VOLUME_HOLD: Duration = Duration::from_secs(3);

/// The keys the chord is made of; every other key is [`ChordKey::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordKey {
    /// Enter or the keypad's Enter.
    Enter,
    VolumeUp,
    VolumeDown,
    Other,
}

/// xkbcommon keysyms the chord is made of.
const XKB_KEY_RETURN: u32 = 0xff0d;
const XKB_KEY_KP_ENTER: u32 = 0xff8d;
const XKB_KEY_XF86_AUDIO_RAISE_VOLUME: u32 = 0x1008_ff13;
const XKB_KEY_XF86_AUDIO_LOWER_VOLUME: u32 = 0x1008_ff11;

impl ChordKey {
    /// The key an xkbcommon keysym names.
    #[must_use]
    pub fn from_keysym(keysym: u32) -> Self {
        match keysym {
            XKB_KEY_RETURN | XKB_KEY_KP_ENTER => Self::Enter,
            XKB_KEY_XF86_AUDIO_RAISE_VOLUME => Self::VolumeUp,
            XKB_KEY_XF86_AUDIO_LOWER_VOLUME => Self::VolumeDown,
            _ => Self::Other,
        }
    }

    /// The key a winit key event is, by its physical position first (the
    /// layout cannot move Enter or the volume keys), then by what it types.
    #[must_use]
    pub fn from_winit(event: &winit::event::KeyEvent) -> Self {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => return Self::Enter,
            PhysicalKey::Code(KeyCode::AudioVolumeUp) => return Self::VolumeUp,
            PhysicalKey::Code(KeyCode::AudioVolumeDown) => return Self::VolumeDown,
            _ => {}
        }
        match event.logical_key {
            Key::Named(NamedKey::Enter) => Self::Enter,
            Key::Named(NamedKey::AudioVolumeUp) => Self::VolumeUp,
            Key::Named(NamedKey::AudioVolumeDown) => Self::VolumeDown,
            _ => Self::Other,
        }
    }
}

/// The modifiers held with a key. The chord wants exactly Ctrl and Super.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent modifier keys, as every host reports them"
)]
pub struct ChordModifiers {
    pub ctrl: bool,
    pub super_key: bool,
    pub alt: bool,
    pub shift: bool,
}

impl ChordModifiers {
    /// winit's modifier state.
    #[must_use]
    pub fn from_winit(state: winit::keyboard::ModifiersState) -> Self {
        Self {
            ctrl: state.control_key(),
            super_key: state.super_key(),
            alt: state.alt_key(),
            shift: state.shift_key(),
        }
    }

    fn is_ctrl_super(self) -> bool {
        self.ctrl && self.super_key && !self.alt && !self.shift
    }
}

/// One key going down or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordEvent {
    pub key: ChordKey,
    pub modifiers: ChordModifiers,
    pub pressed: bool,
    /// The key is held and auto-repeating.
    pub repeat: bool,
    pub at: Instant,
}

/// Recognizes the screen reader chord over a host's key events.
#[derive(Clone, Debug, Default)]
pub struct ScreenReaderChord {
    volume_up: bool,
    volume_down: bool,
    /// When both volume keys became held; `None` once this hold toggled.
    both_since: Option<Instant>,
}

impl ScreenReaderChord {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one key event. `true` when it toggles the screen reader: the
    /// press of Enter under exactly Ctrl + Super, or the event that finds
    /// both volume keys held for [`VOLUME_HOLD`].
    pub fn key(&mut self, event: ChordEvent) -> bool {
        match event.key {
            ChordKey::Enter => event.pressed && !event.repeat && event.modifiers.is_ctrl_super(),
            ChordKey::VolumeUp | ChordKey::VolumeDown => {
                let both_before = self.volume_up && self.volume_down;
                if event.key == ChordKey::VolumeUp {
                    self.volume_up = event.pressed;
                } else {
                    self.volume_down = event.pressed;
                }
                match (both_before, self.volume_up && self.volume_down) {
                    (false, true) => {
                        self.both_since = Some(event.at);
                        false
                    }
                    (true, false) => {
                        self.both_since = None;
                        false
                    }
                    _ => self.tick(event.at),
                }
            }
            ChordKey::Other => false,
        }
    }

    /// When the volume hold toggles if both keys stay down; the host arms a
    /// timer for it and calls [`Self::tick`] then.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.both_since.map(|since| since + VOLUME_HOLD)
    }

    /// `true` once per hold, at or after [`Self::deadline`].
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|due| now >= due) {
            self.both_since = None;
            return true;
        }
        false
    }

    /// Forget held keys, as when the host loses keyboard focus and will not
    /// see their release.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// What is said when the chord turns the screen reader on or off, in the
/// interface language; English when no catalog is loaded.
#[must_use]
pub fn state_words(on: bool) -> String {
    let (key, english) = if on {
        ("screen-reader-turned-on", "Screen reader on")
    } else {
        ("screen-reader-turned-off", "Screen reader off")
    };
    let resolved = crate::i18n!(key);
    if resolved == key {
        english.to_owned()
    } else {
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_SUPER: ChordModifiers = ChordModifiers {
        ctrl: true,
        super_key: true,
        alt: false,
        shift: false,
    };

    fn event(key: ChordKey, modifiers: ChordModifiers, pressed: bool, at: Instant) -> ChordEvent {
        ChordEvent {
            key,
            modifiers,
            pressed,
            repeat: false,
            at,
        }
    }

    #[test]
    fn ctrl_super_enter_toggles_on_the_press_only() {
        let now = crate::time::now();
        let mut chord = ScreenReaderChord::new();
        assert!(chord.key(event(ChordKey::Enter, CTRL_SUPER, true, now)));
        assert!(!chord.key(ChordEvent {
            repeat: true,
            ..event(ChordKey::Enter, CTRL_SUPER, true, now)
        }));
        assert!(!chord.key(event(ChordKey::Enter, CTRL_SUPER, false, now)));
        assert!(!chord.key(event(ChordKey::Other, CTRL_SUPER, true, now)));
    }

    #[test]
    fn the_modifiers_must_be_exactly_ctrl_and_super() {
        let now = crate::time::now();
        let mut chord = ScreenReaderChord::new();
        for modifiers in [
            ChordModifiers::default(),
            ChordModifiers {
                ctrl: true,
                ..ChordModifiers::default()
            },
            ChordModifiers {
                super_key: true,
                ..ChordModifiers::default()
            },
            ChordModifiers {
                shift: true,
                ..CTRL_SUPER
            },
            ChordModifiers {
                alt: true,
                ..CTRL_SUPER
            },
        ] {
            assert!(
                !chord.key(event(ChordKey::Enter, modifiers, true, now)),
                "{modifiers:?}"
            );
        }
    }

    #[test]
    fn both_volume_keys_held_three_seconds_toggle_once() {
        let start = crate::time::now();
        let none = ChordModifiers::default();
        let mut chord = ScreenReaderChord::new();
        assert!(!chord.key(event(ChordKey::VolumeUp, none, true, start)));
        assert_eq!(chord.deadline(), None, "one key is not the chord");
        let both = start + Duration::from_millis(200);
        assert!(!chord.key(event(ChordKey::VolumeDown, none, true, both)));
        assert_eq!(chord.deadline(), Some(both + VOLUME_HOLD));
        assert!(!chord.tick(both + Duration::from_millis(2_999)));
        // Auto-repeat of a held key does not restart the hold.
        assert!(!chord.key(ChordEvent {
            repeat: true,
            ..event(
                ChordKey::VolumeUp,
                none,
                true,
                both + Duration::from_secs(1)
            )
        }));
        assert!(chord.tick(both + VOLUME_HOLD));
        assert_eq!(chord.deadline(), None);
        assert!(!chord.tick(both + Duration::from_secs(10)), "once per hold");
        // Releasing one key and pressing it again starts a new hold.
        let again = both + Duration::from_secs(11);
        assert!(!chord.key(event(ChordKey::VolumeDown, none, false, again)));
        assert!(!chord.key(event(ChordKey::VolumeDown, none, true, again)));
        assert!(chord.key(ChordEvent {
            repeat: true,
            ..event(ChordKey::VolumeUp, none, true, again + VOLUME_HOLD)
        }));
    }

    #[test]
    fn releasing_either_volume_key_cancels_the_hold() {
        let start = crate::time::now();
        let none = ChordModifiers::default();
        let mut chord = ScreenReaderChord::new();
        chord.key(event(ChordKey::VolumeDown, none, true, start));
        chord.key(event(ChordKey::VolumeUp, none, true, start));
        chord.key(event(
            ChordKey::VolumeUp,
            none,
            false,
            start + Duration::from_secs(2),
        ));
        assert_eq!(chord.deadline(), None);
        assert!(!chord.tick(start + Duration::from_secs(5)));
        chord.key(event(ChordKey::VolumeUp, none, true, start));
        chord.reset();
        assert_eq!(chord.deadline(), None);
    }

    #[test]
    fn the_new_state_is_said_in_words() {
        assert_eq!(state_words(true), "Screen reader on");
        assert_eq!(state_words(false), "Screen reader off");
    }

    #[test]
    fn keysyms_and_winit_keys_name_the_same_keys() {
        assert_eq!(ChordKey::from_keysym(0xff0d), ChordKey::Enter);
        assert_eq!(ChordKey::from_keysym(0xff8d), ChordKey::Enter);
        assert_eq!(ChordKey::from_keysym(0x1008_ff13), ChordKey::VolumeUp);
        assert_eq!(ChordKey::from_keysym(0x1008_ff11), ChordKey::VolumeDown);
        assert_eq!(ChordKey::from_keysym(u32::from('a')), ChordKey::Other);

        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        let key = |physical, logical| {
            let mut event = crate::synthetic_input::key_event(
                logical,
                None,
                winit::event::ElementState::Pressed,
            );
            event.physical_key = physical;
            ChordKey::from_winit(&event)
        };
        assert_eq!(
            key(
                PhysicalKey::Code(KeyCode::NumpadEnter),
                Key::Named(NamedKey::Enter)
            ),
            ChordKey::Enter
        );
        assert_eq!(
            key(
                PhysicalKey::Code(KeyCode::AudioVolumeUp),
                Key::Named(NamedKey::AudioVolumeUp)
            ),
            ChordKey::VolumeUp
        );
        assert_eq!(
            key(
                PhysicalKey::Code(KeyCode::AudioVolumeDown),
                Key::Named(NamedKey::AudioVolumeDown)
            ),
            ChordKey::VolumeDown
        );
        assert_eq!(
            key(PhysicalKey::Code(KeyCode::KeyA), Key::Character("a".into())),
            ChordKey::Other
        );
    }
}
