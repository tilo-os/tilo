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
use crate::{QuickSettingsState, SoundInput, SoundOutput, SoundStream};

/// What a person calls an output: a device by the short name it gives
/// itself, and the session's own software sink by a name the panel says in
/// the person's language — the sink is Tilo's, not a device's.
fn output_name(device: &audio::sound::Device) -> String {
    if device.name == media::audio::SESSION_SINK {
        t("qs-sound-session-output")
    } else {
        device.label.clone()
    }
}

/// Land `sound` in the panel's readings: how loud the sound is, whether it
/// is muted, which outputs there are and which one plays, the microphone
/// that records, every application's stream, and who is using the camera or
/// the microphone.
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
            name: output_name(device),
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
    qs.status.media = sound
        .uses
        .iter()
        .map(|use_| os_actions::ActiveMediaUse {
            client_id: use_.client_id,
            pid: use_.pid,
            app_id: use_.app_id.clone(),
            label: use_.label.clone(),
            binary: std::path::PathBuf::from(&use_.binary),
            kind: match use_.kind {
                audio::sound::UseKind::Microphone => os_actions::MediaKind::Microphone,
                audio::sound::UseKind::Camera => os_actions::MediaKind::Camera,
            },
        })
        .collect();
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
            uses: vec![audio::sound::Use {
                client_id: 61,
                pid: 6100,
                app_id: "so.tilo.meet".into(),
                label: "Meet".into(),
                binary: "/usr/bin/meet".into(),
                kind: audio::sound::UseKind::Microphone,
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
        assert_eq!(
            qs.status.media,
            [os_actions::ActiveMediaUse {
                client_id: 61,
                pid: 6100,
                app_id: "so.tilo.meet".into(),
                label: "Meet".into(),
                binary: "/usr/bin/meet".into(),
                kind: os_actions::MediaKind::Microphone,
            }],
            "who is recording, named as the service attributes it"
        );
    }

    /// The session's own software sink carries no device's name, so the
    /// panel says it in the person's language; a device keeps the short name
    /// it gives itself.
    #[test]
    fn an_output_is_named_in_the_persons_language() {
        let mut sound = sound();
        sound.outputs[0].name = media::audio::SESSION_SINK.into();
        sound.outputs[0].label = "Session output".into();
        let mut qs = QuickSettingsState::unread();
        land(&mut qs, &sound);
        assert_eq!(
            qs.sound_outputs[0].name,
            t("qs-sound-session-output"),
            "the session's sink is Tilo's to name"
        );
        assert_eq!(
            qs.sound_outputs[1].name, "Headphones",
            "a device is named as it names itself"
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
        assert!(qs.status.media.is_empty(), "and nothing uses the camera");
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
