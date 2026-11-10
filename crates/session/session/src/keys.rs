//! The keys the session answers wherever the keyboard is: the desktop's chords and the hardware
//! keys along the top of the keyboard. The compositor offers this surface every press before the
//! focused window or panel gets it ([`SurfaceHost::claim_key`]). A press that is a chord of the
//! desktop's one shortcut table ([`tilo_desktop::shortcuts`]) or a hardware key is taken and
//! becomes an invocation of a published action, run through the same door every other caller uses;
//! nothing here performs anything itself.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::platform::surfaces::DeskGesture;
use tilo_actions::{ActionAddress, DoorError};
use tilo_compositor::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_desktop::desk_widgets::DeskFold;
use tilo_desktop::osd::{Device, DeviceGlyph, Hint, Level, OSD_RESOURCE, Osd, OsdKind};
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_sheets::shortcut_guide::{Caught, State as GuideState};
use ui::host_operation::ResourceOperationStart;
use ui::shortcut_table::{FixedAction, KeyChord, ShortcutAction, ShortcutConfig};
use xkeysym::key as keysyms;

use crate::Shared;
use crate::controls::SessionCommand;
use crate::displays;
use crate::doors::plane::answered;
use crate::i18n::t;
use crate::shell::state::{OVERLAYS, WINDOW_MENU};

/// The key a press is, as the toolkit names it, with the modifiers held: what the shortcut
/// tables and the desktop's chords match against.
fn toolkit_key(key: &HostKey) -> (ui::winit::keyboard::Key, ui::winit::event::Modifiers) {
    let held = key.modifiers;
    let under_chord = held.ctrl || held.alt || held.logo;
    (
        ui::keys::chord(
            key.keysym,
            key.text.as_deref(),
            key.latin_keysym,
            under_chord,
        )
        .0,
        ui::keys::modifiers(held.ctrl, held.alt, held.shift, held.logo).into(),
    )
}

/// One published action to invoke, with its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The readout to show once it ran (a hardware key's answer on screen).
    pub readout: Option<OsdKind>,
}

impl Invocation {
    /// `action` of `resource` , without arguments.
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

    /// The same, with `name` set to `value` .
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        self.arguments.insert(name.to_owned(), value.into());
        self
    }

    /// The chrome's shortcut action for `action` .
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

/// Chords pressed that run a saved shortcut's keyboard trigger, waiting for the automation host to
/// fire them.
pub type PressedChords = Rc<RefCell<Vec<KeyChord>>>;

/// What the keys hand the rest of the session: the shortcuts the person's presses invoked, and the
/// chords an armed shortcut answers, which the automation host starts its run on.
#[derive(Clone, Default)]
pub(crate) struct Presses {
    pub invocations: Invocations,
    pub triggered: PressedChords,
}

/// Quick settings' resource, whose effects the notification and paste keys run.
pub const QUICK_SETTINGS: &str = "quick-settings.panel";
/// What plays: the audio service's media, whose own controls reach the player in front.
pub const MEDIA: &str = tilo_audio_service::media::MEDIA;
/// The sound: the audio service's, which owns the outputs, the level and the mute.
pub const SOUND: &str = tilo_audio_service::sound::SOUND;
/// The microphone: the audio service's sound resource.
pub const MICROPHONE: &str = SOUND;
/// The radios: the connectivity service's network, which owns airplane mode.
pub const NETWORK: &str = tilo_connectivity_service::NETWORK;
/// This device's input devices, the session's own.
const TOUCHPAD: &str = crate::controls::INPUT_RESOURCE;
/// The keyboard's backlight, the system service's.
pub const KEYBOARD_BACKLIGHT: &str = tilo_system_service::KEYBOARD_BACKLIGHT;
/// The displays: the session's own, their arrangements and their brightness.
const DISPLAYS: &str = crate::displays::RESOURCE;
/// The machine's power policy, the power service's: what the power key does.
pub const SLEEP: &str = tilo_session_power::SLEEP;
/// The capture studio, whose `screenshot` action the Print key is.
const CAPTURE_STUDIO: &str = tilo_capture_studio::RESOURCE;

/// `XF86XK_RFKill` , which the keysym tables predate: laptops send it for the airplane-mode key;
/// `XF86WLAN` is the older spelling.
const KEY_XF86_RFKILL: u32 = 0x1008_ffb5;

/// The app the calculator key opens.
const CALCULATOR: &str = "so.tilo.calculator";

/// How long a key's readout waits for the device to answer it before what it showed stands as it
/// is.
const READOUT_SETTLE: Duration = Duration::from_millis(250);

/// A key's readout whose action the device took and has not answered yet.
#[derive(Debug)]
pub(crate) struct PendingReadout {
    pub(crate) kind: OsdKind,
    /// What the readout showed when the key ran.
    pub(crate) before: Osd,
    pub(crate) at: Instant,
}

/// When the readouts a device has not answered yet stop waiting for what it answers: after that,
/// what one showed stands as it is.
pub(crate) fn next_readout_deadline(waiting: &[PendingReadout]) -> Option<Instant> {
    waiting
        .iter()
        .filter_map(|pending| pending.at.checked_add(READOUT_SETTLE))
        .min()
}

/// A hardware key: the action it runs, whether holding it repeats it, and whether it works on the
/// lock screen (it changes a level and reveals nothing behind the lock).
struct Hardware {
    action: HardwareAction,
    repeats: bool,
    while_locked: bool,
    /// The readout that answers it on screen.
    readout: Option<OsdKind>,
    /// Shift on the key asks for every display, as it does on the brightness key: the invocation
    /// carries `all` .
    shift_all: bool,
    /// The app the key's invocation opens, carried as its `app` argument.
    app: Option<&'static str>,
}

/// What a hardware key runs: an action of the session's resources, or one of the desktop's own
/// shortcuts, which the chrome carries out as its chord would.
#[derive(Clone, Copy)]
enum HardwareAction {
    Resource {
        resource: &'static str,
        action: &'static str,
    },
    Shortcut(ShortcutAction),
    /// Print: the capture studio, or the capture a modifier on it takes at once.
    Print,
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
        shift_all: false,
        app: None,
    }
}

/// A key that asks for `action` of the desktop's shortcut table, as the chord bound to it does.
const fn desktop(action: ShortcutAction, repeats: bool, while_locked: bool) -> Hardware {
    Hardware {
        action: HardwareAction::Shortcut(action),
        repeats,
        while_locked,
        readout: None,
        shift_all: false,
        app: None,
    }
}

impl Hardware {
    const fn reads(mut self, kind: OsdKind) -> Self {
        self.readout = Some(kind);
        self
    }

    /// Shift on it moves every display that can be dimmed.
    const fn together(mut self) -> Self {
        self.shift_all = true;
        self
    }

    /// The key's action opens `app` : what the calculator key opens.
    const fn opening(mut self, app: &'static str) -> Self {
        self.app = Some(app);
        self
    }
}

/// What the Print key asks the capture studio for: the studio itself, the whole display at once
/// with Shift, or the window in front at once with Alt. The studio's own action takes the window in
/// front when nothing is named, and comes up to choose one when nothing is in front.
fn print_invocation(modifiers: tilo_compositor::KeyModifiers) -> Invocation {
    if modifiers.shift {
        return Invocation::new(CAPTURE_STUDIO, "screenshot").with("source", "display");
    }
    if modifiers.alt {
        return Invocation::new(CAPTURE_STUDIO, "screenshot").with("source", "window");
    }
    Invocation::new(CAPTURE_STUDIO, "open").with(
        "mode",
        serde_json::to_value(ui::floating_surface::CaptureStudioProtocolMode::Screenshot)
            .unwrap_or_default(),
    )
}

/// The hardware key `keysym` is, if it is one.
fn classify(keysym: u32) -> Option<Hardware> {
    Some(match keysym {
        keysyms::XF86_AudioRaiseVolume => {
            hardware(SOUND, "volume-up", true, true).reads(OsdKind::Volume)
        }
        keysyms::XF86_AudioLowerVolume => {
            hardware(SOUND, "volume-down", true, true).reads(OsdKind::Volume)
        }
        keysyms::XF86_AudioMute => {
            hardware(SOUND, "toggle-muted", false, true).reads(OsdKind::Volume)
        }
        keysyms::XF86_MonBrightnessUp => hardware(DISPLAYS, "brightness-up", true, true)
            .reads(OsdKind::Brightness)
            .together(),
        keysyms::XF86_MonBrightnessDown => hardware(DISPLAYS, "brightness-down", true, true)
            .reads(OsdKind::Brightness)
            .together(),
        keysyms::XF86_WLAN | KEY_XF86_RFKILL => {
            hardware(NETWORK, "toggle-airplane-mode", false, false).reads(OsdKind::AirplaneMode)
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
        // The calculator key on a media keyboard opens Calculator, as the launcher's own action
        // does; the lock screen stays shut.
        keysyms::XF86_Calculator => {
            hardware(CHROME_RESOURCE, "open-app", false, false).opening(CALCULATOR)
        }
        // The dedicated sleep key, and the suspend and standby spellings other keyboards send,
        // sleeps the machine whatever the power policy says the power button does; it keeps its key
        // to itself on the lock screen, as suspending from there would.
        keysyms::XF86_Sleep | keysyms::XF86_Suspend | keysyms::XF86_Standby => {
            hardware(SLEEP, "sleep", false, false)
        }
        // The power key works on the lock screen too: a press there that the policy resolves to
        // lock turns the display off instead.
        keysyms::XF86_PowerOff => hardware(SLEEP, "power-key", false, true),
        // The Print key opens the capture studio; Shift takes the whole screen at once and Alt the
        // window in front, as the studio's own actions take them. The lock screen shows no desktop
        // to take.
        keysyms::Print => Hardware {
            action: HardwareAction::Print,
            repeats: false,
            while_locked: false,
            readout: None,
            shift_all: false,
            app: None,
        },
        // F1 names every key the desktop answers. It is the desktop's own key, which nothing may be
        // bound to, so the guide opens whatever modifiers are held with it.
        keysyms::F1 => desktop(ShortcutAction::ShortcutHelp, false, false),
        _ => return None,
    })
}

/// The controller the person last drove the desktop with, and whether the keys being routed are
/// that controller's own rather than their fingers on a keyboard.
#[derive(Clone, Default)]
pub(crate) struct ControllerKeys {
    /// The controller the person last drove the desktop with, which a press of their own replaces:
    /// the guide then leads with the keyboard.
    pub(crate) last: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed are a controller's own, standing for its presses rather than
    /// the person's fingers on a keyboard.
    pub(crate) own: Rc<Cell<bool>>,
}

/// What stands over the desk and what the keys owe it: Escape folds the card a desk widget has
/// open, and closes the topmost surface of the stack that answers Escape. Neither hears keys
/// itself, so both say here what theirs are worth.
#[derive(Clone, Default)]
pub(crate) struct DeskKeys {
    fold: DeskFold,
    overlays: Overlays,
    /// The desk's own overlays, where Alt+Tab's walk lives.
    switcher: Option<Rc<crate::overlays::Overlays>>,
    /// What the keys ask the panel's own app, when this device has one and draws it: Escape asks it
    /// to go back a page, and the panel answers whether it had one.
    panel: Option<Rc<RefCell<Vec<&'static str>>>>,
}

impl DeskKeys {
    /// The desk's fold and the surfaces over it, as the session holds them.
    #[must_use]
    pub(crate) fn new(fold: DeskFold, overlays: Overlays) -> Self {
        Self {
            fold,
            overlays,
            switcher: None,
            panel: None,
        }
    }

    /// The same, with the desk's own overlays: Alt+Tab's walk.
    #[must_use]
    pub(crate) fn with_switcher(mut self, switcher: Rc<crate::overlays::Overlays>) -> Self {
        self.switcher = Some(switcher);
        self
    }

    /// The same, with the queue the panel's own app is asked through, when this device has one and
    /// draws it.
    #[must_use]
    pub(crate) fn with_panel(mut self, asks: Option<Rc<RefCell<Vec<&'static str>>>>) -> Self {
        self.panel = asks;
        self
    }
}

/// The key above Tab: `` ` ``, and `~` with Shift held.
const ABOVE_TAB: [u32; 2] = [keysyms::grave, keysyms::asciitilde];

/// What a press is to the window switcher Alt+Tab opened, or to the chord that opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwitcherKey {
    /// Walk on (or back, with Shift), opening the walk over `opened` when it is not up yet.
    /// Everything else steps inside the scope already walking.
    Walk {
        back: bool,
        opened: crate::overlays::SwitcherScope,
    },
    /// Home and End: the window `at` places along the walk, `-1` the last.
    Place(i64),
    /// Enter, Space, or letting Alt go: go to the window the walk reached.
    Commit,
    /// Escape: close the panel and stay where the person was.
    Cancel,
    /// The walk takes the key and does nothing with it.
    Hold,
}

impl SwitcherKey {
    /// The desk action it asks for: the walk, the place or the commit, as the desk's own published
    /// action. `walking` is the scope of the walk already up: a walk going keeps it, so Alt+grave
    /// opens over the app in front's windows and Alt+Tab steps inside it.
    fn asks(self, walking: Option<crate::overlays::SwitcherScope>) -> Option<Invocation> {
        match self {
            Self::Walk { back, opened } => {
                let scope = walking.unwrap_or(opened);
                Some(
                    Invocation::new(tilo_session_core::shell::DESK_RESOURCE, "walk-switcher")
                        .with("steps", if back { -1 } else { 1 })
                        .with("scope", scope.name()),
                )
            }
            Self::Place(at) => Some(
                Invocation::new(tilo_session_core::shell::DESK_RESOURCE, "select-switcher")
                    .with("at", at),
            ),
            Self::Commit => Some(Invocation::new(
                tilo_session_core::shell::DESK_RESOURCE,
                "commit-switcher",
            )),
            Self::Cancel => Some(
                Invocation::new(tilo_session_core::shell::DESK_RESOURCE, "close-surface")
                    .with("surface", "switcher"),
            ),
            Self::Hold => None,
        }
    }
}

/// The virtual terminal `Ctrl+Alt+F1..F12` asks for, or `None` : the function keys line up with the
/// terminals, numbered from 1.
fn virtual_terminal(keysym: u32) -> Option<i32> {
    let vt = match keysym {
        keysyms::F1 => 1,
        keysyms::F2 => 2,
        keysyms::F3 => 3,
        keysyms::F4 => 4,
        keysyms::F5 => 5,
        keysyms::F6 => 6,
        keysyms::F7 => 7,
        keysyms::F8 => 8,
        keysyms::F9 => 9,
        keysyms::F10 => 10,
        keysyms::F11 => 11,
        keysyms::F12 => 12,
        _ => return None,
    };
    Some(vt)
}

/// The surface that takes the session's keys; it shows nothing.
pub(crate) struct SessionKeys {
    /// The desktop's shortcut table, shared with the shortcut guide so the guide explains the
    /// chords the keys resolve against.
    shortcuts: Rc<RefCell<ShortcutConfig>>,
    /// The display sheet's presentation: Super+P puts it up while it is not there, and once it is,
    /// the chord moves inside it instead.
    displays: Option<tilo_sheets::Presentation>,
    invocations: Invocations,
    /// The chords pressed that run a saved shortcut's keyboard trigger: the automation host starts
    /// them, on the pass that follows.
    triggered: PressedChords,
    /// The apps' shortcuts, which work wherever the keyboard is.
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    /// The shortcut guide's keys while it is up: what it holds back from the desktop behind it, and
    /// the chord it waits for.
    guide: Rc<GuideState>,
    /// Whether a desk widget has its card open, which Escape folds. The desk is under the windows
    /// and hears no keys itself, so it says here what its keys are worth.
    fold: DeskFold,
    /// What stands over the desk: Escape closes the topmost surface that answers it, and while one
    /// holds the keyboard a press is that surface's and not the window's.
    overlays: Overlays,
    /// The desk's own overlays, where Alt+Tab's walk lives: the keys it answers while it is up, and
    /// the chord that opens it.
    switcher: Option<Rc<crate::overlays::Overlays>>,
    /// What the keys ask the panel's own app, when this device draws it.
    panel: Option<Rc<RefCell<Vec<&'static str>>>>,
    /// The shortcut resources whose chords are held down, by key.
    held: RefCell<BTreeMap<u32, String>>,
    /// The controller the person last drove the desktop with, which a press of their own replaces:
    /// the guide then leads with the keyboard.
    controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed are a controller's own, standing for its presses rather than
    /// the person's fingers on a keyboard.
    controller_keys: Rc<Cell<bool>>,
    /// When each display's bar shows over the window covering it: Super asks for it, and the app
    /// under it has nothing of that press.
    reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
    /// The commands a press raises that are the session's own rather than a published action's,
    /// such as leaving for a virtual terminal.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
}

impl SessionKeys {
    pub(crate) fn new(
        shortcuts: Rc<RefCell<ShortcutConfig>>,
        presses: Presses,
        portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
        guide: Rc<GuideState>,
        desk: DeskKeys,
        displays: Option<tilo_sheets::Presentation>,
        controller: ControllerKeys,
        reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
    ) -> Self {
        let Presses {
            invocations,
            triggered,
        } = presses;
        let ControllerKeys {
            last: controller,
            own: controller_keys,
        } = controller;
        Self {
            shortcuts,
            displays,
            invocations,
            triggered,
            portal,
            guide,
            fold: desk.fold,
            overlays: desk.overlays,
            switcher: desk.switcher,
            panel: desk.panel,
            held: RefCell::new(BTreeMap::new()),
            controller,
            controller_keys,
            reveals,
            commands: Rc::default(),
        }
    }

    /// Take the commands the session's own presses raise, such as a virtual terminal's: the same
    /// queue the session's controls drain.
    pub(crate) fn with_commands(mut self, commands: Rc<RefCell<Vec<SessionCommand>>>) -> Self {
        self.commands = commands;
        self
    }

    /// Whether `key` is the desk's own: Escape folds the card a desk widget has open, and what sits
    /// behind the desk does not hear it.
    fn folds_the_desk(&self, key: &HostKey) -> bool {
        key.unmodified_keysym == keysyms::Escape && self.fold.is_open()
    }

    /// What `key` is to Alt+Tab's walk: the chord that opens it, what it does while it is up, or
    /// nothing when it is not the walk's at all. While the walk is up it is modal, as every
    /// desktop's switcher is: the keys are its own, and none of them reaches the window behind it.
    fn switcher_key(&self, key: &HostKey) -> Option<SwitcherKey> {
        let desk = self.switcher.as_deref()?;
        if !desk.is_up() {
            // Alt+Tab walks the workspace's windows, and Alt with the key above Tab the ones the
            // app in front shows: fixed chords, as no shortcut table holds a modifier.
            if !key.pressed || !key.modifiers.alt {
                return None;
            }
            if key.keysym == keysyms::Tab {
                return Some(SwitcherKey::Walk {
                    back: key.modifiers.shift,
                    opened: crate::overlays::SwitcherScope::Workspace,
                });
            }
            if ABOVE_TAB.contains(&key.keysym) && !key.modifiers.ctrl && !key.modifiers.logo {
                return Some(SwitcherKey::Walk {
                    back: key.modifiers.shift,
                    opened: crate::overlays::SwitcherScope::App,
                });
            }
            return None;
        }
        if !key.pressed {
            // Letting Alt go goes to the window the walk reached, whether the press was Alt's or
            // was lost to another seat: every other release is the walk's, and does nothing.
            let alt = matches!(key.keysym, keysyms::Alt_L | keysyms::Alt_R);
            return Some(if alt || !key.modifiers.alt {
                SwitcherKey::Commit
            } else {
                SwitcherKey::Hold
            });
        }
        // A key still repeating after Alt came up does not walk: the walk ends on that release, and
        // a repeat must not choose another window.
        if key.repeat && !key.modifiers.alt {
            return Some(SwitcherKey::Hold);
        }
        let walked = desk.scope();
        let back = key.modifiers.shift;
        Some(match key.keysym {
            keysyms::Tab | keysyms::ISO_Left_Tab => SwitcherKey::Walk {
                back,
                opened: walked,
            },
            keysym if ABOVE_TAB.contains(&keysym) => SwitcherKey::Walk {
                back,
                opened: walked,
            },
            keysyms::Escape => SwitcherKey::Cancel,
            keysyms::Return | keysyms::KP_Enter | keysyms::space => SwitcherKey::Commit,
            keysyms::Left | keysyms::Up => SwitcherKey::Walk {
                back: true,
                opened: walked,
            },
            keysyms::Right | keysyms::Down => SwitcherKey::Walk {
                back: false,
                opened: walked,
            },
            keysyms::Home => SwitcherKey::Place(0),
            keysyms::End => SwitcherKey::Place(-1),
            // A press without Alt settles on the window the walk reached: Alt came up without the
            // walk hearing it. It does not type into a window nobody chose yet.
            _ if !key.modifiers.alt => SwitcherKey::Commit,
            _ => SwitcherKey::Hold,
        })
    }

    /// Ask the desk for what a key of the walk's is worth.
    fn walk_the_switcher(&self, key: &HostKey) {
        let Some(action) = self.switcher_key(key) else {
            return;
        };
        let walking = self
            .switcher
            .as_deref()
            .filter(|desk| desk.is_up())
            .map(crate::overlays::Overlays::scope);
        if let Some(invocation) = action.asks(walking) {
            self.invocations.borrow_mut().push(invocation);
        }
    }

    /// The surface `key` closes, by the stack's dismissal table: Escape, for
    /// the topmost surface that answers it. The surface is drawn elsewhere,
    /// so this surface is the one that hears it.
    fn closes_the_top_surface(&self, key: &HostKey) -> Option<Slot> {
        (key.unmodified_keysym == keysyms::Escape)
            .then(|| self.overlays.escape())
            .flatten()
    }

    /// Whether `key` is the display sheet's chord, which puts the sheet up: Super+P, as the
    /// desktop's fixed table names it. A sheet that is up is not opened again: what is drawn takes
    /// the chord and moves the highlight inside it.
    fn opens_the_display_sheet(&self, key: &HostKey) -> bool {
        let Some(displays) = &self.displays else {
            return false;
        };
        if displays.is_drawn() {
            return false;
        }
        let (logical, modifiers) = toolkit_key(key);
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
        let (logical, modifiers) = toolkit_key(key);
        let shortcuts = self.shortcuts.borrow();
        tilo_desktop::shortcuts::configured_shortcut_action(&shortcuts, &logical, modifiers)
    }

    /// The fixed action `key` is: a chord Settings cannot rebind, answered
    /// before any app's. `None` for a chord the keys answer where they are
    /// resolved (the window walk, the workspace steps, the display sheet,
    /// Print) and for the accessibility chords, which have no native
    /// dispatcher yet.
    fn fixed_action(key: &HostKey) -> Option<FixedAction> {
        let (logical, modifiers) = toolkit_key(key);
        tilo_desktop::shortcuts::fixed_shortcut_action(&logical, modifiers)
    }

    /// The chord of this press that runs a saved shortcut's keyboard trigger,
    /// which the session claims and the automation host fires. The chord is
    /// the table's because the automation put it there when it armed the
    /// shortcut, and it stays the session's until the next arming drops it.
    fn trigger_chord(&self, key: &HostKey) -> Option<KeyChord> {
        let (logical, modifiers) = toolkit_key(key);
        let chord = tilo_desktop::shortcuts::shortcut_chord_from_event(&logical, modifiers)?;
        let shortcuts = self.shortcuts.borrow();
        shortcuts
            .action_for(&chord)
            .filter(|bound| tilo_action_broker::automation::is_shortcut_target(&bound.target))
            .map(|_| chord)
    }

    /// The invocation a press is, and how it is claimed. `inhibited` while a focused client holds
    /// the desktop's shortcuts (a virtual machine, a remote desktop): only a hardware key and
    /// locking the session stay the desktop's then.
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
                HardwareAction::Print => print_invocation(key.modifiers),
            };
            if let Some(app) = hardware.app {
                invocation = invocation.with("app", app);
            }
            if hardware.shift_all && key.modifiers.shift {
                invocation = invocation.with("all", true);
            }
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
        // Super+P is the display sheet's, as it is on every desktop: the keys the fixed table names
        // come before any app's chord.
        if self.opens_the_display_sheet(key) {
            return Some((Invocation::new(DISPLAYS, "open-sheet"), KeyClaim::Take));
        }
        // The fixed chords Settings cannot rebind are the desktop's before any app's, and run the
        // same verb their menu rows and an agent's invocation name.
        if let Some(action) = Self::fixed_action(key) {
            return Some((
                Invocation::new(CHROME_RESOURCE, "fixed-shortcut").with("action", action.id()),
                KeyClaim::Take,
            ));
        }
        // An app's bound shortcut is the person's answer to its sheet: it is pressed like the
        // desktop's own chords are.
        if let Some(resource) = self.portal_shortcut(key) {
            return Some((Invocation::new(&resource, "activate"), KeyClaim::Take));
        }
        let (logical, modifiers) = toolkit_key(key);
        let shortcuts = self.shortcuts.borrow();
        let action =
            tilo_desktop::shortcuts::configured_shortcut_action(&shortcuts, &logical, modifiers)?;
        Some((Invocation::shortcut(action), KeyClaim::Take))
    }

    /// Whether `invocation` is the desktop's way to the shortcut guide, which stays its own key
    /// while the guide is up.
    fn shows_the_guide(invocation: &Invocation) -> bool {
        *invocation == Invocation::shortcut(ShortcutAction::ShortcutHelp)
    }

    /// A press the guide is up over: it answers only what is its own, and while it waits for a new
    /// chord, that press is the chord.
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
        // A hardware key changes a level or takes the screen: it is no chord of the desktop's
        // table, so it answers as it does without the guide.
        if classify(key.keysym).is_some() {
            if let Some((invocation, _)) = resolved {
                self.invocations.borrow_mut().push(invocation);
            }
            return;
        }
        if self.guide.listening().is_none() || key.repeat {
            // The guide is modal for everything else: a chord acts once it is gone.
            return;
        }
        if key.unmodified_keysym == keysyms::Escape {
            self.guide.stop_listening();
            self.guide.catch(Caught::Cancelled);
            return;
        }
        let (logical, modifiers) = toolkit_key(key);
        // A modifier held on its own is no chord, and no modifier is either.
        if let Some(chord) = tilo_desktop::shortcuts::shortcut_chord_from_event(&logical, modifiers)
        {
            self.guide.catch(Caught::Chord(chord));
        }
    }
}

/// Whether `key` is a Super key going down for the first time: the key that asks for a bar hidden
/// by a fullscreen window.
fn is_super(key: &HostKey) -> bool {
    key.pressed
        && !key.repeat
        && matches!(
            key.keysym,
            keysyms::Super_L | keysyms::Super_R | keysyms::Meta_L | keysyms::Meta_R
        )
}

impl SurfaceHost for SessionKeys {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    /// The session's keys draw nothing: they take chords and hand the rest on, so no frame is
    /// theirs to ask for.
    fn prepare_frame(&mut self) -> bool {
        false
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn claim_key(&mut self, key: &HostKey, locked: bool, inhibited: bool) -> KeyClaim {
        // Ctrl+Alt+F1..F12 asks for a virtual terminal. On a bare terminal the session holds the
        // keyboard itself, so the kernel never sees the chord: without this, and from a desktop
        // that cannot give the machine back, the person could not leave it for a text console at
        // all. A locked session takes no such chord, as the lock takes no other.
        if !locked
            && key.pressed
            && !key.repeat
            && key.modifiers.ctrl
            && key.modifiers.alt
            && let Some(vt) = virtual_terminal(key.unmodified_keysym)
                .or_else(|| virtual_terminal(key.latin_keysym))
        {
            self.commands
                .borrow_mut()
                .push(SessionCommand::SwitchVt { vt });
            return KeyClaim::Take;
        }
        // Super going down asks for the bar over the window that covers the display, as reaching
        // for the top edge does. The app under it has nothing of a modifier that asked for the
        // desktop's own bar.
        if !locked && !inhibited && is_super(key) && self.reveals.borrow().covers_display(None) {
            self.invocations
                .borrow_mut()
                .push(Invocation::new(CHROME_RESOURCE, "reveal-top-bar"));
            return KeyClaim::Take;
        }
        if self.guide.up() {
            if self.guide.listening().is_some() {
                // The guide waits for a shortcut's new keys: every press is that chord, and none of
                // them reaches what is behind it.
                return KeyClaim::Take;
            }
            // The guide is modal: a chord of the desktop's table or an app's shortcut must not act
            // on what it hides. A hardware key (the volume keys, F1) is no chord and answers as it
            // does without the guide.
            if classify(key.keysym).is_none() {
                if self.trigger_chord(key).is_some() {
                    return KeyClaim::Take;
                }
                return self
                    .resolve(key, locked, inhibited)
                    .map_or(KeyClaim::Pass, |(_, claim)| claim);
            }
        }
        // Alt+Tab's walk: the chord that opens it, and, while it is up, every key — its own Alt's
        // release among them. A locked session, or one whose focused client holds the shortcuts,
        // opens no walk.
        if let Some(action) = self.switcher_key(key) {
            let up = self
                .switcher
                .as_deref()
                .is_some_and(crate::overlays::Overlays::is_up);
            if up || (!locked && !inhibited) {
                return if key.pressed && matches!(action, SwitcherKey::Walk { .. }) {
                    KeyClaim::TakeRepeating
                } else {
                    KeyClaim::Take
                };
            }
        }
        if self.closes_the_top_surface(key).is_some() {
            return KeyClaim::Take;
        }
        if self.folds_the_desk(key) {
            return KeyClaim::Take;
        }
        // A chord that runs a saved shortcut is the session's, as a chord of the desktop's own
        // table is: the shortcut fires rather than the window behind it. A locked session, or one
        // whose focused client holds the shortcuts, answers neither.
        if !locked && !inhibited && self.trigger_chord(key).is_some() {
            return KeyClaim::Take;
        }
        self.resolve(key, locked, inhibited)
            .map_or(KeyClaim::Pass, |(_, claim)| claim)
    }

    fn key(&mut self, key: &HostKey) {
        if !key.pressed {
            // The app hears its shortcut released wherever the modifiers are by then: the chord is
            // remembered by the key that held it.
            if let Some(resource) = self.held.borrow_mut().remove(&key.keycode) {
                self.invocations
                    .borrow_mut()
                    .push(Invocation::new(&resource, "deactivate"));
            }
            // Letting Alt go is the walk's, wherever the press that opened it was recorded: the
            // switcher goes to the window it reached.
            self.walk_the_switcher(key);
            return;
        }
        // A press of the person's own: the guide leads with the keyboard again. The controller's
        // own presses reach here too, standing for its buttons, and say nothing about a keyboard.
        if !self.controller_keys.get() {
            self.controller.set(None);
        }
        if self.guide.up() {
            self.guide_key(key);
            return;
        }
        // Alt+Tab's walk answers the keys the desk's switcher shows: the walk, its places, and the
        // press that settles it.
        if self.switcher_key(key).is_some() {
            self.walk_the_switcher(key);
            return;
        }
        if let Some(slot) = self.closes_the_top_surface(key) {
            if slot == Slot::QuickSettings
                && let Some(asks) = &self.panel
            {
                // The panel's own pages are its own: Escape goes back one of them, and the panel
                // answers whether it had one to go back to (`Shared::panel_back`). Its main page
                // has none, and the session closes the panel there.
                asks.borrow_mut().push("back");
                return;
            }
            self.invocations
                .borrow_mut()
                .push(Invocation::new(OVERLAYS, "close").with("slot", slot.as_str()));
            return;
        }
        if self.folds_the_desk(key) {
            self.fold.ask();
            return;
        }
        // A chord of a saved shortcut's keyboard trigger starts the shortcut: the automation host
        // is handed the press on this pass.
        if let Some(chord) = self.trigger_chord(key) {
            self.triggered.borrow_mut().push(chord);
            return;
        }
        // A repeat of a key taken once is not run again (only a level key repeats); the compositor
        // repeats only keys claimed repeating.
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

impl Shared {
    /// What the touchpad gestures the desktop took asked, as the chrome's published actions.
    pub(crate) fn follow_gestures(&self) {
        let gestures = self.desktop.take_gestures();
        let mut invocations = self.invocations.borrow_mut();
        for gesture in gestures {
            invocations.push(match gesture {
                DeskGesture::StepStrip(steps) => {
                    Invocation::new(CHROME_RESOURCE, "step-strip").with("steps", steps)
                }
                // A strip under the fingers is the desk's own drawing, as the strip deck is: the
                // fingers move it here, where the frame is, rather than through a published action
                // that would arrive a pass later.
                DeskGesture::StripDrag { travel, began } => {
                    self.desktop.with_widgets(|host| {
                        if began {
                            host.begin_strip_carry(travel);
                        } else {
                            host.drag_strip_carry(travel);
                        }
                    });
                    continue;
                }
                DeskGesture::StripDragEnd { cancelled } => {
                    self.desktop
                        .with_widgets(|host| host.end_strip_carry(cancelled));
                    continue;
                }
                // The deck is the desk's own drawing: the fingers move it here, where the frame is,
                // rather than through a published action that would arrive a pass later.
                DeskGesture::StripDeckDrag { travel } => {
                    self.desktop.with_widgets(|host| {
                        if host.wants_hand_on_deck() {
                            host.drag_strip_deck(travel);
                        } else {
                            host.begin_strip_deck(travel);
                        }
                    });
                    continue;
                }
                DeskGesture::StepStripFrom { travel, steps } => {
                    self.desktop.with_widgets(|host| {
                        host.end_strip_deck(travel);
                    });
                    if steps != 0 {
                        // A finger chose the strip: the change it makes is drawn sliding, not
                        // landed as a chord's would be.
                        ui::motion::note_source(ui::tokens::MotionSource::Gesture);
                        invocations.push(
                            Invocation::new(CHROME_RESOURCE, "step-strip").with("steps", steps),
                        );
                    }
                    continue;
                }
                DeskGesture::SwitchWorkspace(delta) => {
                    Invocation::new(CHROME_RESOURCE, "switch-workspace").with("delta", delta)
                }
                DeskGesture::Overview(open) => {
                    Invocation::new(CHROME_RESOURCE, "overview").with("open", open)
                }
            });
        }
    }

    /// What a secondary click on a window's bar asked, as the window menu's own published action:
    /// that window's menu up, where it was pressed.
    pub(crate) fn follow_tab_menus(&self) {
        let tabs = self.desktop.take_tab_menus();
        let mut invocations = self.invocations.borrow_mut();
        for (tab, x, y) in tabs {
            invocations.push(
                Invocation::new(WINDOW_MENU, "open")
                    .with("tab", tab.to_string())
                    .with("anchor", format!("{x},{y}")),
            );
        }
    }

    /// Run the published actions keys and gestures asked for, through the door every caller uses.
    /// The lock screen withholds the hosted surfaces' actions while it is up; the keys that work
    /// there are the device's own (a level, the power key), and the services publishing those still
    /// run.
    pub(crate) fn run_invocations(
        &self,
        locked: bool,
        look: &mut tilo_session_core::door::Look<'_>,
    ) -> bool {
        let invocations = std::mem::take(&mut *self.invocations.borrow_mut());
        let ran = !invocations.is_empty();
        for invocation in invocations {
            let outcome = self
                .doors
                .invoke(&invocation.address, &invocation.arguments, look)
                .or_else(|error| {
                    if !locked {
                        return Err(error);
                    }
                    self.services
                        .borrow_mut()
                        .invoke(
                            invocation.address.resource.as_str(),
                            invocation.address.action.as_str(),
                            &invocation.arguments,
                        )
                        .map(|result| result.map(answered))
                        .unwrap_or(Err(error))
                });
            match outcome {
                Ok(started) => {
                    if let Some(kind) = invocation.readout {
                        let now = self.reading(kind);
                        self.show_readout(&now);
                        if started == ResourceOperationStart::Accepted {
                            // The device has not answered yet: the readout shows where it started,
                            // and again where the device left it once the reading says so.
                            self.waiting.borrow_mut().push(PendingReadout {
                                kind,
                                before: now,
                                at: Instant::now(),
                            });
                        }
                    }
                }
                Err(error) => {
                    if invocation.readout == Some(OsdKind::Brightness)
                        && Self::says_nothing_to_dim(&error)
                    {
                        self.show_readout(&Osd::Hint(Hint {
                            kind: OsdKind::NoBacklight,
                            on: true,
                            chord: None,
                            name: None,
                        }));
                    }
                    tracing::warn!(
                        ?error,
                        address = %invocation.address.encode(),
                        "nothing carried out a key's action"
                    );
                }
            }
        }
        ran
    }

    /// What a service publishes now, read back from `resource` , or `None` while nothing publishes
    /// it or it does not read back.
    fn published<T: serde::de::DeserializeOwned>(&self, resource: &str) -> Option<T> {
        let reading = self.services.borrow().read(resource)?;
        serde_json::from_value(reading)
            .inspect_err(|error| {
                tracing::warn!(%error, resource, "a service's reading does not read back");
            })
            .ok()
    }

    /// The readout of `kind` as the device's own service publishes it now: naming the device it is
    /// about, so the pill says whose level it is.
    pub(crate) fn reading(&self, kind: OsdKind) -> Osd {
        if !kind.has_level() {
            return self.without_level(kind);
        }
        // A brightness key on a machine with no display it can dim: the readout says there is
        // nothing to adjust rather than a level of its own, which would be a zero about no display.
        if kind == OsdKind::Brightness
            && self
                .published::<displays::Reading>(displays::RESOURCE)
                .is_none_or(|reading| Self::dimmable(&reading).is_none())
        {
            return Osd::Hint(Hint {
                kind: OsdKind::NoBacklight,
                on: true,
                chord: None,
                name: None,
            });
        }
        let (level, on, device) = match kind {
            OsdKind::Volume => self
                .published::<tilo_audio_service::sound::Sound>(SOUND)
                .map_or((0.0, true, None), |sound| {
                    let device = sound
                        .outputs
                        .iter()
                        .find(|output| output.chosen)
                        .map(|output| {
                            let name = if output.label.is_empty() {
                                output.name.clone()
                            } else {
                                output.label.clone()
                            };
                            Device::new(name, DeviceGlyph::of_sink(&output.name))
                        });
                    (sound.volume.unwrap_or(0.0), !sound.muted, device)
                }),
            OsdKind::Brightness => self
                .published::<displays::Reading>(displays::RESOURCE)
                .map_or((0.0, true, None), |reading| {
                    let device = Self::dimmable(&reading)
                        .map(|display| Device::new(display.name.clone(), DeviceGlyph::Display));
                    (reading.brightness().unwrap_or(0.0), true, device)
                }),
            OsdKind::KeyboardBacklight => self
                .published::<tilo_system_service::model::KeyboardLight>(KEYBOARD_BACKLIGHT)
                .map_or((0.0, true, None), |light| {
                    let level = light
                        .light()
                        .map_or(0.0, os_actions::KeyboardBacklight::fraction);
                    let device = Device::new(t("osd-keyboard-name"), DeviceGlyph::Keyboard);
                    (level, level > 0.0, Some(device))
                }),
            _ => (0.0, true, None),
        };
        Osd::Level(Level {
            kind,
            level,
            on,
            device,
        })
    }

    /// The readout of a key whose action is on or off, as the services publish it now: the pill
    /// shows what the key turned on or off, not a level.
    fn without_level(&self, kind: OsdKind) -> Osd {
        let on = match kind {
            OsdKind::Microphone => self
                .published::<tilo_audio_service::sound::Sound>(SOUND)
                .is_none_or(|sound| !sound.input_muted),
            OsdKind::AirplaneMode => self
                .published::<tilo_connectivity_service::model::Network>(NETWORK)
                .is_some_and(|network| network.airplane_mode),
            _ => true,
        };
        // A transport readout names the song it is about, read from the one now-playing reading
        // every surface shows. Going to the next or the previous song names no song: which one
        // plays is not read yet.
        let name = matches!(kind, OsdKind::Play | OsdKind::Pause)
            .then(|| {
                self.readings
                    .borrow()
                    .playing()
                    .map(|playing| playing.title)
            })
            .flatten();
        Osd::Hint(Hint {
            kind,
            on,
            chord: None,
            name,
        })
    }

    /// The display a brightness key is about: the one the person works on when it can be dimmed,
    /// else the first one that can ([`displays::Reading::brightness`]).
    fn dimmable(reading: &displays::Reading) -> Option<&displays::Display> {
        reading
            .focused
            .as_deref()
            .and_then(|focused| {
                reading
                    .displays
                    .iter()
                    .find(|display| display.connector == focused && display.brightness.is_some())
            })
            .or_else(|| {
                reading
                    .displays
                    .iter()
                    .find(|display| display.brightness.is_some())
            })
    }

    /// Show the readouts a device answered since the key was pressed, where it left them: the level
    /// a person sees is the one their key produced, not the one before it.
    pub(crate) fn show_answered_readouts(&self) {
        let waiting = std::mem::take(&mut *self.waiting.borrow_mut());
        let now = Instant::now();
        let mut unanswered = Vec::new();
        for pending in waiting {
            let answered = self.reading(pending.kind);
            if answered != pending.before {
                self.show_readout(&answered);
            } else if now.saturating_duration_since(pending.at) < READOUT_SETTLE {
                unanswered.push(pending);
            }
        }
        *self.waiting.borrow_mut() = unanswered;
    }

    /// Whether `error` is the session saying that no display here can be dimmed.
    fn says_nothing_to_dim(error: &DoorError) -> bool {
        matches!(error, DoorError::Unavailable { code, .. } if code == "no-backlight")
    }

    /// Show `osd` through the readout's published action, on the display the person works on: the
    /// readout answers there, over whatever that display shows, including the lock.
    pub(crate) fn show_readout(&self, osd: &Osd) {
        let display = self
            .services
            .borrow()
            .read(displays::RESOURCE)
            .and_then(|reading| {
                serde_json::from_value::<displays::Reading>(reading)
                    .inspect_err(|error| {
                        tracing::warn!(%error, "the displays service's reading does not read back");
                    })
                    .ok()
            })
            .and_then(|reading| {
                // A brightness key dims the display it is about; every other readout answers on the
                // display the person works on.
                if osd.kind() == OsdKind::Brightness {
                    Self::dimmable(&reading).map(|display| display.connector.clone())
                } else {
                    reading.focused
                }
            });
        let arguments = osd.arguments(display.as_deref());
        let address = ActionAddress::new(OSD_RESOURCE, "show");
        if let Err(error) = self.doors.invoke(
            &address,
            &arguments,
            &mut tilo_session_core::door::Look::none(),
        ) {
            tracing::warn!(?error, "no readout shown");
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
            latin_keysym: keysym,
            text: text.map(String::from),
            modifiers,
        }
    }

    fn keys() -> SessionKeys {
        SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            Rc::default(),
            DeskKeys::default(),
            None,
            ControllerKeys::default(),
            Rc::default(),
        )
    }

    /// The key surface with the shortcut guide up, and the guide's shared keys.
    fn with_guide() -> (SessionKeys, Rc<GuideState>) {
        let guide: Rc<GuideState> = Rc::default();
        guide.set_up(true);
        let keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            guide.clone(),
            DeskKeys::default(),
            None,
            ControllerKeys::default(),
            Rc::default(),
        );
        (keys, guide)
    }

    /// The key surface with the desk's walk to answer to, and that walk.
    fn with_walk() -> (SessionKeys, Rc<crate::overlays::Overlays>) {
        let walk = Rc::new(crate::overlays::Overlays::new());
        let keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            Rc::default(),
            DeskKeys::default().with_switcher(walk.clone()),
            None,
            ControllerKeys::default(),
            Rc::default(),
        );
        (keys, walk)
    }

    /// Alt held, and with it whatever else `modifiers` says.
    fn alt(modifiers: KeyModifiers) -> KeyModifiers {
        KeyModifiers {
            alt: true,
            ..modifiers
        }
    }

    /// What the keys asked the desk for since the last call.
    fn asked(keys: &SessionKeys) -> Vec<Invocation> {
        keys.invocations.borrow_mut().drain(..).collect()
    }

    fn desk(action: &str) -> Invocation {
        Invocation::new(tilo_session_core::shell::DESK_RESOURCE, action)
    }

    /// A release of `keysym` with `modifiers` held.
    fn released(keysym: u32, modifiers: KeyModifiers) -> HostKey {
        HostKey {
            pressed: false,
            ..key(keysym, None, modifiers)
        }
    }

    #[test]
    fn alt_tab_opens_the_walk_and_shift_walks_it_back() {
        let (mut keys, _walk) = with_walk();
        let tab = key(keysyms::Tab, None, alt(KeyModifiers::default()));
        assert_eq!(keys.claim_key(&tab, false, false), KeyClaim::TakeRepeating);
        keys.key(&tab);
        assert_eq!(
            asked(&keys),
            vec![
                desk("walk-switcher")
                    .with("steps", 1)
                    .with("scope", "workspace")
            ],
            "Alt+Tab walks the workspace's windows on from the one in front"
        );

        let backwards = key(
            keysyms::Tab,
            None,
            alt(KeyModifiers {
                shift: true,
                ..KeyModifiers::default()
            }),
        );
        keys.key(&backwards);
        assert_eq!(
            asked(&keys),
            vec![
                desk("walk-switcher")
                    .with("steps", -1)
                    .with("scope", "workspace")
            ],
            "Shift walks the other way"
        );
    }

    #[test]
    fn alt_and_the_key_above_tab_walk_the_front_apps_windows() {
        let (mut keys, _walk) = with_walk();
        let grave = key(keysyms::grave, Some('`'), alt(KeyModifiers::default()));
        assert_eq!(
            keys.claim_key(&grave, false, false),
            KeyClaim::TakeRepeating
        );
        keys.key(&grave);
        assert_eq!(
            asked(&keys),
            vec![desk("walk-switcher").with("steps", 1).with("scope", "app")]
        );
        // The chord a text field types is nobody's walk: Ctrl or Super with the key above Tab.
        for other in [
            KeyModifiers {
                ctrl: true,
                ..alt(KeyModifiers::default())
            },
            KeyModifiers {
                logo: true,
                ..alt(KeyModifiers::default())
            },
        ] {
            let chord = key(keysyms::grave, Some('`'), other);
            assert_ne!(
                keys.claim_key(&chord, false, false),
                KeyClaim::TakeRepeating,
                "{other:?} is no walk"
            );
            keys.key(&chord);
            assert!(asked(&keys).is_empty(), "{other:?} is no walk");
        }
    }

    #[test]
    fn a_lone_alt_is_no_walk_and_a_locked_session_opens_none() {
        let (mut keys, _walk) = with_walk();
        let alone = key(keysyms::Alt_L, None, alt(KeyModifiers::default()));
        assert_eq!(keys.claim_key(&alone, false, false), KeyClaim::Pass);
        keys.key(&alone);
        assert!(
            asked(&keys).is_empty(),
            "a modifier on its own reaches the window"
        );

        let tab = key(keysyms::Tab, None, alt(KeyModifiers::default()));
        assert_eq!(
            keys.claim_key(&tab, true, false),
            KeyClaim::Pass,
            "the lock takes no chord that walks the windows behind it"
        );
        assert_eq!(
            keys.claim_key(&tab, false, true),
            KeyClaim::Pass,
            "a client that holds the shortcuts opens no walk"
        );
    }

    #[test]
    fn the_walk_is_modal_while_it_is_up() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let letter = key(keysyms::a, Some('a'), alt(KeyModifiers::default()));
        assert_eq!(
            keys.claim_key(&letter, false, false),
            KeyClaim::Take,
            "while the walk is up no key reaches the window behind it"
        );
        keys.key(&letter);
        assert!(
            asked(&keys).is_empty(),
            "an unbound key holds the walk where it is"
        );
        // Locked or not, a walk that is up hears its own keys: it is over the lock too.
        for locked in [false, true] {
            assert_eq!(keys.claim_key(&letter, locked, false), KeyClaim::Take);
        }
    }

    #[test]
    fn letting_alt_go_goes_to_the_window_the_walk_reached() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let let_go = released(keysyms::Alt_L, KeyModifiers::default());
        assert_eq!(keys.claim_key(&let_go, false, false), KeyClaim::Take);
        keys.key(&let_go);
        assert_eq!(asked(&keys), vec![desk("commit-switcher")]);

        // Any other key released with Alt still down is the walk's and does nothing.
        let tab = released(keysyms::Tab, alt(KeyModifiers::default()));
        keys.key(&tab);
        assert!(asked(&keys).is_empty(), "releasing Tab is no answer");
        // A key released once Alt came up commits too: Alt's own release was lost.
        let stray = released(keysyms::a, KeyModifiers::default());
        keys.key(&stray);
        assert_eq!(asked(&keys), vec![desk("commit-switcher")]);
    }

    #[test]
    fn escape_closes_the_walk_and_enter_goes_to_it() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let escape = key(keysyms::Escape, None, alt(KeyModifiers::default()));
        assert_eq!(keys.claim_key(&escape, false, false), KeyClaim::Take);
        keys.key(&escape);
        assert_eq!(
            asked(&keys),
            vec![desk("close-surface").with("surface", "switcher")]
        );

        for commit in [keysyms::Return, keysyms::KP_Enter, keysyms::space] {
            let key = key(commit, None, alt(KeyModifiers::default()));
            keys.key(&key);
            assert_eq!(asked(&keys), vec![desk("commit-switcher")], "{commit:#x}");
        }
    }

    #[test]
    fn escape_asks_the_panel_to_go_back_before_it_closes_it() {
        // The panel's app draws the quick settings on this device: Escape is its own pages' first,
        // and the session waits for its answer.
        let asks = Rc::new(RefCell::new(Vec::new()));
        let overlays = Overlays::new();
        overlays.open(Slot::QuickSettings, None);
        let mut keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            Rc::default(),
            DeskKeys::new(DeskFold::default(), overlays.clone()).with_panel(Some(Rc::clone(&asks))),
            None,
            ControllerKeys::default(),
            Rc::default(),
        );
        let escape = key(keysyms::Escape, None, KeyModifiers::default());
        keys.key(&escape);
        assert_eq!(*asks.borrow(), ["back"], "the panel goes back a page");
        assert!(
            asked(&keys).is_empty(),
            "nothing closes until the panel says it has nowhere left to go"
        );

        // A device whose shell draws the quick settings has no panel app to ask: Escape closes the
        // surface itself.
        let mut shell = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            Rc::default(),
            DeskKeys::new(DeskFold::default(), overlays),
            None,
            ControllerKeys::default(),
            Rc::default(),
        );
        shell.key(&escape);
        assert_eq!(
            asked(&shell),
            vec![Invocation::new(OVERLAYS, "close").with("slot", "quick-settings")]
        );
    }

    #[test]
    fn home_end_and_the_arrows_move_inside_the_walk() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let home = key(keysyms::Home, None, alt(KeyModifiers::default()));
        keys.key(&home);
        assert_eq!(asked(&keys), vec![desk("select-switcher").with("at", 0)]);
        let end = key(keysyms::End, None, alt(KeyModifiers::default()));
        keys.key(&end);
        assert_eq!(asked(&keys), vec![desk("select-switcher").with("at", -1)]);
        for (arrow, steps) in [
            (keysyms::Left, -1),
            (keysyms::Up, -1),
            (keysyms::Right, 1),
            (keysyms::Down, 1),
        ] {
            let arrow = key(arrow, None, alt(KeyModifiers::default()));
            keys.key(&arrow);
            assert_eq!(
                asked(&keys),
                vec![
                    desk("walk-switcher")
                        .with("steps", steps)
                        .with("scope", "workspace")
                ],
                "an arrow steps the walk: {steps}"
            );
        }
    }

    #[test]
    fn a_walk_going_keeps_its_scope_and_the_other_chord_starts_it_again() {
        let (mut keys, walk) = with_walk();
        // A walk in the app's scope: Tab and the key above Tab both step inside it.
        walk.walk(Some("app"), 1);
        for keysym in [keysyms::Tab, keysyms::grave] {
            let step = key(keysym, None, alt(KeyModifiers::default()));
            keys.key(&step);
            assert_eq!(
                asked(&keys),
                vec![desk("walk-switcher").with("steps", 1).with("scope", "app")],
                "a walk going keeps the windows it crosses: {keysym:#x}"
            );
        }
        // The other chord does not cross to the other scope in a walk going: it steps, as it always
        // has. The scope is the chord that opened the walk.
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let grave = key(keysyms::grave, Some('`'), alt(KeyModifiers::default()));
        keys.key(&grave);
        assert_eq!(
            asked(&keys),
            vec![
                desk("walk-switcher")
                    .with("steps", 1)
                    .with("scope", "workspace")
            ]
        );
        // Alt let go and pressed again with the key above Tab opens the app's walk.
        walk.close("switcher");
        walk.forget_walk();
        keys.key(&grave);
        assert_eq!(
            asked(&keys),
            vec![desk("walk-switcher").with("steps", 1).with("scope", "app")]
        );
    }

    #[test]
    fn a_press_without_alt_settles_the_walk_and_its_repeats_do_not() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let typing = key(keysyms::a, Some('a'), KeyModifiers::default());
        assert_eq!(keys.claim_key(&typing, false, false), KeyClaim::Take);
        keys.key(&typing);
        assert_eq!(
            asked(&keys),
            vec![desk("commit-switcher")],
            "Alt came up without the walk hearing it: the press settles the walk"
        );

        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let repeat = HostKey {
            repeat: true,
            ..key(keysyms::a, Some('a'), KeyModifiers::default())
        };
        assert_eq!(keys.claim_key(&repeat, false, false), KeyClaim::Take);
        keys.key(&repeat);
        assert!(
            asked(&keys).is_empty(),
            "a key still repeating as Alt comes up does not choose another window"
        );
    }

    #[test]
    fn the_key_surface_draws_no_frame() {
        let mut keys = keys();
        assert!(
            !keys.prepare_frame(),
            "the keys take chords and pass the rest on: a frame is never theirs"
        );
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
                Invocation::new(SOUND, "volume-up").showing(OsdKind::Volume),
                KeyClaim::TakeRepeating
            ))
        );
        let airplane = key(KEY_XF86_RFKILL, None, KeyModifiers::default());
        assert_eq!(keys.resolve(&airplane, true, false), None);
        assert_eq!(
            keys.resolve(&airplane, false, false),
            Some((
                Invocation::new(NETWORK, "toggle-airplane-mode").showing(OsdKind::AirplaneMode),
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
    fn the_calculator_key_opens_calculator_and_the_sleep_key_sleeps() {
        let keys = keys();
        let calculator = key(keysyms::XF86_Calculator, None, KeyModifiers::default());
        assert_eq!(
            keys.resolve(&calculator, false, false),
            Some((
                Invocation::new(CHROME_RESOURCE, "open-app").with("app", CALCULATOR),
                KeyClaim::Take
            ))
        );
        assert_eq!(
            keys.resolve(&calculator, true, false),
            None,
            "a key that opens an app reveals nothing behind the lock"
        );
        for sleeping in [
            keysyms::XF86_Sleep,
            keysyms::XF86_Suspend,
            keysyms::XF86_Standby,
        ] {
            let sleep = key(sleeping, None, KeyModifiers::default());
            assert_eq!(
                keys.resolve(&sleep, false, false),
                Some((Invocation::new(SLEEP, "sleep"), KeyClaim::Take))
            );
            assert_eq!(keys.resolve(&sleep, true, false), None);
        }
        let sleep = key(keysyms::XF86_Sleep, None, KeyModifiers::default());
        assert_ne!(
            keys.resolve(&sleep, false, false)
                .map(|(invocation, _)| invocation),
            Some(Invocation::new(SLEEP, "power-key")),
            "the sleep key sleeps whatever the power button is set to"
        );
    }

    #[test]
    fn the_print_key_opens_the_studio_or_takes_what_a_modifier_names() {
        let keys = keys();
        let print = key(keysyms::Print, None, KeyModifiers::default());
        let studio = Invocation::new(CAPTURE_STUDIO, "open").with(
            "mode",
            serde_json::to_value(ui::floating_surface::CaptureStudioProtocolMode::Screenshot)
                .unwrap_or_default(),
        );
        // Print alone asks the studio to come up choosing a screenshot: the person then picks the
        // screen, a window or a region.
        assert_eq!(
            keys.resolve(&print, false, false),
            Some((studio, KeyClaim::Take))
        );
        // Shift takes the whole display at once, and Alt the window in front, as the studio's own
        // actions take them.
        let shift = key(
            keysyms::Print,
            None,
            KeyModifiers {
                shift: true,
                ..KeyModifiers::default()
            },
        );
        assert_eq!(
            keys.resolve(&shift, false, false),
            Some((
                Invocation::new(CAPTURE_STUDIO, "screenshot").with("source", "display"),
                KeyClaim::Take
            ))
        );
        let alt = key(
            keysyms::Print,
            None,
            KeyModifiers {
                alt: true,
                ..KeyModifiers::default()
            },
        );
        assert_eq!(
            keys.resolve(&alt, false, false),
            Some((
                Invocation::new(CAPTURE_STUDIO, "screenshot").with("source", "window"),
                KeyClaim::Take
            ))
        );
        // The lock screen shows no desktop to take.
        assert_eq!(keys.resolve(&print, true, false), None);
    }

    #[test]
    fn a_chord_on_a_cyrillic_layout_is_the_latin_key_of_that_key() {
        let keys = keys();
        let logo = KeyModifiers {
            logo: true,
            ..KeyModifiers::default()
        };
        // The key a US keyboard labels L types `д` on a Russian layout; the chord is the Latin
        // letter the same key carries on US.
        let mut cyrillic = key(keysyms::Cyrillic_de, Some('д'), logo);
        cyrillic.latin_keysym = u32::from('l');
        assert_eq!(
            keys.resolve(&cyrillic, false, false),
            Some((Invocation::shortcut(ShortcutAction::Lock), KeyClaim::Take))
        );
        // Without the Latin key on that layout, it is no chord at all.
        cyrillic.latin_keysym = keysyms::Cyrillic_de;
        assert_eq!(keys.resolve(&cyrillic, false, false), None);
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
    fn the_fixed_chords_run_the_chrome_verb_their_menu_rows_name() {
        let keys = keys();
        let logo = KeyModifiers {
            logo: true,
            ..KeyModifiers::default()
        };
        let shift_logo = KeyModifiers {
            shift: true,
            ..logo
        };
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        };
        let fixed = |id: &str| {
            Some((
                Invocation::new(CHROME_RESOURCE, "fixed-shortcut").with("action", id),
                KeyClaim::Take,
            ))
        };
        for (pressed, id) in [
            (key(keysyms::w, Some('w'), logo), "fixed-close-tab"),
            (key(keysyms::F4, None, alt), "fixed-close-tab"),
            (key(keysyms::n, Some('n'), logo), "fixed-new-window"),
            (key(keysyms::t, Some('t'), logo), "fixed-duplicate-tab"),
            (key(keysyms::Return, None, logo), "fixed-open-terminal"),
            (key(keysyms::s, Some('s'), logo), "fixed-strip-switcher"),
            (
                key(keysyms::s, Some('S'), shift_logo),
                "fixed-capture-screenshot",
            ),
            (
                key(keysyms::p, Some('P'), shift_logo),
                "fixed-picture-in-picture",
            ),
            (
                key(keysyms::r, Some('R'), shift_logo),
                "fixed-save-recent-replay",
            ),
            (key(keysyms::h, Some('h'), logo), "fixed-workspace-history"),
            (
                key(keysyms::h, Some('H'), shift_logo),
                "fixed-explore-workspace-history",
            ),
            (key(keysyms::Escape, None, logo), "fixed-stop-remote-input"),
        ] {
            assert_eq!(keys.resolve(&pressed, false, false), fixed(id), "{id}");
        }
        // The chords the keys answer where they resolve keep that answer: the window walk, the
        // workspace steps, the display sheet and Print.
        for answered_before in [
            key(
                keysyms::Tab,
                None,
                KeyModifiers {
                    alt: true,
                    ..KeyModifiers::default()
                },
            ),
            key(keysyms::Page_Up, None, logo),
            key(keysyms::p, Some('p'), logo),
            key(keysyms::Print, None, KeyModifiers::default()),
        ] {
            let resolved = keys.resolve(&answered_before, false, false);
            assert!(
                resolved
                    .as_ref()
                    .is_none_or(|(invocation, _)| invocation.address.action != "fixed-shortcut"),
                "{resolved:?}"
            );
        }
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
        // A key that answers nothing at all is not the guide's to take: the sheet behind it moves
        // its focus with Tab.
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
            [Invocation::new(SOUND, "volume-up").showing(OsdKind::Volume)]
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
        // Locking the session stays the desktop's: a machine that could hold that chord could hide
        // the way out of itself.
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
    fn a_shortcuts_chord_is_claimed_and_handed_to_the_automation_host() {
        let shortcuts = Rc::new(RefCell::new(ShortcutConfig::default()));
        let triggered = PressedChords::default();
        let mut keys = SessionKeys::new(
            Rc::clone(&shortcuts),
            Presses {
                invocations: Invocations::default(),
                triggered: Rc::clone(&triggered),
            },
            None,
            Rc::default(),
            DeskKeys::default(),
            None,
            ControllerKeys::default(),
            Rc::default(),
        );
        let chord = KeyChord::parse("ctrl+alt+j").expect("a chord");
        // Nothing armed: the chord is nobody's, and types into what is up.
        assert_eq!(
            keys.claim_key(
                &key(
                    keysyms::j,
                    Some('j'),
                    KeyModifiers {
                        ctrl: true,
                        alt: true,
                        ..KeyModifiers::default()
                    }
                ),
                false,
                false
            ),
            KeyClaim::Pass
        );
        // The automation armed a shortcut on that chord: the session answers it, and hands the
        // press on once.
        let mut table = shortcuts.borrow().clone();
        assert!(tilo_action_broker::automation::replace_trigger_chords(
            &mut table,
            vec![settings_store::hardware::ActionChord {
                chord: chord.clone(),
                target: tilo_action_broker::automation::chord_target("workspace", "tidy"),
                label: "Run Tidy".into(),
            }],
        ));
        *shortcuts.borrow_mut() = table;
        let press = key(
            keysyms::j,
            Some('j'),
            KeyModifiers {
                ctrl: true,
                alt: true,
                ..KeyModifiers::default()
            },
        );
        assert_eq!(keys.claim_key(&press, false, false), KeyClaim::Take);
        assert_eq!(
            keys.claim_key(&press, true, false),
            KeyClaim::Pass,
            "a locked session runs no shortcut"
        );
        keys.key(&press);
        assert_eq!(triggered.borrow().as_slice(), [chord]);
        assert!(
            keys.invocations.borrow().is_empty(),
            "the shortcut starts on the automation host, not as a desktop action"
        );
    }

    #[test]
    fn escape_folds_the_card_a_widget_has_open_and_nothing_behind_it_hears_it() {
        let fold = DeskFold::default();
        let mut keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            Rc::default(),
            DeskKeys::new(fold.clone(), Overlays::new()),
            None,
            ControllerKeys::default(),
            Rc::default(),
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
    /// Ctrl+Alt+F3 asks the session to leave for the third virtual terminal,
    /// and a locked session takes no such chord: the way out of a desktop that
    /// cannot give the machine back is not a way around the lock.
    #[test]
    fn control_alt_function_keys_ask_for_a_virtual_terminal() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let mut keys = SessionKeys::new(
            Rc::new(RefCell::new(ShortcutConfig::default())),
            Presses::default(),
            None,
            Rc::default(),
            DeskKeys::default(),
            None,
            ControllerKeys::default(),
            Rc::default(),
        )
        .with_commands(Rc::clone(&commands));
        let chord = KeyModifiers {
            ctrl: true,
            alt: true,
            ..KeyModifiers::default()
        };
        assert_eq!(
            keys.claim_key(&key(keysyms::F3, None, chord), false, false),
            KeyClaim::Take
        );
        assert_eq!(
            commands.borrow().as_slice(),
            [SessionCommand::SwitchVt { vt: 3 }]
        );
        // A layout that types another character on that key still names the terminal: the chord is
        // the key the US keyboard calls F4.
        let mut other = key(keysyms::Cyrillic_ze, Some('з'), chord);
        other.latin_keysym = keysyms::F4;
        assert_eq!(keys.claim_key(&other, false, false), KeyClaim::Take);
        assert_eq!(
            commands.borrow().as_slice(),
            [
                SessionCommand::SwitchVt { vt: 3 },
                SessionCommand::SwitchVt { vt: 4 }
            ]
        );
        // The same keys without Alt, or on a locked session, are not the session's to take.
        let plain = KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        };
        assert_eq!(
            keys.claim_key(&key(keysyms::F3, None, plain), false, false),
            KeyClaim::Pass
        );
        assert_eq!(
            keys.claim_key(&key(keysyms::F3, None, chord), true, false),
            KeyClaim::Pass
        );
        assert_eq!(
            commands.borrow().len(),
            2,
            "only the chords the person pressed left the session"
        );
    }
}
