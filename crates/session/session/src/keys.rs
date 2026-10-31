//! The keys the session answers wherever the keyboard is: the desktop's
//! chords and the hardware keys along the top of the keyboard.
//!
//! The compositor offers this surface every press before the focused
//! window or panel gets it ([`SurfaceHost::claim_key`]). A press that is a
//! chord of the desktop's one shortcut table ([`tilo_desktop::shortcuts`])
//! or a hardware key (volume, brightness, airplane mode, the touchpad,
//! playback, Print, F1) is taken, and becomes an invocation of a published
//! action: the chord's `desktop:chrome/shortcut`, quick settings' `volume-up`, the
//! media service's `play-pause`. The session runs those through the same
//! door every other caller uses; nothing here performs anything itself.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::ActionAddress;
use tilo_compositor::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_desktop::desk_widgets::DeskFold;
use tilo_desktop::desktop_actions::CHROME_RESOURCE;
use tilo_desktop::osd::OsdKind;
use ui::shortcut_table::{KeyChord, ShortcutAction, ShortcutConfig};
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
    action: HardwareAction,
    repeats: bool,
    while_locked: bool,
    /// The readout that answers it on screen.
    readout: Option<OsdKind>,
}

/// What a hardware key runs: an action of the session's resources, or one of
/// the desktop's own shortcuts, which the chrome carries out as its chord
/// would.
#[derive(Clone, Copy)]
enum HardwareAction {
    Resource {
        resource: &'static str,
        action: &'static str,
    },
    Shortcut(ShortcutAction),
}

const fn hardware(
    resource: &'static str,
    action: &'static str,
    repeats: bool,
    while_locked: bool,
) -> Hardware {
    Hardware {
        action: HardwareAction::Resource { resource, action },
        repeats,
        while_locked,
        readout: None,
    }
}

/// A key that asks for `action` of the desktop's shortcut table, as the
/// chord bound to it does.
const fn desktop(action: ShortcutAction, repeats: bool, while_locked: bool) -> Hardware {
    Hardware {
        action: HardwareAction::Shortcut(action),
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
        // F1 names every key the desktop answers. It is the desktop's own
        // key, which nothing may be bound to, so the guide opens whatever
        // modifiers are held with it.
        keysyms::F1 => desktop(ShortcutAction::ShortcutHelp, false, false),
        _ => return None,
    })
}

/// What the shortcut guide took from the person since it last looked.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Caught {
    /// Escape: the guide waits for no chord any more, and what the last
    /// press was refused for is no longer the person's concern.
    Cancelled,
    /// The chord the person pressed for the shortcut that waited for one.
    Chord(KeyChord),
}

/// What the shortcut guide and the session's keys share while the guide is
/// up: that it is, which shortcut waits for a new chord, and the chords
/// pressed for it since the guide last looked.
///
/// It is the key surface's answer to the guide being modal: the surface
/// sees every press before it acts anywhere, so it is the one place that
/// can hold one back and hand it to the guide instead.
#[derive(Clone, Default)]
pub(crate) struct GuideKeys {
    inner: Rc<GuideKeysInner>,
}

#[derive(Default)]
struct GuideKeysInner {
    up: Cell<bool>,
    listening: RefCell<Option<String>>,
    caught: RefCell<Vec<Caught>>,
}

impl GuideKeys {
    /// The guide is up, or has gone: a guide that is gone waits for no
    /// chord and has nothing left to answer.
    pub(crate) fn set_up(&self, up: bool) {
        self.inner.up.set(up);
        if !up {
            self.inner.listening.replace(None);
            self.inner.caught.borrow_mut().clear();
        }
    }

    /// Whether the guide is up.
    pub(crate) fn up(&self) -> bool {
        self.inner.up.get()
    }

    /// The shortcut whose new chord the next press is, while the guide waits
    /// for one.
    pub(crate) fn listening(&self) -> Option<String> {
        self.inner.listening.borrow().clone()
    }

    /// Wait for a new chord for `action`.
    pub(crate) fn listen_for(&self, action: &str) {
        self.inner.listening.replace(Some(action.to_owned()));
    }

    /// Stop waiting for a chord: the guide stays up and takes its keys for
    /// nothing but itself.
    pub(crate) fn stop_listening(&self) {
        self.inner.listening.replace(None);
    }

    /// Keep `caught` for the guide.
    pub(crate) fn catch(&self, caught: Caught) {
        self.inner.caught.borrow_mut().push(caught);
    }

    /// What was caught since the guide last looked, oldest first.
    pub(crate) fn take_caught(&self) -> Vec<Caught> {
        std::mem::take(&mut *self.inner.caught.borrow_mut())
    }
}

/// The surface that takes the session's keys; it shows nothing.
pub(crate) struct SessionKeys {
    /// The desktop's shortcut table, shared with the shortcut guide so the
    /// guide explains the chords the keys resolve against.
    shortcuts: Rc<RefCell<ShortcutConfig>>,
    /// The display sheet's presentation: Super+P puts it up while it is not
    /// there, and once it is, the chord moves inside it instead.
    displays: Option<tilo_sheets::Presentation>,
    invocations: Invocations,
    /// The apps' shortcuts, which work wherever the keyboard is.
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    /// The shortcut guide's keys while it is up: what it holds back from
    /// the desktop behind it, and the chord it waits for.
    guide: GuideKeys,
    /// Whether a desk widget has its card open, which Escape folds. The desk
    /// is under the windows and hears no keys itself, so it says here what
    /// its keys are worth.
    fold: DeskFold,
    /// The shortcut resources whose chords are held down, by key.
    held: RefCell<BTreeMap<u32, String>>,
    /// The controller the person last drove the desktop with, which a press
    /// of their own replaces: the guide then leads with the keyboard.
    controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed are a controller's own, standing for
    /// its presses rather than the person's fingers on a keyboard.
    controller_keys: Rc<Cell<bool>>,
}

impl SessionKeys {
    pub(crate) fn new(
        shortcuts: Rc<RefCell<ShortcutConfig>>,
        invocations: Invocations,
        portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
        guide: GuideKeys,
        fold: DeskFold,
        displays: Option<tilo_sheets::Presentation>,
        controller: Rc<Cell<Option<ui::ControllerFamily>>>,
        controller_keys: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            shortcuts,
            displays,
            invocations,
            portal,
            guide,
            fold,
            held: RefCell::new(BTreeMap::new()),
            controller,
            controller_keys,
        }
    }

    /// Whether `key` is the desk's own: Escape folds the card a desk widget
    /// has open, and what sits behind the desk does not hear it.
    fn folds_the_desk(&self, key: &HostKey) -> bool {
        key.unmodified_keysym == keysyms::Escape && self.fold.is_open()
    }

    /// Whether `key` is the display sheet's chord, which puts the sheet up:
    /// Super+P, as the desktop's fixed table names it. A sheet that is up is
    /// not opened again: what is drawn takes the chord and moves the
    /// highlight inside it.
    fn opens_the_display_sheet(&self, key: &HostKey) -> bool {
        let Some(displays) = &self.displays else {
            return false;
        };
        if displays.is_drawn() {
            return false;
        }
        let (logical, modifiers) = tilo_surface_host::toolkit_key(key);
        let ui::winit::keyboard::Key::Character(letter) = &logical else {
            return false;
        };
        let state = modifiers.state();
        letter.eq_ignore_ascii_case("p")
            && state.super_key()
            && !state.shift_key()
            && !state.control_key()
            && !state.alt_key()
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

    /// The desktop's shortcut `key` is bound to, as the table names it.
    fn configured_action(&self, key: &HostKey) -> Option<ShortcutAction> {
        let (logical, modifiers) = tilo_surface_host::toolkit_key(key);
        let shortcuts = self.shortcuts.borrow();
        tilo_desktop::shortcuts::configured_shortcut_action(&shortcuts, &logical, modifiers)
    }

    /// The invocation a press is, and how it is claimed. `inhibited` while a
    /// focused client holds the desktop's shortcuts (a virtual machine, a
    /// remote desktop): only a hardware key and locking the session stay the
    /// desktop's then.
    fn resolve(
        &self,
        key: &HostKey,
        locked: bool,
        inhibited: bool,
    ) -> Option<(Invocation, KeyClaim)> {
        if let Some(hardware) = classify(key.keysym) {
            if locked && !hardware.while_locked {
                return None;
            }
            let claim = if hardware.repeats {
                KeyClaim::TakeRepeating
            } else {
                KeyClaim::Take
            };
            let mut invocation = match hardware.action {
                HardwareAction::Resource { resource, action } => Invocation::new(resource, action),
                HardwareAction::Shortcut(action) => Invocation::shortcut(action),
            };
            invocation.readout = hardware.readout;
            return Some((invocation, claim));
        }
        if locked {
            return None;
        }
        if inhibited {
            // The client showing another machine answers its own chords;
            // locking this one is never its to keep, or it could hide the
            // way out of itself.
            let action = self.configured_action(key)?;
            return (action == ShortcutAction::Lock)
                .then(|| (Invocation::shortcut(action), KeyClaim::Take));
        }
        // Super+P is the display sheet's, as it is on every desktop: the
        // keys the fixed table names come before any app's chord.
        if self.opens_the_display_sheet(key) {
            return Some((Invocation::new(DISPLAYS, "open-sheet"), KeyClaim::Take));
        }
        // An app's bound shortcut is the person's answer to its sheet: it
        // is pressed like the desktop's own chords are.
        if let Some(resource) = self.portal_shortcut(key) {
            return Some((Invocation::new(&resource, "activate"), KeyClaim::Take));
        }
        let (logical, modifiers) = tilo_surface_host::toolkit_key(key);
        let shortcuts = self.shortcuts.borrow();
        let action =
            tilo_desktop::shortcuts::configured_shortcut_action(&shortcuts, &logical, modifiers)?;
        Some((Invocation::shortcut(action), KeyClaim::Take))
    }

    /// Whether `invocation` is the desktop's way to the shortcut guide, which
    /// stays its own key while the guide is up.
    fn shows_the_guide(invocation: &Invocation) -> bool {
        *invocation == Invocation::shortcut(ShortcutAction::ShortcutHelp)
    }

    /// A press the guide is up over: it answers only what is its own, and
    /// while it waits for a new chord, that press is the chord.
    fn guide_key(&self, key: &HostKey) {
        let resolved = self.resolve(key, false, false);
        // F1 and the desktop's help chord stay the guide's own way out,
        // waiting for a chord or not.
        if let Some((invocation, _)) = &resolved
            && Self::shows_the_guide(invocation)
        {
            self.invocations.borrow_mut().push(invocation.clone());
            return;
        }
        // A hardware key changes a level or takes the screen: it is no chord
        // of the desktop's table, so it answers as it does without the guide.
        if classify(key.keysym).is_some() {
            if let Some((invocation, _)) = resolved {
                self.invocations.borrow_mut().push(invocation);
            }
            return;
        }
        if self.guide.listening().is_none() || key.repeat {
            // The guide is modal for everything else: a chord acts once it
            // is gone.
            return;
        }
        if key.unmodified_keysym == keysyms::Escape {
            self.guide.stop_listening();
            self.guide.catch(Caught::Cancelled);
            return;
        }
        let (logical, modifiers) = tilo_surface_host::toolkit_key(key);
        // A modifier held on its own is no chord, and no modifier is either.
        if let Some(chord) = tilo_desktop::shortcuts::shortcut_chord_from_event(&logical, modifiers)
        {
            self.guide.catch(Caught::Chord(chord));
        }
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

    fn claim_key(&mut self, key: &HostKey, locked: bool, inhibited: bool) -> KeyClaim {
        if self.guide.up() {
            if self.guide.listening().is_some() {
                // The guide waits for a shortcut's new keys: every press is
                // that chord, and none of them reaches what is behind it.
                return KeyClaim::Take;
            }
            // The guide is modal: a chord of the desktop's table or an app's
            // shortcut must not act on what it hides. A hardware key (the
            // volume keys, F1) is no chord and answers as it does without
            // the guide.
            if classify(key.keysym).is_none() {
                return self
                    .resolve(key, locked, inhibited)
                    .map_or(KeyClaim::Pass, |(_, claim)| claim);
            }
        }
        if self.folds_the_desk(key) {
            return KeyClaim::Take;
        }
        self.resolve(key, locked, inhibited)
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
        // A press of the person's own: the guide leads with the keyboard
        // again. The controller's own presses reach here too, standing for
        // its buttons, and say nothing about a keyboard.
        if !self.controller_keys.get() {
            self.controller.set(None);
        }
        if self.guide.up() {
            self.guide_key(key);
            return;
        }
        if self.folds_the_desk(key) {
            self.fold.ask();
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
        if let Some((invocation, _)) = self.resolve(key, false, false) {
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
        SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Invocations::default(),
            None,
            GuideKeys::default(),
            DeskFold::default(),
            None,
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
        )
    }

    /// The key surface with the shortcut guide up, and the guide's shared
    /// keys.
    fn with_guide() -> (SessionKeys, GuideKeys) {
        let guide = GuideKeys::default();
        guide.set_up(true);
        let keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Invocations::default(),
            None,
            guide.clone(),
            DeskFold::default(),
            None,
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
        );
        (keys, guide)
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
            keys.resolve(&volume, true, false),
            Some((
                Invocation::new(QUICK_SETTINGS, "volume-up").showing(OsdKind::Volume),
                KeyClaim::TakeRepeating
            ))
        );
        let airplane = key(KEY_XF86_RFKILL, None, KeyModifiers::default());
        assert_eq!(keys.resolve(&airplane, true, false), None);
        assert_eq!(
            keys.resolve(&airplane, false, false),
            Some((
                Invocation::new(QUICK_SETTINGS, "toggle-airplane-mode")
                    .showing(OsdKind::AirplaneMode),
                KeyClaim::Take
            ))
        );
        let play = key(keysyms::XF86_AudioPlay, None, KeyModifiers::default());
        assert_eq!(
            keys.resolve(&play, true, false)
                .map(|(invocation, _)| invocation),
            Some(Invocation::new(MEDIA, "play-pause").showing(OsdKind::Play))
        );
    }

    #[test]
    fn the_print_key_takes_a_screenshot_unless_the_screen_is_locked() {
        let keys = keys();
        let print = key(keysyms::Print, None, KeyModifiers::default());
        assert_eq!(
            keys.resolve(&print, false, false),
            Some((
                Invocation::new(CAPTURE_STUDIO, "screenshot"),
                KeyClaim::Take
            ))
        );
        assert_eq!(keys.resolve(&print, true, false), None);
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
            keys.resolve(&tab, false, false),
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
        assert_eq!(keys.resolve(&tab, true, false), None);
        let a = key(keysyms::a, Some('a'), KeyModifiers::default());
        assert_eq!(keys.resolve(&a, false, false), None);
    }

    #[test]
    fn the_first_function_key_shows_the_shortcut_guide() {
        let keys = keys();
        let f1 = key(keysyms::F1, None, KeyModifiers::default());
        assert_eq!(
            keys.resolve(&f1, false, false),
            Some((
                Invocation::shortcut(ShortcutAction::ShortcutHelp),
                KeyClaim::Take
            ))
        );
        assert_eq!(
            Invocation::shortcut(ShortcutAction::ShortcutHelp).arguments["action"],
            "shortcut_help"
        );
        // The lock screen shows no desktop, and no guide to its keys.
        assert_eq!(keys.resolve(&f1, true, false), None);
    }

    #[test]
    fn the_shortcut_guide_holds_back_the_desktops_chords_while_it_is_up() {
        let (mut keys, guide) = with_guide();
        let logo = KeyModifiers {
            logo: true,
            ..KeyModifiers::default()
        };
        let space = key(keysyms::space, Some(' '), logo);
        assert_eq!(keys.claim_key(&space, false, false), KeyClaim::Take);
        keys.key(&space);
        assert!(
            keys.invocations.borrow().is_empty(),
            "search must not open on the guide: {:?}",
            keys.invocations.borrow()
        );
        // The guide's own key still reaches the desktop, which closes it.
        let f1 = key(keysyms::F1, None, KeyModifiers::default());
        assert_eq!(keys.claim_key(&f1, false, false), KeyClaim::Take);
        keys.key(&f1);
        assert_eq!(
            keys.invocations.borrow().as_slice(),
            [Invocation::shortcut(ShortcutAction::ShortcutHelp)]
        );
        // A key that answers nothing at all is not the guide's to take: the
        // sheet behind it moves its focus with Tab.
        let tab = key(keysyms::Tab, None, KeyModifiers::default());
        assert_eq!(keys.claim_key(&tab, false, false), KeyClaim::Pass);
        // A hardware key changes a level whatever is on screen.
        let volume = key(
            keysyms::XF86_AudioRaiseVolume,
            None,
            KeyModifiers::default(),
        );
        assert_eq!(
            keys.claim_key(&volume, false, false),
            KeyClaim::TakeRepeating
        );
        keys.invocations.borrow_mut().clear();
        keys.key(&volume);
        assert_eq!(
            keys.invocations.borrow().as_slice(),
            [Invocation::new(QUICK_SETTINGS, "volume-up").showing(OsdKind::Volume)]
        );
        assert_eq!(guide.take_caught(), []);
    }

    #[test]
    fn the_shortcut_guide_takes_the_next_chord_while_it_waits() {
        let (mut keys, guide) = with_guide();
        guide.listen_for("search");
        let control = KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        };
        let j = key(keysyms::j, Some('j'), control);
        assert_eq!(keys.claim_key(&j, false, false), KeyClaim::Take);
        keys.key(&j);
        assert_eq!(
            guide.take_caught(),
            [Caught::Chord(KeyChord::parse("Ctrl+J").expect("a chord"))]
        );
        assert!(
            keys.invocations.borrow().is_empty(),
            "the chord is the guide's, not the desktop's"
        );
        // A modifier on its own is no chord.
        let shift = key(
            keysyms::Shift_L,
            None,
            KeyModifiers {
                shift: true,
                ..KeyModifiers::default()
            },
        );
        keys.key(&shift);
        assert_eq!(guide.take_caught(), []);
        // Escape leaves the waiting to the guide, which stays up.
        let escape = key(keysyms::Escape, None, KeyModifiers::default());
        assert_eq!(keys.claim_key(&escape, false, false), KeyClaim::Take);
        keys.key(&escape);
        assert_eq!(guide.take_caught(), [Caught::Cancelled]);
        assert_eq!(guide.listening(), None);
        assert!(guide.up(), "the guide itself stays up");
    }

    #[test]
    fn a_client_holding_the_shortcuts_keeps_the_chords_but_not_the_way_to_lock() {
        let mut keys = keys();
        let logo = KeyModifiers {
            logo: true,
            ..KeyModifiers::default()
        };
        let tab = key(keysyms::Tab, None, logo);
        assert_eq!(keys.claim_key(&tab, false, false), KeyClaim::Take);
        assert_eq!(
            keys.claim_key(&tab, false, true),
            KeyClaim::Pass,
            "the virtual machine answers its own Tab"
        );
        // Locking the session stays the desktop's: a machine that could hold
        // that chord could hide the way out of itself.
        let lock = key(keysyms::l, Some('l'), logo);
        assert_eq!(keys.claim_key(&lock, false, false), KeyClaim::Take);
        assert_eq!(keys.claim_key(&lock, false, true), KeyClaim::Take);
        keys.invocations.borrow_mut().clear();
        keys.key(&lock);
        assert_eq!(
            keys.invocations.borrow().as_slice(),
            [Invocation::shortcut(ShortcutAction::Lock)]
        );
        // A hardware key changes a level whatever a client holds.
        let volume = key(
            keysyms::XF86_AudioRaiseVolume,
            None,
            KeyModifiers::default(),
        );
        assert_eq!(
            keys.claim_key(&volume, false, true),
            KeyClaim::TakeRepeating
        );
    }

    #[test]
    fn escape_folds_the_card_a_widget_has_open_and_nothing_behind_it_hears_it() {
        let fold = DeskFold::default();
        let mut keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Invocations::default(),
            None,
            GuideKeys::default(),
            fold.clone(),
            None,
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
        );
        let escape = key(keysyms::Escape, None, KeyModifiers::default());
        // Folded cards want no key: Escape goes on to whatever is up.
        assert_eq!(keys.claim_key(&escape, false, false), KeyClaim::Pass);

        fold.set_open(true);
        assert_eq!(
            keys.claim_key(&escape, false, false),
            KeyClaim::Take,
            "an open card folds on Escape before anything else hears it"
        );
        keys.key(&escape);
        assert!(fold.take_asked(), "the key asks the desk to fold");
        assert!(!fold.take_asked(), "and asks once");
    }
}
