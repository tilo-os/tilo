//! The keys this device has and the session answers on it: the levels, the media, power, Print and
//! the virtual terminals, read from the compositor's keysyms, and the apps' shortcuts the portal
//! binds. [`NativeKeys`] reads each press into the session's [`Press`] and hands it to the session's
//! keys; what a device key ran shows its readout once the device answers.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_actions::{ActionAddress, DoorError};
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_desktop::osd::{Device, DeviceGlyph, Hint, Level, OSD_RESOURCE, Osd, OsdKind};
use tilo_session_core::surface::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use ui::host_operation::ResourceOperationStart;
use ui::winit::keyboard::{Key, NamedKey};
use xkeysym::key as keysyms;

use crate::Shared;
use crate::controls::SessionCommand;
use crate::displays;
use crate::doors::plane::answered;
use crate::i18n::t;
use crate::keys::{DeviceKey, Invocation, Press, SessionKeys};
use crate::platform::surfaces::DeskGesture;
use crate::shell::state::WINDOW_MENU;

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
pub(crate) const CAPTURE_STUDIO: &str = tilo_capture_studio::RESOURCE;

/// `XF86XK_RFKill` , which the keysym tables predate: laptops send it for the airplane-mode key;
/// `XF86WLAN` is the older spelling.
pub(crate) const KEY_XF86_RFKILL: u32 = 0x1008_ffb5;

/// The app the calculator key opens.
pub(crate) const CALCULATOR: &str = "so.tilo.calculator";

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

/// A key of the device's: the action it runs, whether holding it repeats it, and whether it works
/// on the lock screen (it changes a level and reveals nothing behind the lock).
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

/// What a device key runs: an action of the session's resources, or Print.
#[derive(Clone, Copy)]
enum HardwareAction {
    Resource {
        resource: &'static str,
        action: &'static str,
    },
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
fn print_invocation(modifiers: tilo_session_core::surface::KeyModifiers) -> Invocation {
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
        _ => return None,
    })
}

/// The device key `key` is, as the session's keys take it: its invocation, with the modifiers on
/// it read.
fn device_key(key: &HostKey) -> Option<DeviceKey> {
    let hardware = classify(key.keysym)?;
    let mut invocation = match hardware.action {
        HardwareAction::Resource { resource, action } => Invocation::new(resource, action),
        HardwareAction::Print => print_invocation(key.modifiers),
    };
    if let Some(app) = hardware.app {
        invocation = invocation.with("app", app);
    }
    if hardware.shift_all && key.modifiers.shift {
        invocation = invocation.with("all", true);
    }
    invocation.readout = hardware.readout;
    Some(DeviceKey {
        invocation,
        repeats: hardware.repeats,
        while_locked: hardware.while_locked,
    })
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

/// The session's keys on this device: each press the compositor offers, read into the session's
/// [`Press`], and the virtual terminals, which only a machine the session holds the keyboard of has.
pub(crate) struct NativeKeys {
    pub(crate) keys: SessionKeys,
    /// The apps' shortcuts, which work wherever the keyboard is.
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    /// The commands a press raises that are the session's own rather than a published action's,
    /// such as leaving for a virtual terminal.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
}

impl NativeKeys {
    pub(crate) fn new(
        keys: SessionKeys,
        portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
        commands: Rc<RefCell<Vec<SessionCommand>>>,
    ) -> Self {
        Self {
            keys,
            portal,
            commands,
        }
    }

    /// `key` as the session's keys read it: the toolkit's key, the device key it is and the app
    /// shortcut bound to it.
    pub(crate) fn press(&self, key: &HostKey) -> Press {
        let held = key.modifiers;
        let under_chord = held.ctrl || held.alt || held.logo;
        // The toolkit names no Meta keysym; it is the desktop's key on keyboards that send it.
        let logical = if matches!(key.keysym, keysyms::Meta_L | keysyms::Meta_R) {
            Key::Named(NamedKey::Meta)
        } else {
            ui::keys::chord(
                key.keysym,
                key.text.as_deref(),
                key.latin_keysym,
                under_chord,
            )
            .0
        };
        Press {
            key: logical,
            bare: ui::keys::from_keysym(key.unmodified_keysym, None).0,
            modifiers: held,
            pressed: key.pressed,
            repeat: key.repeat,
            code: key.keycode,
            device: device_key(key),
            app_shortcut: self.portal.as_ref().and_then(|portal| {
                portal.triggered(
                    key.keycode,
                    tilo_portal::trigger::Modifiers {
                        ctrl: held.ctrl,
                        alt: held.alt,
                        shift: held.shift,
                        logo: held.logo,
                    },
                )
            }),
        }
    }
}

impl SurfaceHost for NativeKeys {
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
        self.keys.claim(&self.press(key), locked, inhibited)
    }

    fn key(&mut self, key: &HostKey) {
        self.keys.answer(&self.press(key));
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

    /// What a service publishes now, read back from `resource`.
    fn published<T: serde::de::DeserializeOwned>(&self, resource: &str) -> Option<T> {
        self.services.borrow().published(resource)
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
