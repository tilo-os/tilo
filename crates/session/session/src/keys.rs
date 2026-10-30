//! The keys the session answers wherever the keyboard is: the desktop's
//! chords and the hardware keys along the top of the keyboard.
//!
//! The compositor offers this surface every press before the focused
//! window or panel gets it ([`SurfaceHost::claim_key`]). A press that is a
//! chord of the desktop's one shortcut table ([`tilo_desktop::shortcuts`])
//! or a hardware key (volume, brightness, airplane mode, the touchpad,
//! playback, Print) is taken, and becomes an invocation of a published
//! action: the chord's `desktop:chrome/shortcut`, quick settings' `volume-up`, the
//! media service's `play-pause`. The session runs those through the same
//! door every other caller uses; nothing here performs anything itself.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::ActionAddress;
use tilo_compositor::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_desktop::desktop_actions::CHROME_RESOURCE;
use tilo_desktop::osd::OsdKind;
use ui::shortcut_table::{ShortcutAction, ShortcutConfig};
use xkeysym::key as keysyms;

/// One published action to invoke, with its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The readout to show once it ran (a hardware key's answer on screen).
    pub readout: Option<OsdKind>,
}

impl Invocation {
    /// `action` of `resource`, without arguments.
    #[must_use]
    pub fn new(resource: &str, action: &str) -> Self {
        Self {
            address: ActionAddress::new(resource, action),
            arguments: serde_json::Map::new(),
            readout: None,
        }
    }

    /// The same, showing the readout `kind` once it ran.
    #[must_use]
    pub const fn showing(mut self, kind: OsdKind) -> Self {
        self.readout = Some(kind);
        self
    }

    /// The same, with `name` set to `value`.
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        self.arguments.insert(name.to_owned(), value.into());
        self
    }

    /// The chrome's shortcut action for `action`.
    #[must_use]
    pub fn shortcut(action: ShortcutAction) -> Self {
        let id = serde_json::to_value(action)
            .ok()
            .and_then(|id| id.as_str().map(str::to_owned))
            .unwrap_or_default();
        Self::new(CHROME_RESOURCE, "shortcut").with("action", id)
    }
}

/// Invocations waiting for the session's door.
pub type Invocations = Rc<RefCell<Vec<Invocation>>>;

/// Quick settings' resource, whose effects the level and radio keys run.
pub const QUICK_SETTINGS: &str = "quick-settings.panel";
/// What plays: the audio service's media, whose own controls reach the
/// player in front.
pub const MEDIA: &str = tilo_audio_service::media::MEDIA;
/// The microphone: the audio service's sound resource.
pub const MICROPHONE: &str = tilo_audio_service::sound::SOUND;
/// This device's input devices, the session's own.
const TOUCHPAD: &str = crate::controls::INPUT_RESOURCE;
/// The keyboard's backlight, the system service's.
pub const KEYBOARD_BACKLIGHT: &str = tilo_system_service::KEYBOARD_BACKLIGHT;
/// The display modes (mirror, extend, …), the session's own.
const DISPLAYS: &str = crate::controls::DISPLAYS_RESOURCE;
/// The machine's power policy, the power service's: what the power key does.
pub const SLEEP: &str = tilo_session_power::SLEEP;
/// The capture studio, whose `screenshot` action the Print key is.
const CAPTURE_STUDIO: &str = tilo_capture_studio::RESOURCE;

/// `XF86XK_RFKill`, which the keysym tables predate: laptops send it for
/// the airplane-mode key; `XF86WLAN` is the older spelling.
const KEY_XF86_RFKILL: u32 = 0x1008_ffb5;

/// A hardware key: the action it runs, whether holding it repeats it, and
/// whether it works on the lock screen (it changes a level and reveals
/// nothing behind the lock).
struct Hardware {
    resource: &'static str,
    action: &'static str,
    repeats: bool,
    while_locked: bool,
    /// The readout that answers it on screen.
    readout: Option<OsdKind>,
}

const fn hardware(
    resource: &'static str,
    action: &'static str,
    repeats: bool,
    while_locked: bool,
) -> Hardware {
    Hardware {
        resource,
        action,
        repeats,
        while_locked,
        readout: None,
    }
}

impl Hardware {
    const fn reads(mut self, kind: OsdKind) -> Self {
        self.readout = Some(kind);
        self
    }
}

/// The hardware key `keysym` is, if it is one.
fn classify(keysym: u32) -> Option<Hardware> {
    Some(match keysym {
        keysyms::XF86_AudioRaiseVolume => {
            hardware(QUICK_SETTINGS, "volume-up", true, true).reads(OsdKind::Volume)
        }
        keysyms::XF86_AudioLowerVolume => {
            hardware(QUICK_SETTINGS, "volume-down", true, true).reads(OsdKind::Volume)
        }
        keysyms::XF86_AudioMute => {
            hardware(QUICK_SETTINGS, "toggle-muted", false, true).reads(OsdKind::Volume)
        }
        keysyms::XF86_MonBrightnessUp => {
            hardware(QUICK_SETTINGS, "brightness-up", true, true).reads(OsdKind::Brightness)
        }
        keysyms::XF86_MonBrightnessDown => {
            hardware(QUICK_SETTINGS, "brightness-down", true, true).reads(OsdKind::Brightness)
        }
        keysyms::XF86_WLAN | KEY_XF86_RFKILL => {
            hardware(QUICK_SETTINGS, "toggle-airplane-mode", false, false)
                .reads(OsdKind::AirplaneMode)
        }
        keysyms::XF86_AudioMicMute => {
            hardware(MICROPHONE, "toggle-input-muted", false, true).reads(OsdKind::Microphone)
        }
        keysyms::XF86_AudioPlay | keysyms::XF86_AudioPause => {
            hardware(MEDIA, "play-pause", false, true).reads(OsdKind::Play)
        }
        keysyms::XF86_AudioNext => hardware(MEDIA, "next", false, true).reads(OsdKind::NextTrack),
        keysyms::XF86_AudioPrev => {
            hardware(MEDIA, "previous", false, true).reads(OsdKind::PreviousTrack)
        }
        keysyms::XF86_AudioStop => hardware(MEDIA, "stop", false, true).reads(OsdKind::Stop),
        keysyms::XF86_TouchpadToggle => hardware(TOUCHPAD, "toggle-touchpad", false, false),
        keysyms::XF86_KbdBrightnessUp => {
            hardware(KEYBOARD_BACKLIGHT, "brighter", true, true).reads(OsdKind::KeyboardBacklight)
        }
        keysyms::XF86_KbdBrightnessDown => {
            hardware(KEYBOARD_BACKLIGHT, "dimmer", true, true).reads(OsdKind::KeyboardBacklight)
        }
        keysyms::XF86_KbdLightOnOff => {
            hardware(KEYBOARD_BACKLIGHT, "toggle", false, true).reads(OsdKind::KeyboardBacklight)
        }
        keysyms::XF86_Display => hardware(DISPLAYS, "next-mode", false, false),
        // The power key works on the lock screen too: a press there that the
        // policy resolves to lock turns the display off instead.
        keysyms::XF86_PowerOff => hardware(SLEEP, "power-key", false, true),
        // The Print key takes the whole screen, as the studio's own bar
        // does; the lock screen shows no desktop to take.
        keysyms::Print => hardware(CAPTURE_STUDIO, "screenshot", false, false),
        _ => return None,
    })
}

/// The surface that takes the session's keys; it shows nothing.
pub(crate) struct SessionKeys {
    shortcuts: ShortcutConfig,
    invocations: Invocations,
    /// The apps' shortcuts, which work wherever the keyboard is.
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    /// The shortcut resources whose chords are held down, by key.
    held: RefCell<BTreeMap<u32, String>>,
}

impl SessionKeys {
    pub(crate) fn new(
        shortcuts: ShortcutConfig,
        invocations: Invocations,
        portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    ) -> Self {
        Self {
            shortcuts,
            invocations,
            portal,
            held: RefCell::new(BTreeMap::new()),
        }
    }

    /// The app shortcut a press of `key` is, as its published resource.
    fn portal_shortcut(&self, key: &HostKey) -> Option<String> {
        let portal = self.portal.as_ref()?;
        portal.triggered(
            key.keycode,
            tilo_portal::trigger::Modifiers {
                ctrl: key.modifiers.ctrl,
                alt: key.modifiers.alt,
                shift: key.modifiers.shift,
                logo: key.modifiers.logo,
            },
        )
    }

    /// The invocation a press is, and how it is claimed.
    fn resolve(&self, key: &HostKey, locked: bool) -> Option<(Invocation, KeyClaim)> {
        if let Some(hardware) = classify(key.keysym) {
            if locked && !hardware.while_locked {
                return None;
            }
            let claim = if hardware.repeats {
                KeyClaim::TakeRepeating
            } else {
                KeyClaim::Take
            };
            let mut invocation = Invocation::new(hardware.resource, hardware.action);
            invocation.readout = hardware.readout;
            return Some((invocation, claim));
        }
        if locked {
            return None;
        }
        // An app's bound shortcut is the person's answer to its sheet: it
        // is pressed like the desktop's own chords are.
        if let Some(resource) = self.portal_shortcut(key) {
            return Some((Invocation::new(&resource, "activate"), KeyClaim::Take));
        }
        let (logical, modifiers) = tilo_surface_host::toolkit_key(key);
        let action = tilo_desktop::shortcuts::configured_shortcut_action(
            &self.shortcuts,
            &logical,
            modifiers,
        )?;
        Some((Invocation::shortcut(action), KeyClaim::Take))
    }
}

impl SurfaceHost for SessionKeys {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn claim_key(&mut self, key: &HostKey, locked: bool) -> KeyClaim {
        self.resolve(key, locked)
            .map_or(KeyClaim::Pass, |(_, claim)| claim)
    }

    fn key(&mut self, key: &HostKey) {
        if !key.pressed {
            // The app hears its shortcut released wherever the modifiers
            // are by then: the chord is remembered by the key that held it.
            if let Some(resource) = self.held.borrow_mut().remove(&key.keycode) {
                self.invocations
                    .borrow_mut()
                    .push(Invocation::new(&resource, "deactivate"));
            }
            return;
        }
        // A repeat of a key taken once is not run again (only a level key
        // repeats); the compositor repeats only keys claimed repeating.
        if let Some(resource) = self.portal_shortcut(key) {
            self.held.borrow_mut().insert(key.keycode, resource.clone());
            self.invocations
                .borrow_mut()
                .push(Invocation::new(&resource, "activate"));
            return;
        }
        if let Some((invocation, _)) = self.resolve(key, false) {
            self.invocations.borrow_mut().push(invocation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_compositor::KeyModifiers;

    fn key(keysym: u32, text: Option<char>, modifiers: KeyModifiers) -> HostKey {
        HostKey {
            keycode: 0,
            pressed: true,
            repeat: false,
            keysym,
            unmodified_keysym: keysym,
            text,
            modifiers,
        }
    }

    fn keys() -> SessionKeys {
        SessionKeys::new(ShortcutConfig::default(), Invocations::default(), None)
    }

    #[test]
    fn level_keys_repeat_and_work_locked_and_the_rest_wait_for_the_unlock() {
        let keys = keys();
        let volume = key(
            keysyms::XF86_AudioRaiseVolume,
            None,
            KeyModifiers::default(),
        );
        assert_eq!(
            keys.resolve(&volume, true),
            Some((
                Invocation::new(QUICK_SETTINGS, "volume-up").showing(OsdKind::Volume),
                KeyClaim::TakeRepeating
            ))
        );
        let airplane = key(KEY_XF86_RFKILL, None, KeyModifiers::default());
        assert_eq!(keys.resolve(&airplane, true), None);
        assert_eq!(
            keys.resolve(&airplane, false),
            Some((
                Invocation::new(QUICK_SETTINGS, "toggle-airplane-mode")
                    .showing(OsdKind::AirplaneMode),
                KeyClaim::Take
            ))
        );
        let play = key(keysyms::XF86_AudioPlay, None, KeyModifiers::default());
        assert_eq!(
            keys.resolve(&play, true).map(|(invocation, _)| invocation),
            Some(Invocation::new(MEDIA, "play-pause").showing(OsdKind::Play))
        );
    }

    #[test]
    fn the_print_key_takes_a_screenshot_unless_the_screen_is_locked() {
        let keys = keys();
        let print = key(keysyms::Print, None, KeyModifiers::default());
        assert_eq!(
            keys.resolve(&print, false),
            Some((
                Invocation::new(CAPTURE_STUDIO, "screenshot"),
                KeyClaim::Take
            ))
        );
        assert_eq!(keys.resolve(&print, true), None);
    }

    #[test]
    fn chords_of_the_shortcut_table_become_the_chromes_shortcut_action() {
        let keys = keys();
        let logo = KeyModifiers {
            logo: true,
            ..KeyModifiers::default()
        };
        let tab = key(keysyms::Tab, None, logo);
        assert_eq!(
            keys.resolve(&tab, false),
            Some((
                Invocation::shortcut(ShortcutAction::WorkspaceOverview),
                KeyClaim::Take
            ))
        );
        assert_eq!(
            Invocation::shortcut(ShortcutAction::WorkspaceOverview).arguments["action"],
            "workspace_overview"
        );
        // Chords wait while the session is locked; plain typing is never one.
        assert_eq!(keys.resolve(&tab, true), None);
        let a = key(keysyms::a, Some('a'), KeyModifiers::default());
        assert_eq!(keys.resolve(&a, false), None);
    }
}
