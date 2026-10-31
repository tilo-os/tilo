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

use crate::i18n::t;
use crate::{MediaOutput, QuickSettingsState, SoundInput, SoundOutput, SoundStream};

/// Land `sound` in the panel's readings: how loud the sound is, whether it
/// is muted, which outputs there are and which one plays, the microphone
/// that records, and every application's stream.
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
    qs.microphone = sound.input.map(|node| SoundInput {
        node,
        // The microphone the service reads, as it names it; a device it
        // names nothing is still the microphone.
        name: sound
            .inputs
            .iter()
            .find(|device| device.node == node)
            .map(|device| device.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| t("qs-microphone")),
        level: sound.input_volume.unwrap_or(0.0),
        muted: sound.input_muted,
    });
    qs.sound_streams = sound
        .streams
        .iter()
        .map(|stream| SoundStream {
            node: stream.node,
            label: stream.label(),
            plays: stream.plays().map(str::to_owned),
            playback: stream.playback,
            target: stream.target,
            level: stream.volume.unwrap_or(0.0),
            muted: stream.muted,
        })
        .collect();
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
            input_volume: Some(0.5),
            input_muted: true,
            outputs: vec![
                Device {
                    node: 40,
                    name: "node-40".into(),
                    label: "Speakers".into(),
                    chosen: true,
                    volume: Some(0.25),
                    muted: true,
                    channels: 2,
                    codec: None,
                },
                Device {
                    node: 41,
                    name: "node-41".into(),
                    label: "Headphones".into(),
                    chosen: false,
                    volume: None,
                    muted: false,
                    channels: 2,
                    codec: None,
                },
            ],
            inputs: vec![Device {
                node: 50,
                name: "node-50".into(),
                label: "Microphone".into(),
                chosen: true,
                volume: None,
                muted: true,
                channels: 1,
                codec: None,
            }],
            streams: vec![audio::sound::Stream {
                node: 60,
                app: "Music".into(),
                media: Some("Clair de lune".into()),
                playback: true,
                target: Some(41),
                volume: Some(0.3),
                muted: false,
            }],
            hardware: Vec::new(),
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
            qs.microphone
                .as_ref()
                .is_some_and(|microphone| microphone.muted),
            "the microphone's state answers its key on screen"
        );
        assert_eq!(
            qs.microphone,
            Some(SoundInput {
                node: 50,
                name: "Microphone".into(),
                level: 0.5,
                muted: true,
            }),
            "the microphone the service records from, with its level"
        );
        assert_eq!(
            qs.sound_streams,
            [SoundStream {
                node: 60,
                label: "Music".into(),
                plays: Some("Clair de lune".into()),
                playback: true,
                target: Some(41),
                level: 0.3,
                muted: false,
            }],
            "the application's stream, and what it plays as its own fact"
        );
    }

    #[test]
    fn a_machine_without_an_output_offers_no_sound_control() {
        let mut qs = QuickSettingsState::unread();
        land(&mut qs, &Sound::default());
        assert!(!qs.volume_available);
        assert_eq!(qs.status.volume, None);
        assert!(qs.sound_outputs.is_empty());
        assert_eq!(qs.microphone, None, "nothing records");
        assert!(qs.sound_streams.is_empty(), "nothing plays");
    }

    #[test]
    fn a_microphone_the_service_names_nothing_is_still_the_microphone() {
        let mut reading = sound();
        reading.inputs.clear();
        let mut qs = QuickSettingsState::unread();
        land(&mut qs, &reading);
        assert_eq!(
            qs.microphone.map(|microphone| microphone.name),
            Some("Microphone".into())
        );
    }
}
