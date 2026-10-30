//! The panel's sound, as the audio service reads it.
//!
//! One process owns the audio graph: `tilo-audio` reads it, publishes the
//! `sound` resource, and every change (the level, mute, which output plays,
//! whether the microphone records) is one of that resource's actions. The
//! panel shows what the service read and asks it for what the person does,
//! so the graph has one writer and the panel never disagrees with it.
//!
//! A session whose desktop has the sound (a nested session in another
//! desktop's window) shows no sound controls rather than a second copy of
//! the graph.

use audio::sound::Sound;

use crate::{MediaOutput, QuickSettingsState, SoundOutput};

/// Land `sound` in the panel's readings: how loud the sound is, whether it
/// is muted, which outputs there are and which one plays.
pub(crate) fn land(qs: &mut QuickSettingsState, sound: &Sound) {
    qs.volume_available = sound.output.is_some();
    qs.status.volume = sound
        .output
        .map(|_| media::audio::default_node::VolumeState {
            level: sound.volume.unwrap_or(0.0),
            muted: sound.muted,
        });
    qs.sound_outputs = sound
        .outputs
        .iter()
        .map(|device| SoundOutput {
            node: device.node,
            name: device.label.clone(),
            current: device.chosen,
        })
        .collect();
    qs.microphone_muted = sound.input_muted;
}

/// Where what plays can go: this machine's outputs, each named by the audio
/// service's own resource for it, so the card's choice is that device.
pub(crate) fn destinations(sound: &Sound) -> Vec<MediaOutput> {
    sound
        .outputs
        .iter()
        .map(|device| MediaOutput {
            token: audio::sound::output_id(device.node),
            label: device.label.clone(),
            current: device.chosen,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use audio::sound::{Device, Sound};

    use super::*;

    fn sound() -> Sound {
        Sound {
            output: Some(40),
            volume: Some(0.25),
            muted: true,
            input: Some(50),
            input_muted: true,
            outputs: vec![
                Device {
                    node: 40,
                    label: "Speakers".into(),
                    chosen: true,
                    volume: Some(0.25),
                    muted: true,
                },
                Device {
                    node: 41,
                    label: "Headphones".into(),
                    chosen: false,
                    volume: None,
                    muted: false,
                },
            ],
            inputs: vec![Device {
                node: 50,
                label: "Microphone".into(),
                chosen: true,
                volume: None,
                muted: true,
            }],
            pending: Vec::new(),
            problem: None,
        }
    }

    #[test]
    fn the_services_reading_is_the_panels() {
        let mut qs = QuickSettingsState::unread();
        land(&mut qs, &sound());
        assert!(qs.volume_available);
        assert_eq!(qs.volume_level(), 0.25);
        assert!(qs.volume_muted());
        assert_eq!(
            qs.sound_outputs,
            [
                SoundOutput {
                    node: 40,
                    name: "Speakers".into(),
                    current: true,
                },
                SoundOutput {
                    node: 41,
                    name: "Headphones".into(),
                    current: false,
                },
            ]
        );
        assert!(
            qs.microphone_muted,
            "the microphone's state answers its key on screen"
        );
    }

    #[test]
    fn a_machine_without_an_output_offers_no_sound_control() {
        let mut qs = QuickSettingsState::unread();
        land(&mut qs, &Sound::default());
        assert!(!qs.volume_available);
        assert_eq!(qs.status.volume, None);
        assert!(qs.sound_outputs.is_empty());
    }
}
