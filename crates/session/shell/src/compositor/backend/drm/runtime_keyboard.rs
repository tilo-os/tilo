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
    /// Dead keys and the Compose key, from the session locale's compose
    /// table, so `´` then `e` types `é` in the shell's own fields as it does
    /// in apps. `None` when no table loads: keys then type as they are.
    compose: Option<xkb::compose::State>,
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
        let compose = locale_compose_table(&context)
            .map(|table| xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS));
        Some(Self {
            _context: context,
            keymap,
            state,
            mods: ModifiersState::empty(),
            compiled: Self::keymap_names(settings),
            compose,
        })
    }

    /// What a press types once dead keys and the Compose key have had their
    /// say: nothing while a sequence is open or when it is abandoned, the
    /// composed text when it completes, and the key's own text otherwise.
    fn composed_text(&mut self, sym: Keysym, typed: Option<SmolStr>) -> Option<SmolStr> {
        use xkb::compose::{FeedResult, Status};
        let Some(compose) = self.compose.as_mut() else {
            return typed;
        };
        if compose.feed(sym) == FeedResult::Ignored {
            return typed;
        }
        match compose.status() {
            Status::Nothing => typed,
            Status::Composing => None,
            Status::Composed => {
                let text = compose
                    .utf8()
                    .filter(|text| !text.is_empty())
                    .map(SmolStr::from)
                    .or_else(|| {
                        compose
                            .keysym()
                            .and_then(Keysym::key_char)
                            .map(|ch| SmolStr::new(ch.encode_utf8(&mut [0; 4])))
                    });
                compose.reset();
                text
            }
            Status::Cancelled => {
                compose.reset();
                None
            }
        }
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

    /// The logical key a press names. Under Ctrl, Super or Alt, a letter
    /// from a non-Latin source (Russian, Greek, Hebrew, Arabic, ...) is named
    /// by the Latin character another configured source puts on that key, so
    /// Super+L locks and Ctrl+C copies whichever source is typing. Latin
    /// letters with diacritics stay as typed: they are shortcuts of their own
    /// on the layouts that have them.
    fn chord_logical_key(&self, keycode: xkb::Keycode, sym: Keysym, mods: ModifiersState) -> Key {
        let logical = keysym_to_winit_key(sym.raw());
        if !(mods.control_key() || mods.super_key() || mods.alt_key()) {
            return logical;
        }
        let Key::Character(typed) = &logical else {
            return logical;
        };
        if !typed.chars().any(ui::latin_chord::is_non_latin_letter) {
            return logical;
        }
        let active = self.state.key_get_layout(keycode);
        (0..self.keymap.num_layouts_for_key(keycode))
            .filter(|layout| *layout != active)
            .find_map(|layout| {
                self.keymap
                    .key_get_syms_by_level(keycode, layout, 0)
                    .first()
                    .and_then(|latin| latin.key_char())
                    .filter(char::is_ascii_graphic)
            })
            .map_or(logical, |latin| {
                Key::Character(SmolStr::new(latin.encode_utf8(&mut [0; 4])))
            })
    }

    /// Forget keys and modifiers which may have been released while input
    /// delivery was suspended, while preserving the user's active layout.
    pub(super) fn reset_state(&mut self) {
        let active_layout = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        self.state = xkb::State::new(&self.keymap);
        let _ = self.state.update_mask(0, 0, 0, 0, 0, active_layout);
        self.mods = ModifiersState::empty();
        if let Some(compose) = self.compose.as_mut() {
            compose.reset();
        }
    }

    /// Produce the same repeated `KeyboardInput` winit emits without
    /// mutating xkb's down/up state a second time.
    pub(super) fn repeat_event(&self, keycode: xkb::Keycode) -> WindowEvent {
        let sym = self.state.key_get_one_sym(keycode);
        let logical = self.chord_logical_key(keycode, sym, self.current_mods());
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
        let logical = self.chord_logical_key(keycode, sym, new_mods);
        let mut text = text_for(sym.key_char(), new_mods);
        if pressed && !(new_mods.control_key() || new_mods.super_key()) {
            text = self.composed_text(sym, text);
        }
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

/// The session locale's compose table: the first of `LC_ALL`, `LC_CTYPE`
/// and `LANG` that is set, as libc resolves it. libxkbcommon reads the C
/// locale as en_US.UTF-8.
fn locale_compose_table(context: &xkb::Context) -> Option<xkb::compose::Table> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C".into());
    xkb::compose::Table::new_from_locale(context, &locale, xkb::compose::COMPILE_NO_FLAGS)
        .inspect_err(|()| {
            tracing::warn!(
                ?locale,
                "DRM input: no compose table for the locale; dead keys type nothing in the shell"
            );
        })
        .ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(events: Vec<BackendEvent>) -> winit::event::KeyEvent {
        events
            .into_iter()
            .find_map(|event| match event {
                BackendEvent::Input(WindowEvent::KeyboardInput { event, .. }) => Some(event),
                _ => None,
            })
            .expect("a key event")
    }

    #[test]
    fn a_chord_typed_on_a_russian_source_names_its_latin_key() {
        let settings = KeyboardSettings {
            model: "pc105".into(),
            source_ids: vec!["us".into(), "ru".into()],
            layouts: "us,ru".into(),
            variants: ",".into(),
            options: None,
            active_index: 1,
        };
        let mut keyboard =
            KeyboardXkb::from_settings(&settings).expect("the us,ru keymap compiles");
        // Unmodified, the key a US keyboard labels L types the Russian letter.
        let typed = pressed(keyboard.translate(38, true));
        assert_eq!(typed.logical_key, Key::Character("д".into()));
        assert_eq!(typed.text.as_deref(), Some("д"));
        let _ = keyboard.translate(38, false);

        // Under Super it is the lock chord, as on the US source.
        let _ = keyboard.translate(125, true);
        let chord = pressed(keyboard.translate(38, true));
        assert_eq!(chord.logical_key, Key::Character("l".into()));
        let shortcuts = settings_store::hardware::ShortcutConfig::default();
        assert_eq!(
            tilo_desktop::shortcuts::configured_shortcut_action(
                &shortcuts,
                &chord.logical_key,
                Modifiers::from(ModifiersState::SUPER),
            ),
            Some(settings_store::hardware::ShortcutAction::Lock)
        );
        let WindowEvent::KeyboardInput { event, .. } =
            keyboard.repeat_event(xkb::Keycode::new(38 + 8))
        else {
            panic!("a repeat is a key event")
        };
        assert_eq!(event.logical_key, Key::Character("l".into()));
        let _ = keyboard.translate(38, false);
        let _ = keyboard.translate(125, false);

        // Ctrl+C copies on the Russian source too.
        let _ = keyboard.translate(29, true);
        let copy = pressed(keyboard.translate(46, true));
        assert_eq!(copy.logical_key, Key::Character("c".into()));
    }

    #[test]
    fn a_dead_key_then_a_letter_types_the_accented_letter() {
        let settings = KeyboardSettings {
            model: "pc105".into(),
            source_ids: vec!["de".into()],
            layouts: "de".into(),
            variants: String::new(),
            options: None,
            active_index: 0,
        };
        let mut keyboard = KeyboardXkb::from_settings(&settings).expect("the de keymap compiles");
        // The table this test types with, so it does not depend on the
        // build machine's locale files.
        let table = xkb::compose::Table::new_from_buffer(
            &xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            "<dead_acute> <e> : \"é\" eacute\n<Multi_key> <a> <e> : \"æ\" ae\n",
            "C",
            xkb::compose::FORMAT_TEXT_V1,
            xkb::compose::COMPILE_NO_FLAGS,
        )
        .expect("the compose table parses");
        keyboard.compose = Some(xkb::compose::State::new(
            &table,
            xkb::compose::STATE_NO_FLAGS,
        ));
        let mut tap = |code: u32| {
            let text = keyboard
                .translate(code, true)
                .into_iter()
                .find_map(|event| match event {
                    BackendEvent::Input(WindowEvent::KeyboardInput { event, .. }) => Some(event),
                    _ => None,
                })
                .expect("a key event")
                .text
                .map(|text| text.to_string());
            let _ = keyboard.translate(code, false);
            text
        };
        // The German key right of ß is the acute dead key (evdev 13).
        assert_eq!(tap(13), None, "the dead key alone types nothing yet");
        assert_eq!(tap(18).as_deref(), Some("é"));
        assert_eq!(tap(18).as_deref(), Some("e"), "the next e is plain again");
    }

    #[test]
    fn a_chord_on_a_latin_letter_with_a_diacritic_keeps_that_letter() {
        let settings = KeyboardSettings {
            model: "pc105".into(),
            source_ids: vec!["us".into(), "de:nodeadkeys".into()],
            layouts: "us,de".into(),
            variants: ",nodeadkeys".into(),
            options: None,
            active_index: 1,
        };
        let mut keyboard =
            KeyboardXkb::from_settings(&settings).expect("the us,de keymap compiles");
        // Ctrl and the German Ö key (US semicolon, evdev 39) stays Ö.
        let _ = keyboard.translate(29, true);
        let chord = pressed(keyboard.translate(39, true));
        assert_eq!(chord.logical_key, Key::Character("ö".into()));
    }
}
