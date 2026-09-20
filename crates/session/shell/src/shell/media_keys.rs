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

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::shell::osd::{Osd, Transport};
use crate::shell::qs_actions::QsActions;
use crate::shell::status::StatusBridge;

/// One press of a hardware media / brightness key.
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
}

/// Step per press, as a fraction of full scale.
const VOLUME_STEP: f32 = 0.05;
/// Brightness moves in bigger steps than volume: the perceptual range of
/// a backlight is compressed at the top, and 5% steps mean a dozen
/// presses to get from "readable indoors" to "readable in sunlight".
const BRIGHTNESS_STEP: f32 = 0.10;

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
    let Key::Named(named) = &key.logical_key else {
        return None;
    };
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
        _ => return None,
    })
}

/// `status`, when running, is read for the current level (rather than
/// shelling out again) and written back so the tray indicator moves the
/// same frame. `None` when nothing could be done, so the caller shows no
/// OSD rather than claim a change that didn't happen.
pub fn apply(key: MediaKey, actions: &dyn QsActions, status: Option<&StatusBridge>) -> Option<Osd> {
    match key {
        MediaKey::VolumeUp => step_volume(VOLUME_STEP, actions, status),
        MediaKey::VolumeDown => step_volume(-VOLUME_STEP, actions, status),
        MediaKey::VolumeMute => toggle_mute(actions, status),
        MediaKey::MicMute => toggle_mic_mute(),
        MediaKey::BrightnessUp => step_brightness(BRIGHTNESS_STEP, actions),
        MediaKey::BrightnessDown => step_brightness(-BRIGHTNESS_STEP, actions),
        MediaKey::PlayPause => transport(crate::shell::mpris::Command::PlayPause),
        MediaKey::NextTrack => transport(crate::shell::mpris::Command::Next),
        MediaKey::PreviousTrack => transport(crate::shell::mpris::Command::Previous),
    }
}

/// Current sink level + mute, preferring the poller's reading and
/// falling back to asking the daemon directly.
fn current_volume(status: Option<&StatusBridge>) -> Option<os_actions::VolumeState> {
    if let Some(v) = status.and_then(|s| s.snapshot().volume) {
        return Some(v);
    }
    os_actions::get_volume_state().ok()
}

fn step_volume(delta: f32, actions: &dyn QsActions, status: Option<&StatusBridge>) -> Option<Osd> {
    let now = current_volume(status)?;
    let next = (now.level + delta).clamp(0.0, 1.0);
    actions.set_volume(next * 100.0).ok()?;
    // Nudging the volume un-mutes: reaching for volume-up on a muted sink
    // means "let me hear it".
    let muted = if now.muted && delta > 0.0 {
        let _ = actions.set_mute(false);
        false
    } else {
        now.muted
    };
    let published = os_actions::VolumeState { level: next, muted };
    if let Some(s) = status {
        s.set_volume(published);
    }
    Some(Osd::volume(next, muted))
}

fn toggle_mute(actions: &dyn QsActions, status: Option<&StatusBridge>) -> Option<Osd> {
    actions.toggle_mute().ok()?;
    // `wpctl set-mute toggle` reports nothing, so assume the inverse of
    // what we last knew rather than pay a second subprocess.
    let now = current_volume(status);
    let level = now.map_or(0.0, |v| v.level);
    let muted = !now.is_some_and(|v| v.muted);
    if let Some(s) = status {
        s.set_volume(os_actions::VolumeState { level, muted });
    }
    Some(Osd::volume(level, muted))
}

fn toggle_mic_mute() -> Option<Osd> {
    let muted = os_actions::toggle_source_mute().ok()?;
    Some(Osd::mic_mute(muted))
}

fn step_brightness(delta: f32, actions: &dyn QsActions) -> Option<Osd> {
    // No poller: reading sysfs is a file read, not a subprocess, so
    // asking every time is cheap.
    let now = os_actions::get_brightness().ok()?;
    let next = (now + delta).clamp(0.0, 1.0);
    actions.set_brightness(next * 100.0).ok()?;
    Some(Osd::brightness(next))
}

fn transport(cmd: crate::shell::mpris::Command) -> Option<Osd> {
    let outcome = crate::shell::mpris::send(cmd)?;
    Some(Osd::transport(match outcome {
        crate::shell::mpris::Outcome::NowPlaying => Transport::Play,
        crate::shell::mpris::Outcome::NowPaused => Transport::Pause,
        crate::shell::mpris::Outcome::Skipped => match cmd {
            crate::shell::mpris::Command::Next => Transport::Next,
            _ => Transport::Previous,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Build a `KeyboardInput` event for `key` via the zeroed-struct
    /// workaround (winit's `KeyEvent` has a private `platform_specific` field).
    fn press(key: Key) -> WindowEvent {
        key_event(key, ElementState::Pressed)
    }

    fn key_event(key: Key, state: ElementState) -> WindowEvent {
        // SAFETY: every public field is overwritten below, and the one
        // private field tolerates an all-zero bit pattern — the same
        // invariant `native::keyboard_input_event` relies on.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = state;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    /// Recording dispatcher: captures the actuation without touching the
    /// host's hardware.
    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
    }

    impl QsActions for Recorder {
        fn set_brightness(&self, pct: f32) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("brightness={pct:.1}"));
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
            Ok(())
        }
        fn toggle_mute(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("toggle_mute".to_string());
            Ok(())
        }
    }

    fn bridge_at(level: f32, muted: bool) -> StatusBridge {
        let b = StatusBridge::new();
        b.set_volume(os_actions::VolumeState { level, muted });
        b
    }

    #[test]
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

    #[test]
    fn ordinary_keys_are_left_for_the_focused_client() {
        assert_eq!(classify(&press(Key::Named(NamedKey::Enter))), None);
        assert_eq!(
            classify(&press(Key::Character("a".into()))),
            None,
            "typing must never be swallowed as a media key"
        );
    }

    #[test]
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

    #[test]
    fn volume_up_steps_by_five_percent_and_publishes_the_new_level() {
        let rec = Recorder::default();
        let bridge = bridge_at(0.40, false);
        let osd = apply(MediaKey::VolumeUp, &rec, Some(&bridge)).expect("an OSD");
        assert_eq!(rec.calls.borrow().as_slice(), ["volume=45.0"]);
        assert_eq!(osd.caption(), "Volume 45%");
        let published = bridge.snapshot().volume.unwrap().level;
        assert!(
            (published - 0.45).abs() < 1e-5,
            "the tray moves in the same frame, not at the next poll: got {published}"
        );
    }

    #[test]
    fn volume_down_steps_the_other_way_and_stops_at_silence() {
        let rec = Recorder::default();
        let bridge = bridge_at(0.02, false);
        let osd = apply(MediaKey::VolumeDown, &rec, Some(&bridge)).expect("an OSD");
        assert_eq!(rec.calls.borrow().as_slice(), ["volume=0.0"]);
        assert_eq!(osd.caption(), "Volume 0%");
    }

    #[test]
    fn volume_up_never_exceeds_full_scale() {
        let rec = Recorder::default();
        let bridge = bridge_at(0.98, false);
        apply(MediaKey::VolumeUp, &rec, Some(&bridge)).expect("an OSD");
        assert_eq!(rec.calls.borrow().as_slice(), ["volume=100.0"]);
    }

    #[test]
    fn turning_a_muted_sink_up_unmutes_it() {
        let rec = Recorder::default();
        let bridge = bridge_at(0.30, true);
        let osd = apply(MediaKey::VolumeUp, &rec, Some(&bridge)).expect("an OSD");
        assert!(
            rec.calls.borrow().contains(&"mute=false".to_string()),
            "reaching for volume-up on a muted sink means 'let me hear it': {:?}",
            rec.calls.borrow()
        );
        assert_eq!(osd.caption(), "Volume 35%");
        assert!(!bridge.snapshot().volume.unwrap().muted);
    }

    #[test]
    fn turning_a_muted_sink_down_leaves_it_muted() {
        let rec = Recorder::default();
        let bridge = bridge_at(0.30, true);
        let osd = apply(MediaKey::VolumeDown, &rec, Some(&bridge)).expect("an OSD");
        assert!(
            !rec.calls.borrow().contains(&"mute=false".to_string()),
            "turning down is not a request to hear anything"
        );
        assert_eq!(osd.caption(), "Muted");
    }

    #[test]
    fn the_mute_key_flips_the_state_and_says_so() {
        let rec = Recorder::default();
        let bridge = bridge_at(0.50, false);
        let osd = apply(MediaKey::VolumeMute, &rec, Some(&bridge)).expect("an OSD");
        assert_eq!(rec.calls.borrow().as_slice(), ["toggle_mute"]);
        assert_eq!(osd.caption(), "Muted");
        assert!(bridge.snapshot().volume.unwrap().muted);

        let osd = apply(MediaKey::VolumeMute, &rec, Some(&bridge)).expect("an OSD");
        assert_eq!(osd.caption(), "Volume 50%");
        assert!(!bridge.snapshot().volume.unwrap().muted);
    }

    #[test]
    fn a_machine_with_no_audio_daemon_shows_no_readout() {
        let rec = Recorder::default();
        // No bridge reading; this host may have no wpctl in the test sandbox.
        if os_actions::get_volume_state().is_err() {
            assert!(
                apply(MediaKey::VolumeUp, &rec, None).is_none(),
                "no slab may claim a change that did not happen"
            );
            assert!(
                rec.calls.borrow().is_empty(),
                "and nothing is actuated either"
            );
        }
    }
}
