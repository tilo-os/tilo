//! The hardware keys along the top of every laptop keyboard.
//!
//! This module is the classification and the actuation, kept out of the
//! backend loops so both of them route the same press to the same
//! outcome. The loop's job is one `else if`.
//!
//! # Volume steps
//!
//! 5% per press. Two presses to move a noticeable amount, twenty from
//! silent to full — the range every desktop converges on, and small
//! enough that holding the key (which autorepeats) ramps smoothly rather
//! than jumping.
//!
//! # Keys without a named spelling
//!
//! winit has no [`NamedKey`] for the keyboard backlight, wireless,
//! display-switch or touchpad keys; both backends deliver them as the raw
//! XKB keysym in [`NativeKey::Xkb`], which is what [`classify`] matches.

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey, NativeKey};
use xkeysym::key as keysyms;

use crate::shell::osd::{Osd, Transport};
use crate::shell::quick_settings::actions::QsActions;
use crate::shell::status::StatusBridge;

/// `XF86XK_RFKill`, which the `xkbcommon` bindings predate. Laptops send it
/// for the airplane-mode key; `XF86WLAN` is the older Wi-Fi-key spelling.
const KEY_XF86_RFKILL: u32 = 0x1008_ffb5;

/// One press of a hardware media / brightness / machine key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MicMute,
    BrightnessUp,
    BrightnessDown,
    PlayPause,
    NextTrack,
    PreviousTrack,
    Stop,
    Sleep,
    KeyboardBacklightUp,
    KeyboardBacklightDown,
    KeyboardBacklightToggle,
    /// The airplane-mode (rfkill / WLAN) key.
    Wireless,
    /// The display-switch key (a monitor glyph on the function row).
    DisplayMode,
    Calculator,
    TouchpadToggle,
}

impl MediaKey {
    /// Whether this key keeps working on the lock screen. Volume,
    /// brightness, backlight and playback adjust the machine and reveal
    /// nothing behind the lock. Keys that open UI or launch apps
    /// (display modes, calculator), change connectivity or input devices,
    /// or suspend stay captured by the lock.
    pub fn acts_while_locked(self) -> bool {
        match self {
            Self::VolumeUp
            | Self::VolumeDown
            | Self::VolumeMute
            | Self::MicMute
            | Self::BrightnessUp
            | Self::BrightnessDown
            | Self::PlayPause
            | Self::NextTrack
            | Self::PreviousTrack
            | Self::Stop
            | Self::KeyboardBacklightUp
            | Self::KeyboardBacklightDown
            | Self::KeyboardBacklightToggle => true,
            Self::Sleep
            | Self::Wireless
            | Self::DisplayMode
            | Self::Calculator
            | Self::TouchpadToggle => false,
        }
    }
}

/// What one press asks of the session beyond what [`apply`] actuated
/// itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Response {
    /// Raise this readout.
    Osd(Osd),
    /// Suspend the machine.
    Suspend,
    /// Open the display-mode and projection picker.
    OpenDisplayModes,
    /// Open this installed app.
    Launch {
        package_id: &'static str,
        display_name: &'static str,
    },
}

/// The sink and source writes behind the volume, mute and mic-mute keys.
///
/// The session's implementation queues them on a thread of their own
/// (`audio_writer::AudioWriter`): `wpctl` can block on `PipeWire` for
/// seconds, and these keys are pressed on the compositor thread.
pub trait AudioControl {
    /// Set the default sink to `pct` percent.
    ///
    /// # Errors
    ///
    /// The daemon's error, where the write is not merely queued.
    fn set_volume(&self, pct: f32) -> Result<(), String>;
    /// Set the default sink's mute flag.
    ///
    /// # Errors
    ///
    /// The daemon's error, where the write is not merely queued.
    fn set_mute(&self, muted: bool) -> Result<(), String>;
    /// Flip the default sink's mute flag.
    ///
    /// # Errors
    ///
    /// The daemon's error, where the write is not merely queued.
    fn toggle_mute(&self) -> Result<(), String>;
    /// Flip the default source's mute flag. Its readout is raised through
    /// `readout` once the resulting state is known.
    ///
    /// # Errors
    ///
    /// When this control cannot reach the microphone at all.
    fn toggle_source_mute(
        &self,
        _readout: &crate::session_runtime::Delivery<Osd>,
    ) -> Result<(), String> {
        Err("no microphone control".into())
    }
}

/// Audio writes made synchronously through a Quick Settings dispatcher,
/// as the volume slider makes them.
pub struct DispatcherAudio<'a>(pub &'a dyn QsActions);

impl AudioControl for DispatcherAudio<'_> {
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.0.set_volume(pct)
    }

    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.0.set_mute(muted)
    }

    fn toggle_mute(&self) -> Result<(), String> {
        self.0.toggle_mute()
    }
}

/// Everything [`apply`] may read or drive.
pub struct Context<'a> {
    pub actions: &'a dyn QsActions,
    /// Volume, mute and mic-mute writes.
    pub audio: &'a dyn AudioControl,
    /// Read for the current level and written back so the tray indicator
    /// moves the same frame. Without a reading the volume keys do nothing:
    /// asking the daemon here would stall the compositor thread.
    pub status: Option<&'a StatusBridge>,
    /// Where a late readout is raised: a media player's answer to a
    /// transport key, or the microphone's state after the mic-mute key.
    pub transport_osd: &'a crate::session_runtime::Delivery<Osd>,
    /// Airplane mode as Quick Settings last showed it; the wireless key
    /// flips exactly what the Quick Settings toggle would.
    pub airplane_mode: bool,
    /// Brightness keys adjust the monitor under the pointer.
    pub brightness: BrightnessKeys<'a>,
}

/// What a brightness key acts on.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrightnessKeys<'a> {
    /// Every monitor with a brightness control and its cached level.
    pub displays: &'a [os_actions::MonitorLevel],
    /// Connector of the monitor under the pointer, when known.
    pub under_pointer: Option<&'a str>,
}

/// Step per press, as a fraction of full scale.
const VOLUME_STEP: f32 = 0.05;
/// Brightness moves in bigger steps than volume: the perceptual range of
/// a backlight is compressed at the top, and 5% steps mean a dozen
/// presses to get from "readable indoors" to "readable in sunlight".
const BRIGHTNESS_STEP: f32 = 0.10;

/// The calculator the launch key opens.
const CALCULATOR: (&str, &str) = ("so.tilo.calculator", "Calculator");

/// Only key *presses* count. These keys autorepeat, and a repeat is a
/// genuine second press — holding volume-down to fade out is the expected
/// gesture — so repeats are not filtered here.
pub fn classify(event: &WindowEvent) -> Option<MediaKey> {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return None;
    };
    if key.state != ElementState::Pressed {
        return None;
    }
    match &key.logical_key {
        Key::Named(named) => classify_named(*named),
        Key::Unidentified(NativeKey::Xkb(raw)) => classify_keysym(*raw),
        _ => None,
    }
}

fn classify_named(named: NamedKey) -> Option<MediaKey> {
    Some(match named {
        NamedKey::AudioVolumeUp => MediaKey::VolumeUp,
        NamedKey::AudioVolumeDown => MediaKey::VolumeDown,
        NamedKey::AudioVolumeMute => MediaKey::VolumeMute,
        NamedKey::MicrophoneVolumeMute => MediaKey::MicMute,
        NamedKey::BrightnessUp => MediaKey::BrightnessUp,
        NamedKey::BrightnessDown => MediaKey::BrightnessDown,
        NamedKey::MediaPlayPause | NamedKey::MediaPlay | NamedKey::MediaPause => {
            MediaKey::PlayPause
        }
        NamedKey::MediaTrackNext => MediaKey::NextTrack,
        NamedKey::MediaTrackPrevious => MediaKey::PreviousTrack,
        NamedKey::MediaStop => MediaKey::Stop,
        NamedKey::Standby => MediaKey::Sleep,
        NamedKey::LaunchApplication2 => MediaKey::Calculator,
        _ => return None,
    })
}

fn classify_keysym(raw: u32) -> Option<MediaKey> {
    Some(match raw {
        keysyms::XF86_KbdBrightnessUp => MediaKey::KeyboardBacklightUp,
        keysyms::XF86_KbdBrightnessDown => MediaKey::KeyboardBacklightDown,
        keysyms::XF86_KbdLightOnOff => MediaKey::KeyboardBacklightToggle,
        keysyms::XF86_WLAN | KEY_XF86_RFKILL => MediaKey::Wireless,
        keysyms::XF86_Display => MediaKey::DisplayMode,
        keysyms::XF86_TouchpadToggle => MediaKey::TouchpadToggle,
        _ => return None,
    })
}

/// `None` when nothing could be done, so the caller shows no OSD rather
/// than claim a change that didn't happen.
///
/// Transport keys address a media player over D-Bus, which the caller must
/// not wait on: they return `None` at once, and their OSD arrives later
/// through `transport_osd` if the player answers.
pub fn apply(key: MediaKey, cx: &Context<'_>) -> Option<Response> {
    use crate::shell::mpris::Command;
    let osd = match key {
        MediaKey::VolumeUp => step_volume(VOLUME_STEP, cx.audio, cx.status),
        MediaKey::VolumeDown => step_volume(-VOLUME_STEP, cx.audio, cx.status),
        MediaKey::VolumeMute => toggle_mute(cx.audio, cx.status),
        MediaKey::MicMute => toggle_mic_mute(cx.audio, cx.transport_osd),
        MediaKey::BrightnessUp => step_brightness(BRIGHTNESS_STEP, cx.actions, &cx.brightness),
        MediaKey::BrightnessDown => step_brightness(-BRIGHTNESS_STEP, cx.actions, &cx.brightness),
        MediaKey::PlayPause => transport(Command::PlayPause, cx.transport_osd),
        MediaKey::NextTrack => transport(Command::Next, cx.transport_osd),
        MediaKey::PreviousTrack => transport(Command::Previous, cx.transport_osd),
        MediaKey::Stop => transport(Command::Stop, cx.transport_osd),
        MediaKey::KeyboardBacklightUp => step_keyboard_backlight(true, cx.actions),
        MediaKey::KeyboardBacklightDown => step_keyboard_backlight(false, cx.actions),
        MediaKey::KeyboardBacklightToggle => toggle_keyboard_backlight(cx.actions),
        MediaKey::Wireless => toggle_airplane_mode(cx.airplane_mode, cx.actions),
        MediaKey::TouchpadToggle => toggle_touchpad(),
        MediaKey::Sleep => return Some(Response::Suspend),
        MediaKey::DisplayMode => return Some(Response::OpenDisplayModes),
        MediaKey::Calculator => {
            return Some(Response::Launch {
                package_id: CALCULATOR.0,
                display_name: CALCULATOR.1,
            });
        }
    };
    osd.map(Response::Osd)
}

/// Current sink level + mute, as the status poller last read it. Never the
/// daemon itself: the keys run on the compositor thread, and a key pressed
/// before the first reading shows nothing rather than freeze the desktop.
fn current_volume(status: Option<&StatusBridge>) -> Option<os_actions::VolumeState> {
    status.and_then(|s| s.snapshot().volume)
}

fn step_volume(delta: f32, audio: &dyn AudioControl, status: Option<&StatusBridge>) -> Option<Osd> {
    let now = current_volume(status)?;
    let next = (now.level + delta).clamp(0.0, 1.0);
    commit_volume(audio, status, Some(now), next)
        .ok()
        .map(|(_, osd)| osd)
}

/// Set the default sink to `level` (`0.0..=1.0`) — the one path behind the
/// volume keys and the Quick Settings slider, so both answer the same
/// change with the same sink state, tray reading and readout.
///
/// `previous` is the state the change starts from. A change that does not
/// turn the level down, and leaves it audible, un-mutes: reaching for more
/// volume on a muted sink means "let me hear it", while turning down is not
/// a request to hear anything. The tray is updated only from a known
/// previous state and only once every write succeeded, so a failure never
/// publishes a state the sink is not in.
///
/// # Errors
///
/// The daemon's error when setting the level or un-muting failed.
pub(crate) fn commit_volume(
    audio: &dyn AudioControl,
    status: Option<&StatusBridge>,
    previous: Option<os_actions::VolumeState>,
    level: f32,
) -> Result<(os_actions::VolumeState, Osd), String> {
    let level = level.clamp(0.0, 1.0);
    audio.set_volume(level * 100.0)?;
    let was_muted = previous.is_some_and(|now| now.muted);
    let unmute = previous.is_some_and(|now| level > 0.0 && level >= now.level);
    if was_muted && unmute {
        audio.set_mute(false)?;
    }
    let committed = os_actions::VolumeState {
        level,
        muted: was_muted && !unmute,
    };
    if let (Some(status), Some(_)) = (status, previous) {
        status.set_volume(committed);
    }
    Ok((committed, Osd::volume(level, committed.muted)))
}

fn toggle_mute(audio: &dyn AudioControl, status: Option<&StatusBridge>) -> Option<Osd> {
    // Read before actuating: a reading taken after the toggle could observe
    // the new state and invert it a second time.
    let now = current_volume(status)?;
    audio.toggle_mute().ok()?;
    // `wpctl set-mute toggle` reports nothing, so assume the inverse of
    // what we last knew rather than pay a second subprocess.
    let level = now.level;
    let muted = !now.muted;
    if let Some(s) = status {
        s.set_volume(os_actions::VolumeState { level, muted });
    }
    Some(Osd::volume(level, muted))
}

/// The microphone's state is not polled, so its readout arrives once the
/// flip has happened and reported where it ended: `None` now, the slab
/// through `readout` a moment later.
fn toggle_mic_mute(
    audio: &dyn AudioControl,
    readout: &crate::session_runtime::Delivery<Osd>,
) -> Option<Osd> {
    if let Err(error) = audio.toggle_source_mute(readout) {
        tracing::warn!(target: "tilo::os_actions", %error, "mic mute key unavailable");
    }
    None
}

fn step_brightness(delta: f32, actions: &dyn QsActions, keys: &BrightnessKeys<'_>) -> Option<Osd> {
    let target = keys
        .under_pointer
        .and_then(|connector| keys.displays.iter().find(|d| d.connector == connector))
        .or_else(|| keys.displays.first());
    if let Some(display) = target {
        // Cached levels: a DDC/CI read takes tens of milliseconds, and a
        // held key repeats faster than that.
        let next = (display.level + delta).clamp(0.0, 1.0);
        actions
            .set_display_brightness(&display.connector, next * 100.0)
            .ok()?;
        return Some(Osd::brightness(next));
    }
    // No poller: reading sysfs is a file read, not a subprocess, so
    // asking every time is cheap. A machine with nothing to read — an
    // external monitor, a desktop, a VM — says so instead of ignoring
    // the key.
    let Ok(now) = actions.brightness() else {
        return Some(Osd::no_backlight());
    };
    let next = (now + delta).clamp(0.0, 1.0);
    actions.set_brightness(next * 100.0).ok()?;
    Some(Osd::brightness(next))
}

fn step_keyboard_backlight(up: bool, actions: &dyn QsActions) -> Option<Osd> {
    let now = actions.keyboard_backlight().ok()?;
    set_keyboard_backlight(now, now.step(up), actions)
}

fn toggle_keyboard_backlight(actions: &dyn QsActions) -> Option<Osd> {
    let now = actions.keyboard_backlight().ok()?;
    set_keyboard_backlight(now, now.toggled(), actions)
}

fn set_keyboard_backlight(
    now: os_actions::KeyboardBacklight,
    level: u32,
    actions: &dyn QsActions,
) -> Option<Osd> {
    actions.set_keyboard_backlight(level).ok()?;
    Some(Osd::keyboard_backlight(
        os_actions::KeyboardBacklight { level, ..now }.fraction(),
    ))
}

fn toggle_airplane_mode(on: bool, actions: &dyn QsActions) -> Option<Osd> {
    let next = !on;
    actions.set_airplane_mode(next).ok()?;
    Some(Osd::airplane_mode(next))
}

/// Flip the touchpad preference in the session's settings. The compositor
/// owns libinput and applies the change on its next settings poll, the
/// same path a Settings-pane toggle takes. The flip reads and writes under
/// the settings lock, so two quick presses flip twice.
fn toggle_touchpad() -> Option<Osd> {
    let origin = settings_store::ChangeOrigin::local(TOUCHPAD_TOGGLE_ORIGIN);
    match settings_store::hardware::TOUCHPAD_ENABLED.update_by(&origin, |enabled| !enabled) {
        Ok(next) => Some(Osd::touchpad(next)),
        Err(error) => {
            tracing::warn!(%error, "touchpad toggle could not be saved");
            None
        }
    }
}

/// The change origin the touchpad-toggle hardware key records.
const TOUCHPAD_TOGGLE_ORIGIN: &str = "media-key.touchpad-toggle";
/// The change origin the mouse-keys chord records: its fixed-chord id.
const MOUSE_KEYS_ORIGIN: &str = "fixed-toggle-mouse-keys";

/// Flip mouse keys in the session's settings, for the Shift+Alt+Num Lock
/// chord. Like the touchpad key, the compositor picks the change up on its
/// next settings poll, the path a Settings-pane toggle takes.
pub fn toggle_mouse_keys() -> Option<Osd> {
    let origin = settings_store::ChangeOrigin::local(MOUSE_KEYS_ORIGIN);
    match settings_store::hardware::MOUSE_KEYS.update_by(&origin, |enabled| !enabled) {
        Ok(next) => Some(Osd::mouse_keys(next)),
        Err(error) => {
            tracing::warn!(%error, "mouse keys toggle could not be saved");
            None
        }
    }
}

fn transport(
    cmd: crate::shell::mpris::Command,
    transport_osd: &crate::session_runtime::Delivery<Osd>,
) -> Option<Osd> {
    let delivery = transport_osd.clone();
    crate::shell::mpris::send(cmd, move |outcome| {
        delivery.deliver(transport_outcome_osd(cmd, outcome));
    });
    None
}

/// The OSD for a transport command the player accepted.
fn transport_outcome_osd(
    cmd: crate::shell::mpris::Command,
    outcome: crate::shell::mpris::Outcome,
) -> Osd {
    Osd::transport(match outcome {
        crate::shell::mpris::Outcome::NowPlaying => Transport::Play,
        crate::shell::mpris::Outcome::NowPaused => Transport::Pause,
        crate::shell::mpris::Outcome::Stopped => Transport::Stop,
        crate::shell::mpris::Outcome::Skipped => match cmd {
            crate::shell::mpris::Command::Next => Transport::Next,
            _ => Transport::Previous,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// `apply` for the keys these tests press, which never reach a player.
    fn apply_local(
        key: MediaKey,
        actions: &Recorder,
        status: Option<&StatusBridge>,
    ) -> Option<Osd> {
        match respond(key, actions, status, false) {
            Some(Response::Osd(osd)) => Some(osd),
            None => None,
            Some(other) => panic!("{key:?} answered {other:?}, not a readout"),
        }
    }

    fn respond(
        key: MediaKey,
        actions: &Recorder,
        status: Option<&StatusBridge>,
        airplane_mode: bool,
    ) -> Option<Response> {
        let (transport_osd, _mailbox) = crate::session_runtime::mailbox(std::sync::Arc::new(|| {}));
        apply(
            key,
            &Context {
                actions,
                audio: &DispatcherAudio(actions),
                status,
                transport_osd: &transport_osd,
                airplane_mode,
                brightness: BrightnessKeys::default(),
            },
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn media_keys_are_classified_on_press_and_transport_osd_names_the_state() {
        fn a_transport_osd_names_the_resulting_state() {
            use crate::shell::mpris::{Command, Outcome};
            use crate::shell::osd::OsdKind;
            assert_eq!(
                transport_outcome_osd(Command::PlayPause, Outcome::NowPaused).kind,
                OsdKind::Transport(Transport::Pause)
            );
            assert_eq!(
                transport_outcome_osd(Command::PlayPause, Outcome::NowPlaying).kind,
                OsdKind::Transport(Transport::Play)
            );
            assert_eq!(
                transport_outcome_osd(Command::Next, Outcome::Skipped).kind,
                OsdKind::Transport(Transport::Next)
            );
            assert_eq!(
                transport_outcome_osd(Command::Previous, Outcome::Skipped).kind,
                OsdKind::Transport(Transport::Previous)
            );
            assert_eq!(
                transport_outcome_osd(Command::Stop, Outcome::Stopped).kind,
                OsdKind::Transport(Transport::Stop)
            );
        }

        fn every_hardware_key_is_classified() {
            let cases = [
                (NamedKey::AudioVolumeUp, MediaKey::VolumeUp),
                (NamedKey::AudioVolumeDown, MediaKey::VolumeDown),
                (NamedKey::AudioVolumeMute, MediaKey::VolumeMute),
                (NamedKey::MicrophoneVolumeMute, MediaKey::MicMute),
                (NamedKey::BrightnessUp, MediaKey::BrightnessUp),
                (NamedKey::BrightnessDown, MediaKey::BrightnessDown),
                (NamedKey::MediaPlayPause, MediaKey::PlayPause),
                (NamedKey::MediaTrackNext, MediaKey::NextTrack),
                (NamedKey::MediaTrackPrevious, MediaKey::PreviousTrack),
            ];
            for (named, want) in cases {
                assert_eq!(
                    classify(&press(Key::Named(named))),
                    Some(want),
                    "{named:?} must reach a handler"
                );
            }
        }

        fn ordinary_keys_are_left_for_the_focused_client() {
            assert_eq!(classify(&press(Key::Named(NamedKey::Enter))), None);
            assert_eq!(
                classify(&press(Key::Character("a".into()))),
                None,
                "typing must never be swallowed as a media key"
            );
        }

        fn a_key_release_is_not_a_press() {
            assert_eq!(
                classify(&key_event(
                    Key::Named(NamedKey::AudioVolumeUp),
                    ElementState::Released
                )),
                None,
                "acting on both edges would double every step"
            );
        }

        fn every_machine_key_is_classified() {
            let named = [
                (NamedKey::MediaStop, MediaKey::Stop),
                (NamedKey::Standby, MediaKey::Sleep),
                (NamedKey::LaunchApplication2, MediaKey::Calculator),
            ];
            for (named, want) in named {
                assert_eq!(
                    classify(&press(Key::Named(named))),
                    Some(want),
                    "{named:?} must reach a handler"
                );
            }
            let raw = [
                (keysyms::XF86_KbdBrightnessUp, MediaKey::KeyboardBacklightUp),
                (
                    keysyms::XF86_KbdBrightnessDown,
                    MediaKey::KeyboardBacklightDown,
                ),
                (
                    keysyms::XF86_KbdLightOnOff,
                    MediaKey::KeyboardBacklightToggle,
                ),
                (keysyms::XF86_WLAN, MediaKey::Wireless),
                (KEY_XF86_RFKILL, MediaKey::Wireless),
                (keysyms::XF86_Display, MediaKey::DisplayMode),
                (keysyms::XF86_TouchpadToggle, MediaKey::TouchpadToggle),
            ];
            for (keysym, want) in raw {
                assert_eq!(
                    classify(&press_keysym(keysym)),
                    Some(want),
                    "keysym {keysym:#x} must reach a handler"
                );
            }
            assert_eq!(
                classify(&press_keysym(0x1008_ff00 + 0xee)),
                None,
                "an unknown vendor key is left for the client"
            );
        }

        a_transport_osd_names_the_resulting_state();
        every_hardware_key_is_classified();
        ordinary_keys_are_left_for_the_focused_client();
        a_key_release_is_not_a_press();
        every_machine_key_is_classified();
    }

    fn press(key: Key) -> WindowEvent {
        key_event(key, ElementState::Pressed)
    }

    fn press_keysym(raw: u32) -> WindowEvent {
        press(Key::Unidentified(NativeKey::Xkb(raw)))
    }

    fn key_event(key: Key, state: ElementState) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(key, None, state))
    }

    /// Recording dispatcher: captures the actuation without touching the
    /// host's hardware.
    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        fail_unmute: Cell<bool>,
        bridge_on_toggle: RefCell<Option<StatusBridge>>,
        /// `None`: the machine has no display backlight.
        brightness: Cell<Option<f32>>,
        /// `None`: the machine has no keyboard backlight.
        keyboard: Cell<Option<os_actions::KeyboardBacklight>>,
    }

    impl QsActions for Recorder {
        fn set_brightness(&self, pct: f32) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("brightness={pct:.1}"));
            Ok(())
        }
        fn set_display_brightness(&self, connector: &str, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("brightness:{connector}={pct:.1}"));
            Ok(())
        }
        fn set_volume(&self, pct: f32) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("volume={pct:.1}"));
            Ok(())
        }
        fn set_wifi(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
        fn set_mute(&self, muted: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("mute={muted}"));
            if !muted && self.fail_unmute.get() {
                return Err("unmute failed".into());
            }
            Ok(())
        }
        fn toggle_mute(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("toggle_mute".to_string());
            if let Some(bridge) = self.bridge_on_toggle.borrow().as_ref() {
                let before = bridge.snapshot().volume.expect("seeded volume");
                bridge.set_volume(os_actions::VolumeState {
                    level: before.level,
                    muted: !before.muted,
                });
            }
            Ok(())
        }
        fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("airplane={enabled}"));
            Ok(())
        }
        fn brightness(&self) -> Result<f32, String> {
            self.brightness.get().ok_or_else(|| "no backlight".into())
        }
        fn keyboard_backlight(&self) -> Result<os_actions::KeyboardBacklight, String> {
            self.keyboard
                .get()
                .ok_or_else(|| "no keyboard backlight".into())
        }
        fn set_keyboard_backlight(&self, level: u32) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("keyboard={level}"));
            Ok(())
        }
    }

    fn bridge_at(level: f32, muted: bool) -> StatusBridge {
        let b = StatusBridge::new();
        b.set_volume(os_actions::VolumeState { level, muted });
        b
    }

    #[test]
    fn brightness_keys_without_a_backlight_say_so() {
        let rec = Recorder::default();
        for key in [MediaKey::BrightnessUp, MediaKey::BrightnessDown] {
            let osd = apply_local(key, &rec, None).expect("an OSD, not silence");
            assert_eq!(osd.caption(), "No adjustable display");
        }
        assert!(
            rec.calls.borrow().is_empty(),
            "nothing is written to a backlight that is not there"
        );
    }

    #[test]
    fn brightness_keys_adjust_the_monitor_under_the_pointer() {
        let displays = [
            os_actions::MonitorLevel {
                connector: "eDP-1".into(),
                level: 0.5,
            },
            os_actions::MonitorLevel {
                connector: "DP-2".into(),
                level: 0.3,
            },
        ];
        let rec = Recorder::default();
        let (transport_osd, _mailbox) = crate::session_runtime::mailbox(std::sync::Arc::new(|| {}));
        let press = |key, under_pointer| {
            apply(
                key,
                &Context {
                    actions: &rec,
                    audio: &DispatcherAudio(&rec),
                    status: None,
                    transport_osd: &transport_osd,
                    airplane_mode: false,
                    brightness: BrightnessKeys {
                        displays: &displays,
                        under_pointer,
                    },
                },
            )
        };
        let Some(Response::Osd(osd)) = press(MediaKey::BrightnessUp, Some("DP-2")) else {
            panic!("an OSD for the adjusted monitor");
        };
        assert_eq!(osd.caption(), "Brightness 40%");
        // A pointer over a monitor without a control adjusts the first.
        press(MediaKey::BrightnessDown, Some("HDMI-A-1"));
        assert_eq!(
            *rec.calls.borrow(),
            ["brightness:DP-2=40.0", "brightness:eDP-1=40.0"]
        );
    }

    #[test]
    fn brightness_steps_the_backlight_it_can_read() {
        let rec = Recorder::default();
        rec.brightness.set(Some(0.5));
        let osd = apply_local(MediaKey::BrightnessUp, &rec, None).expect("an OSD");
        assert_eq!(rec.calls.borrow().as_slice(), ["brightness=60.0"]);
        assert_eq!(osd.caption(), "Brightness 60%");
    }

    #[test]
    fn keyboard_backlight_keys_step_and_toggle_the_light() {
        let rec = Recorder::default();
        rec.keyboard
            .set(Some(os_actions::KeyboardBacklight { level: 1, max: 2 }));
        let up = apply_local(MediaKey::KeyboardBacklightUp, &rec, None).expect("an OSD");
        assert_eq!(up.caption(), "Keyboard backlight 100%");
        let down = apply_local(MediaKey::KeyboardBacklightDown, &rec, None).expect("an OSD");
        assert_eq!(down.caption(), "Keyboard backlight off");
        let toggle = apply_local(MediaKey::KeyboardBacklightToggle, &rec, None).expect("an OSD");
        assert_eq!(toggle.caption(), "Keyboard backlight off");
        assert_eq!(
            rec.calls.borrow().as_slice(),
            ["keyboard=2", "keyboard=0", "keyboard=0"]
        );
    }

    #[test]
    fn a_machine_without_a_keyboard_light_ignores_its_keys() {
        let rec = Recorder::default();
        assert!(apply_local(MediaKey::KeyboardBacklightUp, &rec, None).is_none());
        assert!(rec.calls.borrow().is_empty());
    }

    #[test]
    fn the_wireless_key_flips_airplane_mode_through_quick_settings() {
        let rec = Recorder::default();
        let on = respond(MediaKey::Wireless, &rec, None, false);
        assert!(
            matches!(on, Some(Response::Osd(osd)) if osd.caption() == "Airplane mode on"),
            "{on:?}"
        );
        let off = respond(MediaKey::Wireless, &rec, None, true);
        assert!(
            matches!(off, Some(Response::Osd(osd)) if osd.caption() == "Airplane mode off"),
            "{off:?}"
        );
        assert_eq!(
            rec.calls.borrow().as_slice(),
            ["airplane=true", "airplane=false"]
        );
    }

    /// Run `test` with the session's records in `dir` as the settings,
    /// as the shell runs.
    fn with_session_records(test: impl FnOnce(&settings_store::SessionPaths)) {
        let dir = tempfile::tempdir().unwrap();
        let paths = settings_store::SessionPaths::beside(dir.path().join("settings.json"));
        let records = settings_store::SessionSettings::open(paths.clone()).unwrap();
        let _settings = crate::shell::test_support::install_settings(std::sync::Arc::new(records));
        test(&paths);
    }

    #[test]
    fn the_touchpad_key_flips_the_preference_in_the_devices_record() {
        with_session_records(|paths| {
            let rec = Recorder::default();
            let caption = |r: Option<Response>| match r {
                Some(Response::Osd(osd)) => osd.caption(),
                other => panic!("expected a readout, got {other:?}"),
            };
            assert_eq!(
                caption(respond(MediaKey::TouchpadToggle, &rec, None, false)),
                "Touchpad off",
                "an absent preference means the touchpad was on"
            );
            assert_eq!(
                settings_store::load_at(&paths.device)["input"]
                    [settings_store::hardware::TOUCHPAD_ENABLED_KEY],
                false
            );
            assert_eq!(
                settings_store::load_at(&paths.copy)["input"]
                    [settings_store::hardware::TOUCHPAD_ENABLED_KEY],
                false,
                "the compositor reads the copy"
            );
            assert_eq!(
                caption(respond(MediaKey::TouchpadToggle, &rec, None, false)),
                "Touchpad on"
            );
        });
    }

    #[test]
    fn the_mouse_keys_chord_flips_the_preference_in_the_records() {
        with_session_records(|paths| {
            let on = toggle_mouse_keys().expect("saved");
            assert_eq!(on.caption(), "Mouse keys on", "absent means off");
            assert_eq!(
                settings_store::load_section("accessibility").get("mouse_keys"),
                Some(&serde_json::Value::Bool(true))
            );
            assert_eq!(
                settings_store::load_at(&paths.copy)["accessibility"]["mouse_keys"],
                true
            );
            let off = toggle_mouse_keys().expect("saved");
            assert_eq!(off.caption(), "Mouse keys off");
            let log = std::fs::read_to_string(paths.change_log()).expect("change log");
            let entries: Vec<serde_json::Value> = log
                .lines()
                .map(|line| serde_json::from_str(line).expect("one JSON entry per line"))
                .collect();
            assert_eq!(entries.len(), 2, "one entry per flip: {entries:?}");
            for (entry, after) in entries.iter().zip([true, false]) {
                assert_eq!(entry["section"], "accessibility");
                assert_eq!(entry["leaf"], "mouse_keys");
                assert_eq!(entry["after"], after);
                assert_eq!(entry["action"], MOUSE_KEYS_ORIGIN, "the chord is named");
                assert_eq!(
                    entry["principal"],
                    settings_store::ChangeOrigin::LOCAL_USER,
                    "pressed at this device's keyboard"
                );
            }
        });
    }

    #[test]
    fn session_keys_hand_their_intent_to_the_compositor() {
        let rec = Recorder::default();
        assert_eq!(
            respond(MediaKey::Sleep, &rec, None, false),
            Some(Response::Suspend)
        );
        assert_eq!(
            respond(MediaKey::DisplayMode, &rec, None, false),
            Some(Response::OpenDisplayModes)
        );
        assert_eq!(
            respond(MediaKey::Calculator, &rec, None, false),
            Some(Response::Launch {
                package_id: "so.tilo.calculator",
                display_name: "Calculator",
            })
        );
        assert!(rec.calls.borrow().is_empty());
    }

    #[test]
    fn volume_keys_step_clamp_and_unmute_only_when_it_succeeds() {
        fn volume_up_steps_by_five_percent_and_publishes_the_new_level() {
            let rec = Recorder::default();
            let bridge = bridge_at(0.40, false);
            let osd = apply_local(MediaKey::VolumeUp, &rec, Some(&bridge)).expect("an OSD");
            assert_eq!(rec.calls.borrow().as_slice(), ["volume=45.0"]);
            assert_eq!(osd.caption(), "Volume 45%");
            let published = bridge.snapshot().volume.unwrap().level;
            assert!(
                (published - 0.45).abs() < 1e-5,
                "the tray moves in the same frame, not at the next poll: got {published}"
            );
        }

        fn volume_down_steps_the_other_way_and_stops_at_silence() {
            let rec = Recorder::default();
            let bridge = bridge_at(0.02, false);
            let osd = apply_local(MediaKey::VolumeDown, &rec, Some(&bridge)).expect("an OSD");
            assert_eq!(rec.calls.borrow().as_slice(), ["volume=0.0"]);
            assert_eq!(osd.caption(), "Volume 0%");
        }

        fn volume_up_never_exceeds_full_scale() {
            let rec = Recorder::default();
            let bridge = bridge_at(0.98, false);
            apply_local(MediaKey::VolumeUp, &rec, Some(&bridge)).expect("an OSD");
            assert_eq!(rec.calls.borrow().as_slice(), ["volume=100.0"]);
        }

        fn turning_a_muted_sink_up_unmutes_it() {
            let rec = Recorder::default();
            let bridge = bridge_at(0.30, true);
            let osd = apply_local(MediaKey::VolumeUp, &rec, Some(&bridge)).expect("an OSD");
            assert!(
                rec.calls.borrow().contains(&"mute=false".to_string()),
                "reaching for volume-up on a muted sink means 'let me hear it': {:?}",
                rec.calls.borrow()
            );
            assert_eq!(osd.caption(), "Volume 35%");
            assert!(!bridge.snapshot().volume.unwrap().muted);
        }

        fn volume_up_does_not_claim_audio_when_unmute_fails() {
            let rec = Recorder::default();
            rec.fail_unmute.set(true);
            let bridge = bridge_at(0.30, true);

            assert!(apply_local(MediaKey::VolumeUp, &rec, Some(&bridge)).is_none());
            assert_eq!(rec.calls.borrow().as_slice(), ["volume=35.0", "mute=false"]);
            assert!(
                bridge.snapshot().volume.unwrap().muted,
                "a failed unmute must not publish an audible state"
            );
        }

        fn turning_a_muted_sink_down_leaves_it_muted() {
            let rec = Recorder::default();
            let bridge = bridge_at(0.30, true);
            let osd = apply_local(MediaKey::VolumeDown, &rec, Some(&bridge)).expect("an OSD");
            assert!(
                !rec.calls.borrow().contains(&"mute=false".to_string()),
                "turning down is not a request to hear anything"
            );
            assert_eq!(osd.caption(), "Muted");
        }

        volume_up_steps_by_five_percent_and_publishes_the_new_level();
        volume_down_steps_the_other_way_and_stops_at_silence();
        volume_up_never_exceeds_full_scale();
        turning_a_muted_sink_up_unmutes_it();
        volume_up_does_not_claim_audio_when_unmute_fails();
        turning_a_muted_sink_down_leaves_it_muted();
    }

    #[test]
    fn mute_key_flips_the_prior_state_and_no_daemon_shows_nothing() {
        fn the_mute_key_flips_the_state_and_says_so() {
            let rec = Recorder::default();
            let bridge = bridge_at(0.50, false);
            let osd = apply_local(MediaKey::VolumeMute, &rec, Some(&bridge)).expect("an OSD");
            assert_eq!(rec.calls.borrow().as_slice(), ["toggle_mute"]);
            assert_eq!(osd.caption(), "Muted");
            assert!(bridge.snapshot().volume.unwrap().muted);

            let osd = apply_local(MediaKey::VolumeMute, &rec, Some(&bridge)).expect("an OSD");
            assert_eq!(osd.caption(), "Volume 50%");
            assert!(!bridge.snapshot().volume.unwrap().muted);
        }

        fn mute_uses_the_state_from_before_actuation() {
            let bridge = bridge_at(0.50, false);
            let rec = Recorder::default();
            *rec.bridge_on_toggle.borrow_mut() = Some(bridge.clone());

            let osd = apply_local(MediaKey::VolumeMute, &rec, Some(&bridge)).expect("an OSD");

            assert_eq!(osd.caption(), "Muted");
            assert!(
                bridge.snapshot().volume.unwrap().muted,
                "a post-toggle read must not be inverted a second time"
            );
        }

        fn a_machine_with_no_audio_daemon_shows_no_readout() {
            let rec = Recorder::default();
            // No bridge reading; this host may have no wpctl in the test sandbox.
            if os_actions::get_volume_state().is_err() {
                assert!(
                    apply_local(MediaKey::VolumeUp, &rec, None).is_none(),
                    "no slab may claim a change that did not happen"
                );
                assert!(
                    rec.calls.borrow().is_empty(),
                    "and nothing is actuated either"
                );
            }
        }

        the_mute_key_flips_the_state_and_says_so();
        mute_uses_the_state_from_before_actuation();
        a_machine_with_no_audio_daemon_shows_no_readout();
    }

    /// An audio control that counts every write it is asked for.
    #[derive(Default)]
    struct Untouchable {
        writes: Cell<usize>,
    }

    impl Untouchable {
        fn refuse(&self) -> Result<(), String> {
            self.writes.set(self.writes.get().saturating_add(1));
            Err("no write without a known starting level".into())
        }
    }

    impl AudioControl for Untouchable {
        fn set_volume(&self, _: f32) -> Result<(), String> {
            self.refuse()
        }
        fn set_mute(&self, _: bool) -> Result<(), String> {
            self.refuse()
        }
        fn toggle_mute(&self) -> Result<(), String> {
            self.refuse()
        }
    }

    #[test]
    fn volume_key_without_snapshot_does_not_query_daemon() {
        let rec = Recorder::default();
        let audio = Untouchable::default();
        let unread = StatusBridge::new();
        let (transport_osd, _mailbox) = crate::session_runtime::mailbox(std::sync::Arc::new(|| {}));
        for key in [
            MediaKey::VolumeUp,
            MediaKey::VolumeDown,
            MediaKey::VolumeMute,
        ] {
            for status in [None, Some(&unread)] {
                let response = apply(
                    key,
                    &Context {
                        actions: &rec,
                        audio: &audio,
                        status,
                        transport_osd: &transport_osd,
                        airplane_mode: false,
                        brightness: BrightnessKeys::default(),
                    },
                );
                assert_eq!(
                    response, None,
                    "{key:?} with no reading shows nothing and asks nobody"
                );
            }
        }
        assert!(rec.calls.borrow().is_empty());
        assert_eq!(audio.writes.get(), 0, "no write is even attempted");
        assert_eq!(
            unread.snapshot().volume,
            None,
            "nothing is invented for the tray"
        );
    }
}
