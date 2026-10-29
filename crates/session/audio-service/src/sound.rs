//! The `sound` resource: where sound plays and records, how loud, and the
//! actions that change it, for quick settings, Settings, media keys, a
//! script and an agent alike.
//!
//! Each output and input is a child offering to play (or record) through
//! it. Choosing one makes it the default and releases every stream to
//! follow the default, never pinning a stream to the device: a pinned
//! stream stays put when a headset is plugged in later, and `WirePlumber`
//! remembers the pin for the app's next stream.
//!
//! The graph is read with `pw-dump` and changed with `wpctl`, on the
//! service's worker; the reading follows after every change and whenever
//! the policy reports a device arriving or leaving ([`SoundService::refresh`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_resource::Effect;
use tilo_service::{offer, unavailable};

/// The resource's id.
pub const SOUND: &str = "sound";

/// How far a step of the volume moves it.
pub const VOLUME_STEP: f32 = 0.05;

/// What the `sound` resource reads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Sound {
    /// The output sound plays through, by node id.
    pub output: Option<u32>,
    /// Its level from 0 to 1, whether or not muted.
    pub volume: Option<f32>,
    pub muted: bool,
    /// The input sound records from, by node id.
    pub input: Option<u32>,
    pub input_muted: bool,
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    /// Actions taken and not yet answered by the device, by id.
    pub pending: Vec<String>,
    /// Why the last change did not happen, until the next one.
    pub problem: Option<String>,
}

/// One output or input.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Device {
    /// Its `PipeWire` node id.
    pub node: u32,
    pub label: String,
    /// The one in use.
    pub chosen: bool,
    pub volume: Option<f32>,
    pub muted: bool,
}

/// The `sound` resource's actions; each changes this device only.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.audio.sound")]
pub enum SoundAction {
    /// Set the volume.
    #[effect(
        own_device,
        idempotent,
        label = "Volume",
        icon = "volume-2",
        keywords = "sound, loudness, audio level, speaker, lautstärke"
    )]
    SetVolume {
        #[param("Volume from 0 to 1")]
        level: f64,
    },
    /// Turn the volume up by a step.
    #[effect(
        own_device,
        label = "Louder",
        icon = "volume-2",
        keywords = "louder, volume up, lauter"
    )]
    VolumeUp,
    /// Turn the volume down by a step.
    #[effect(
        own_device,
        label = "Quieter",
        icon = "volume-1",
        keywords = "quieter, volume down, leiser"
    )]
    VolumeDown,
    /// Mute or unmute the sound.
    #[effect(own_device, idempotent, icon = "volume-x", keywords = "sound, silence")]
    SetMuted {
        #[param("Whether the sound is muted")]
        muted: bool,
    },
    /// Unmute the sound when it is muted, and mute it when it is not.
    #[effect(
        own_device,
        label = "Mute or unmute",
        icon = "volume-x",
        keywords = "mute, unmute, stumm"
    )]
    ToggleMuted,
    /// Play sound through an output.
    #[effect(
        own_device,
        row,
        idempotent,
        icon = "speaker",
        keywords = "speakers, headphones"
    )]
    RouteOutput {
        #[param("The output's audio node")]
        node: u32,
    },
    /// Record sound from an input.
    #[effect(
        own_device,
        row,
        idempotent,
        icon = "mic",
        keywords = "microphone, input"
    )]
    SelectInput {
        #[param("The input's audio node")]
        node: u32,
    },
    /// Mute or unmute the microphone.
    #[effect(
        own_device,
        idempotent,
        icon = "mic-off",
        keywords = "microphone, mic mute"
    )]
    SetInputMuted {
        #[param("Whether the microphone is muted")]
        muted: bool,
    },
}

/// The sound resource reading `sound`.
#[must_use]
pub fn resource(sound: &Sound) -> ResourceSpec {
    let mut resource =
        offer::<SoundAction>(ResourceSpec::new(SOUND, "so.tilo.audio.sound", "Sound"));
    if sound.output.is_none() {
        for action in [
            "set-volume",
            "volume-up",
            "volume-down",
            "set-muted",
            "toggle-muted",
        ] {
            unavailable(
                &mut resource,
                action,
                "no_output",
                "There is no sound output",
            );
        }
    }
    if sound.input.is_none() {
        unavailable(
            &mut resource,
            "set-input-muted",
            "no_input",
            "There is no sound input",
        );
    }
    let outputs = sound.outputs.iter().map(|device| {
        ResourceSpec::new(
            format!("sound-output:{}", device.node),
            "so.tilo.audio.output",
            device.label.clone(),
        )
        .with_action(SoundAction::RouteOutput { node: device.node }.bound())
    });
    let inputs = sound.inputs.iter().map(|device| {
        ResourceSpec::new(
            format!("sound-input:{}", device.node),
            "so.tilo.audio.input",
            device.label.clone(),
        )
        .with_action(SoundAction::SelectInput { node: device.node }.bound())
    });
    resource.with_children(outputs.chain(inputs))
}

/// The specs a reading published as lines stands for: the session's side
/// of a `tilo-audio --serve` ([`tilo_service::lines::Remote`]).
#[must_use]
pub fn publish(readings: &BTreeMap<String, Value>) -> Vec<ResourceSpec> {
    let sound = readings
        .get(SOUND)
        .and_then(|value| serde_json::from_value::<Sound>(value.clone()).ok())
        .unwrap_or_default();
    vec![resource(&sound)]
}

#[cfg(feature = "service")]
mod service;
#[cfg(feature = "service")]
pub use service::{Graph, SoundService, SystemGraph};
