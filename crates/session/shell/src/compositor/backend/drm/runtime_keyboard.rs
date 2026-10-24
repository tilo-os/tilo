//! The DRM backend's keyboard: xkb turns the raw keycodes libinput reports
//! into the winit key events the shell's chords and fields read. Clients
//! receive the raw keycodes and their own keymap; this is the shell's view.

use smithay::input::keyboard::{Keysym, keysyms, xkb};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, NativeKey, PhysicalKey, SmolStr};
use winit::platform::scancode::PhysicalKeyExtScancode as _;

use super::super::input_config::KeyboardSettings;
use super::evdev_to_xkb;
use crate::compositor::backend::BackendEvent;

/// xkb keymap + state owned by the DRM backend, turning libinput keycodes
/// into the winit `KeyboardInput` the chrome understands — the winit backend gets these from the OS, but bare-TTY libinput only reports raw keycodes.
pub struct KeyboardXkb {
    // Held for the lifetime of `state`. xkbcommon refcounts internally, but keeping ownership explicit makes the lifetime obvious (never read after construction, hence the underscore).
    _context: xkb::Context,
    keymap: xkb::Keymap,
    state: xkb::State,
    /// Last modifier mask we emitted — we only push `ModifiersChanged` when it actually flips, matching winit's behaviour.
    mods: ModifiersState,
    /// The layouts, variants and options this keymap was compiled from, so
    /// a settings reload that only moves the active source switches groups
    /// instead of recompiling and dropping held modifiers.
    compiled: (String, String, String, Option<String>),
}

/// What translating one key event yields: an optional `KeyboardInput` (absent for keys with no logical mapping, e.g. bare modifiers) and an optional `ModifiersChanged` (present only when the mask flips).
pub(super) struct KeyOutcome {
    pub(super) input: Option<WindowEvent>,
    pub(super) modifiers_changed: Option<Modifiers>,
}

impl KeyboardXkb {
    /// Compile the validated source list from the per-user settings file.
    /// Returns `None` (caller logs) if the keymap can't build, degrading to
    /// pointer-only at boot and preserving the old working map on hot reload.
    pub fn new() -> Option<Self> {
        let requested = KeyboardSettings::load();
        Self::from_settings(&requested).or_else(|| {
            tracing::warn!(
                sources = ?requested.source_ids,
                "DRM input: saved XKB sources failed at session start; falling back to US"
            );
            Self::from_settings(&KeyboardSettings::default())
        })
    }

    pub fn from_settings(settings: &KeyboardSettings) -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",              // rules -> XKB_DEFAULT_RULES (normally evdev)
            &settings.model, // explicit physical model
            &settings.layouts,
            &settings.variants,
            settings.options.clone(),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        let mut state = xkb::State::new(&keymap);
        let active = u32::try_from(settings.active_index).ok()?;
        let _ = state.update_mask(0, 0, 0, 0, 0, active);
        Some(Self {
            _context: context,
            keymap,
            state,
            mods: ModifiersState::empty(),
            compiled: Self::keymap_names(settings),
        })
    }

    fn keymap_names(settings: &KeyboardSettings) -> (String, String, String, Option<String>) {
        (
            settings.model.clone(),
            settings.layouts.clone(),
            settings.variants.clone(),
            settings.options.clone(),
        )
    }

    /// Whether `settings` compile to exactly this keymap.
    pub(crate) fn compiles_to_same_keymap(&self, settings: &KeyboardSettings) -> bool {
        self.compiled == Self::keymap_names(settings)
    }

    /// Switch to input source `index`, keeping every held and locked
    /// modifier. An index past the configured sources is ignored.
    pub fn set_active_layout(&mut self, index: usize) {
        let Ok(group) = u32::try_from(index) else {
            return;
        };
        if group >= self.keymap.num_layouts() {
            return;
        }
        let depressed = self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED);
        let latched = self.state.serialize_mods(xkb::STATE_MODS_LATCHED);
        let locked = self.state.serialize_mods(xkb::STATE_MODS_LOCKED);
        let _ = self
            .state
            .update_mask(depressed, latched, locked, 0, 0, group);
    }

    /// The active input source, counting from 0.
    pub fn active_layout(&self) -> usize {
        usize::try_from(self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE)).unwrap_or(0)
    }

    pub(super) fn key_repeats(&self, keycode: xkb::Keycode) -> bool {
        self.keymap.key_repeats(keycode)
    }

    /// The evdev keycode that types `ch` without modifiers on the active
    /// layout, if any key does.
    pub(super) fn keycode_for_char(&self, ch: char) -> Option<u32> {
        let wanted = xkb::utf32_to_keysym(u32::from(ch.to_ascii_lowercase()));
        let layout = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        (self.keymap.min_keycode().raw()..=self.keymap.max_keycode().raw())
            .find(|raw| {
                self.keymap
                    .key_get_syms_by_level(xkb::Keycode::new(*raw), layout, 0)
                    .contains(&wanted)
            })
            .map(|raw| raw.saturating_sub(8))
    }

    /// Forget keys and modifiers which may have been released while input
    /// delivery was suspended, while preserving the user's active layout.
    pub(super) fn reset_state(&mut self) {
        let active_layout = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        self.state = xkb::State::new(&self.keymap);
        let _ = self.state.update_mask(0, 0, 0, 0, 0, active_layout);
        self.mods = ModifiersState::empty();
    }

    /// Produce the same repeated `KeyboardInput` winit emits without
    /// mutating xkb's down/up state a second time.
    pub(super) fn repeat_event(&self, keycode: xkb::Keycode) -> WindowEvent {
        let sym = self.state.key_get_one_sym(keycode);
        let logical = keysym_to_winit_key(sym.raw());
        let mut input = keyboard_input_event(
            logical,
            text_for(sym.key_char(), self.current_mods()),
            true,
            keycode.raw().saturating_sub(8),
        );
        if let WindowEvent::KeyboardInput { event, .. } = &mut input {
            event.repeat = true;
        }
        input
    }

    /// Read the current effective Ctrl/Shift/Alt/Super state as a winit `ModifiersState`.
    pub(super) fn current_mods(&self) -> ModifiersState {
        let eff = xkb::STATE_MODS_EFFECTIVE;
        let mut m = ModifiersState::empty();
        if self.state.mod_name_is_active(xkb::MOD_NAME_CTRL, eff) {
            m |= ModifiersState::CONTROL;
        }
        if self.state.mod_name_is_active(xkb::MOD_NAME_SHIFT, eff) {
            m |= ModifiersState::SHIFT;
        }
        if self.state.mod_name_is_active(xkb::MOD_NAME_ALT, eff) {
            m |= ModifiersState::ALT;
        }
        if self.state.mod_name_is_active(xkb::MOD_NAME_LOGO, eff) {
            m |= ModifiersState::SUPER;
        }
        m
    }

    /// The events evdev key `keycode` going down or up makes on this
    /// keyboard: the modifier change it causes, then the key itself.
    pub(crate) fn translate(&mut self, keycode: u32, pressed: bool) -> Vec<BackendEvent> {
        let outcome = self.process(evdev_to_xkb(keycode), pressed);
        outcome
            .modifiers_changed
            .map(BackendEvent::ModifiersChanged)
            .into_iter()
            .chain(outcome.input.map(BackendEvent::Input))
            .collect()
    }

    /// Translate one key event. `keycode` already carries the +8 offset (see [`smithay::backend::input::KeyboardKeyEvent::key_code`]).
    pub(super) fn process(&mut self, keycode: xkb::Keycode, pressed: bool) -> KeyOutcome {
        let direction = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        let _ = self.state.update_key(keycode, direction);

        // Resolve the symbol and UTF-8 text *after* xkb has incorporated the
        // event. That is required for the key which changes the state itself:
        // a Shift press must publish Shift, not the symbol from the previous
        // level, and a printable press must use every modifier/layout latch
        // effective for that key. Releases are not text-bearing in the
        // consumers below, so resolving their post-release level is harmless.
        let sym = self.state.key_get_one_sym(keycode);

        let new_mods = self.current_mods();
        let modifiers_changed = if new_mods == self.mods {
            None
        } else {
            self.mods = new_mods;
            Some(Modifiers::from(new_mods))
        };

        // Derive the logical key from the *keysym* (not UTF-8 output) so chords like Ctrl+K resolve to `Character("k")`, not the control character.
        let logical = keysym_to_winit_key(sym.raw());
        let text = text_for(sym.key_char(), new_mods);
        let input = Some(keyboard_input_event(
            logical,
            text,
            pressed,
            keycode.raw().saturating_sub(8),
        ));

        KeyOutcome {
            input,
            modifiers_changed,
        }
    }
}

/// Map an xkb keysym to a winit logical [`Key`]: special keys become
/// `Key::Named`, printable Unicode becomes `Key::Character`; bare modifiers and unmapped keys return `None` (carried by `ModifiersChanged` instead).
pub(super) fn keysym_to_winit_key(raw: u32) -> Key {
    // Keep the function row intact. These are not printable keysyms, so they
    // must be named explicitly rather than falling through to `key_char()`.
    let function = match raw {
        keysyms::KEY_F1 => Some(NamedKey::F1),
        keysyms::KEY_F2 => Some(NamedKey::F2),
        keysyms::KEY_F3 => Some(NamedKey::F3),
        keysyms::KEY_F4 => Some(NamedKey::F4),
        keysyms::KEY_F5 => Some(NamedKey::F5),
        keysyms::KEY_F6 => Some(NamedKey::F6),
        keysyms::KEY_F7 => Some(NamedKey::F7),
        keysyms::KEY_F8 => Some(NamedKey::F8),
        keysyms::KEY_F9 => Some(NamedKey::F9),
        keysyms::KEY_F10 => Some(NamedKey::F10),
        keysyms::KEY_F11 => Some(NamedKey::F11),
        keysyms::KEY_F12 => Some(NamedKey::F12),
        keysyms::KEY_F13 => Some(NamedKey::F13),
        keysyms::KEY_F14 => Some(NamedKey::F14),
        keysyms::KEY_F15 => Some(NamedKey::F15),
        keysyms::KEY_F16 => Some(NamedKey::F16),
        keysyms::KEY_F17 => Some(NamedKey::F17),
        keysyms::KEY_F18 => Some(NamedKey::F18),
        keysyms::KEY_F19 => Some(NamedKey::F19),
        keysyms::KEY_F20 => Some(NamedKey::F20),
        keysyms::KEY_F21 => Some(NamedKey::F21),
        keysyms::KEY_F22 => Some(NamedKey::F22),
        keysyms::KEY_F23 => Some(NamedKey::F23),
        keysyms::KEY_F24 => Some(NamedKey::F24),
        _ => None,
    };
    if let Some(function) = function {
        return Key::Named(function);
    }
    let named = match raw {
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(NamedKey::Enter),
        keysyms::KEY_Escape => Some(NamedKey::Escape),
        keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => Some(NamedKey::Tab),
        keysyms::KEY_BackSpace => Some(NamedKey::Backspace),
        keysyms::KEY_Delete | keysyms::KEY_KP_Delete => Some(NamedKey::Delete),
        keysyms::KEY_Left | keysyms::KEY_KP_Left => Some(NamedKey::ArrowLeft),
        keysyms::KEY_Right | keysyms::KEY_KP_Right => Some(NamedKey::ArrowRight),
        keysyms::KEY_Up | keysyms::KEY_KP_Up => Some(NamedKey::ArrowUp),
        keysyms::KEY_Down | keysyms::KEY_KP_Down => Some(NamedKey::ArrowDown),
        keysyms::KEY_Home | keysyms::KEY_KP_Home => Some(NamedKey::Home),
        keysyms::KEY_End | keysyms::KEY_KP_End => Some(NamedKey::End),
        keysyms::KEY_Page_Up | keysyms::KEY_KP_Page_Up => Some(NamedKey::PageUp),
        keysyms::KEY_Page_Down | keysyms::KEY_KP_Page_Down => Some(NamedKey::PageDown),
        keysyms::KEY_space => Some(NamedKey::Space),
        keysyms::KEY_Caps_Lock => Some(NamedKey::CapsLock),
        keysyms::KEY_Num_Lock => Some(NamedKey::NumLock),
        keysyms::KEY_Scroll_Lock => Some(NamedKey::ScrollLock),
        keysyms::KEY_Insert => Some(NamedKey::Insert),
        keysyms::KEY_Menu => Some(NamedKey::ContextMenu),
        keysyms::KEY_Multi_key => Some(NamedKey::Compose),
        keysyms::KEY_ISO_Level3_Shift => Some(NamedKey::AltGraph),
        keysyms::KEY_ISO_Next_Group => Some(NamedKey::GroupNext),
        keysyms::KEY_ISO_Prev_Group => Some(NamedKey::GroupPrevious),
        keysyms::KEY_Pause => Some(NamedKey::Pause),
        keysyms::KEY_Help => Some(NamedKey::Help),
        // The stock `pc` keymap turns Alt+Print into Sys_Req; it is still
        // the Print Screen key, and Alt+Print is the focused-window capture.
        keysyms::KEY_Print | keysyms::KEY_Sys_Req => Some(NamedKey::PrintScreen),
        keysyms::KEY_Shift_L | keysyms::KEY_Shift_R => Some(NamedKey::Shift),
        keysyms::KEY_Control_L | keysyms::KEY_Control_R => Some(NamedKey::Control),
        keysyms::KEY_Alt_L | keysyms::KEY_Alt_R => Some(NamedKey::Alt),
        keysyms::KEY_Super_L | keysyms::KEY_Super_R => Some(NamedKey::Super),
        // The function row. Without these, media/brightness keys reach `key_char()` (no character for them) and the press is dropped before any handler sees it — why volume-up did nothing on real hardware.
        keysyms::KEY_XF86AudioRaiseVolume => Some(NamedKey::AudioVolumeUp),
        keysyms::KEY_XF86AudioLowerVolume => Some(NamedKey::AudioVolumeDown),
        keysyms::KEY_XF86AudioMute => Some(NamedKey::AudioVolumeMute),
        keysyms::KEY_XF86AudioMicMute => Some(NamedKey::MicrophoneVolumeMute),
        keysyms::KEY_XF86MonBrightnessUp => Some(NamedKey::BrightnessUp),
        keysyms::KEY_XF86MonBrightnessDown => Some(NamedKey::BrightnessDown),
        keysyms::KEY_XF86AudioPlay => Some(NamedKey::MediaPlayPause),
        keysyms::KEY_XF86AudioPause => Some(NamedKey::MediaPause),
        keysyms::KEY_XF86AudioNext => Some(NamedKey::MediaTrackNext),
        keysyms::KEY_XF86AudioPrev => Some(NamedKey::MediaTrackPrevious),
        keysyms::KEY_XF86PowerOff => Some(NamedKey::Power),
        keysyms::KEY_XF86AudioStop => Some(NamedKey::MediaStop),
        // winit's own XKB table folds every sleep spelling into Standby and
        // names the calculator key LaunchApplication2; the nested backend
        // delivers those, so this one must too.
        keysyms::KEY_XF86Sleep | keysyms::KEY_XF86Suspend | keysyms::KEY_XF86Standby => {
            Some(NamedKey::Standby)
        }
        keysyms::KEY_XF86Calculator => Some(NamedKey::LaunchApplication2),
        _ => None,
    };
    if let Some(n) = named {
        return Key::Named(n);
    }
    // Printable character (handles layout + shift + caps via the keysym).
    if let Some(c) = Keysym::new(raw).key_char()
        && !c.is_control()
    {
        return Key::Character(SmolStr::new(c.to_string()));
    }
    // Never drop an otherwise valid physical key merely because this adapter
    // does not have a named-key spelling for its keysym. The seat still must
    // receive the raw keycode (IME/vendor keys included), and clients can
    // interpret the xkb keymap they were sent. Returning an unidentified
    // logical key preserves that protocol event while keeping it out of shell
    // chord matching.
    Key::Unidentified(NativeKey::Xkb(raw))
}

/// The typed text for a key event, or `None`. Suppressed under Ctrl/Super (chords, not text) and for control characters — matching winit, keeping control bytes out of text inputs.
pub(super) fn text_for(ch: Option<char>, mods: ModifiersState) -> Option<SmolStr> {
    if mods.control_key() || mods.super_key() {
        return None;
    }
    match ch {
        Some(c) if !c.is_control() => Some(SmolStr::new(c.to_string())),
        _ => None,
    }
}

/// Build a `WindowEvent::KeyboardInput` from the evdev key `evdev_keycode`.
pub(super) fn keyboard_input_event(
    logical_key: Key,
    text: Option<SmolStr>,
    pressed: bool,
    evdev_keycode: u32,
) -> WindowEvent {
    let mut ev = ui::synthetic_input::key_event(
        logical_key,
        text,
        if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
    );
    ev.physical_key = PhysicalKey::from_scancode(evdev_keycode);
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: ev,
        is_synthetic: false,
    }
}
