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

/// What every output's resource id starts with.
const OUTPUT: &str = "sound-output:";

/// The id of the resource that stands for the output at `node`.
#[must_use]
pub fn output_id(node: u32) -> String {
    format!("{OUTPUT}{node}")
}

/// The audio node the resource id `id` names, if it names an output.
#[must_use]
pub fn output_node(id: &str) -> Option<u32> {
    id.strip_prefix(OUTPUT)?.parse().ok()
}

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
    /// Its level from 0 to 1, whether or not muted.
    pub input_volume: Option<f32>,
    pub input_muted: bool,
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    /// Every application's stream, playing or recording.
    pub streams: Vec<Stream>,
    /// The sound cards, and the profiles each offers.
    pub hardware: Vec<Card>,
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
    /// Its `node.name`, the machine name a tone or a meter captures on.
    pub name: String,
    pub label: String,
    /// The one in use.
    pub chosen: bool,
    pub volume: Option<f32>,
    pub muted: bool,
    /// The channels it plays or records: a stereo output takes a balance,
    /// a mono one no speaker test.
    pub channels: usize,
    /// The Bluetooth codec it is on, when it advertises one.
    pub codec: Option<String>,
}

/// One application's stream: what it plays or records, how loud, and where.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Stream {
    /// Its `PipeWire` node id.
    pub node: u32,
    /// The application, as it named itself.
    pub app: String,
    /// What it plays, when the application says.
    pub media: Option<String>,
    /// Whether it plays (`true`) or records.
    pub playback: bool,
    /// The output (or input) it is linked to now.
    pub target: Option<u32>,
    pub volume: Option<f32>,
    pub muted: bool,
}

impl Stream {
    /// What a page calls the stream: the application that owns it.
    #[must_use]
    pub fn label(&self) -> String {
        self.app.clone()
    }

    /// What it plays or records, when the application says: a page shows it
    /// beside the application, as its own fact.
    #[must_use]
    pub fn plays(&self) -> Option<&str> {
        self.media.as_deref().filter(|media| !media.is_empty())
    }
}

/// One sound card (a codec or a Bluetooth transport) and its profiles.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Card {
    /// Its `PipeWire` device id, the handle a profile is chosen by.
    pub node: u32,
    pub label: String,
    /// The index of the profile in use.
    pub profile: Option<u32>,
    pub profiles: Vec<Profile>,
}

/// One profile a sound card offers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Profile {
    pub index: u32,
    pub label: String,
    /// Whether the card offers it here and now.
    pub available: bool,
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
    /// Unmute the microphone when it is muted, and mute it when it is not.
    #[effect(
        own_device,
        label = "Mute or unmute the microphone",
        icon = "mic-off",
        keywords = "microphone, mic mute, mikrofon"
    )]
    ToggleInputMuted,
    /// Set the microphone's level.
    #[effect(
        own_device,
        idempotent,
        label = "Input level",
        icon = "mic",
        keywords = "microphone level, mic gain, eingangspegel"
    )]
    SetInputVolume {
        #[param("Level from 0 to 1")]
        level: f64,
    },
    /// Play (or record) one application's stream through an output (or
    /// input), or release it to follow the default.
    #[effect(
        own_device,
        idempotent,
        label = "Route an application",
        icon = "speaker",
        keywords = "per-app volume, application routing, app stream"
    )]
    RouteStream {
        #[param("The stream's audio node")]
        stream: u32,
        #[param("The output's (or input's) audio node; none follows the default")]
        device: Option<u32>,
    },
    /// Set one application's volume.
    #[effect(
        own_device,
        idempotent,
        label = "Application volume",
        icon = "volume-2",
        keywords = "per-app volume, application level, app stream"
    )]
    SetStreamVolume {
        #[param("The stream's audio node")]
        stream: u32,
        #[param("Level from 0 to 1")]
        level: f64,
    },
    /// Mute or unmute one application.
    #[effect(
        own_device,
        idempotent,
        label = "Mute an application",
        icon = "volume-x",
        keywords = "per-app mute, application mute, app stream"
    )]
    SetStreamMuted {
        #[param("The stream's audio node")]
        stream: u32,
        #[param("Whether the stream is muted")]
        muted: bool,
    },
    /// Use another profile of a sound card (high fidelity, a headset's
    /// microphone).
    #[effect(
        own_device,
        idempotent,
        label = "Audio device profile",
        icon = "speaker",
        keywords = "profile, hfp, a2dp, high fidelity, headset"
    )]
    SetProfile {
        #[param("The sound card's device id")]
        device: u32,
        #[param("The profile's index")]
        profile: u32,
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
        for action in ["set-input-muted", "toggle-input-muted", "set-input-volume"] {
            unavailable(&mut resource, action, "no_input", "There is no sound input");
        }
    }
    if sound.streams.is_empty() {
        for action in ["route-stream", "set-stream-volume", "set-stream-muted"] {
            unavailable(
                &mut resource,
                action,
                "no_streams",
                "No application is playing or recording",
            );
        }
    }
    if !sound
        .hardware
        .iter()
        .any(|card| card.profiles.iter().any(|profile| profile.available))
    {
        unavailable(
            &mut resource,
            "set-profile",
            "no_profiles",
            "No sound card offers a profile",
        );
    }
    let outputs = sound.outputs.iter().map(|device| {
        ResourceSpec::new(
            output_id(device.node),
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
