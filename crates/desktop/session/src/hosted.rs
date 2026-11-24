//! What the session publishes for the quick settings to draw: the Devices section's device
//! fabric rows and sounds, the clipboard's copies and the notification centre.
//!
//! The session holds the device fabric, the clipboard history and the notifications, so it
//! describes each row ([`HostedRow`]) while its section shows, and a press names which of a row's
//! buttons it was ([`RowPart`]).

use ui::icons::Icon;

pub use tilo_session_core::desktop::now_playing::Offer as MediaOffer;

pub use tilo_device_surfaces::devices::{
    ClipboardCopy, DeviceWorkspace, RowPart, SendableSound, SoundAsk, SoundRoute,
};

/// One of the person's devices a Bluetooth accessory can move to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffTarget {
    pub id: String,
    pub label: String,
    pub online: bool,
    /// This device.
    pub local: bool,
}

/// What a row offers besides a press on its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedActions {
    None,
    /// The row's one action, named on a button (stop, cancel, try again).
    Select(String),
    /// Two choices: allow and decline.
    Pair {
        first: (Icon, String),
        second: (Icon, String),
    },
    /// Driving another device with this machine's pointer. Where its
    /// display stands is arranged in its settings pane, which a press on
    /// the row opens.
    Control {
        name: String,
        /// This machine's pointer is driving it now.
        controlling: bool,
        /// It can be driven: online and not this machine.
        controllable: bool,
    },
    /// A copy's pin and remove.
    Clipboard {
        pinned: bool,
        pin: String,
        remove: String,
    },
}

/// One of the person's devices, as the device fabric reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the fabric says of this device now: independent facts"
)]
pub struct PersonalDevice {
    /// The fabric id, which names the device in the panel's effects.
    pub id: String,
    pub label: String,
    /// It has a live session with this device now.
    pub online: bool,
    /// It is heard on this device's local network, so the two reach each
    /// other directly, without a server.
    #[serde(default)]
    pub nearby: bool,
    /// This machine.
    pub local: bool,
    /// It shows, so where its display stands beside this one's is arranged in
    /// its settings pane.
    pub display: bool,
    /// One of its interfaces wakes on a magic packet, so a device on its
    /// network can wake it while it sleeps.
    pub wakeable: bool,
    /// It plays the sound this device sends it.
    pub audio_output: bool,
}

impl PersonalDevice {
    /// The Settings page where this device is managed: where its display is
    /// placed beside this one's when it shows, else the person's devices and
    /// what they keep in step.
    #[must_use]
    pub fn settings_link(&self) -> &'static str {
        if self.display {
            "settings://displays"
        } else {
            "settings://devices_sync"
        }
    }
}

/// The Devices section's rows: one per device other than this one, in
/// `devices`' order. `playing` titled what plays here when another device
/// can take it, and `sending` says a sound of this device's is chosen for
/// one of them: each row that can take it says so, and every other row says
/// only where its device is, since pressing it opens that device's settings.
#[must_use]
pub fn device_rows(
    devices: &[PersonalDevice],
    playing: Option<&str>,
    sending: bool,
) -> Vec<HostedRow> {
    devices
        .iter()
        .filter(|device| !device.local)
        .map(|device| {
            let takes_sound = sending && device.online && device.audio_output;
            HostedRow {
                key: device.id.clone(),
                title: device.label.clone(),
                detail: crate::i18n::t(if takes_sound {
                    "devices-row-send-here"
                } else if device.nearby {
                    "devices-row-nearby"
                } else if device.online {
                    "devices-row-online"
                } else {
                    "devices-row-offline"
                }),
                icon: Icon::Monitor,
                selected: false,
                enabled: true,
                actions: if takes_sound {
                    HostedActions::Select(crate::i18n::t("devices-row-action-send"))
                } else {
                    match playing {
                        Some(title) if device.online => HostedActions::Select(crate::i18n::t_args(
                            "devices-row-play-here",
                            &[("media", title.into())],
                        )),
                        _ if !device.online && device.wakeable => {
                            HostedActions::Select(crate::i18n::t("devices-row-wake"))
                        }
                        _ => HostedActions::None,
                    }
                },
            }
        })
        .collect()
}

/// The row key prefix of one of this device's sounds.
pub const SOUND_KEY: &str = "audio:sound:";
/// The row key prefix of one route of a sound.
pub const ROUTE_KEY: &str = "audio:route:";
/// The row key prefix of one device asking for this device's sound.
pub const ASK_KEY: &str = "audio:ask:";

/// The sounds this device can send, the routes of the sounds it sends or
/// receives, and the devices asking for its sound, as the remote audio
/// service reads them: the rows the Devices section lists above the
/// person's devices.
#[must_use]
pub fn remote_audio_rows(
    sounds: &[SendableSound],
    routes: &[SoundRoute],
    asks: &[SoundAsk],
    chosen: Option<u64>,
) -> Vec<HostedRow> {
    let sounds = sounds.iter().map(|sound| HostedRow {
        key: format!("{SOUND_KEY}{}", sound.token),
        title: sound.label.clone(),
        detail: crate::i18n::t(if sound.application {
            "devices-row-audio-app"
        } else {
            "devices-row-audio-choose"
        }),
        icon: Icon::Volume2,
        selected: chosen == Some(sound.token),
        enabled: true,
        actions: HostedActions::None,
    });
    let asks = asks.iter().map(|ask| HostedRow {
        key: format!("{ASK_KEY}{}", ask.id),
        title: crate::i18n::t_args(
            "devices-row-play-here",
            &[("media", ask.media.as_str().into())],
        ),
        detail: crate::i18n::t_args(
            "devices-row-requested-allow-once",
            &[("source", ask.source.as_str().into())],
        ),
        icon: Icon::Volume2,
        selected: false,
        enabled: true,
        actions: HostedActions::Pair {
            first: (Icon::Check, crate::i18n::t("devices-row-allow-audio")),
            second: (Icon::X, crate::i18n::t("devices-row-decline-audio")),
        },
    });
    let routes = routes.iter().map(|route| HostedRow {
        key: format!("{ROUTE_KEY}{}", route.id),
        title: route.label.clone(),
        detail: route.detail.clone(),
        icon: Icon::Volume2,
        selected: false,
        enabled: true,
        actions: HostedActions::Select(crate::i18n::t(if route.stopped {
            "devices-row-action-dismiss"
        } else {
            "devices-row-action-stop"
        })),
    });
    // What waits for an answer comes first, then what travels, then what can
    // travel: the person answers a device before choosing one.
    asks.chain(routes).chain(sounds).collect()
}

/// The same devices as an accessory's page lists where it can move to, this
/// device included and marked local.
#[must_use]
pub fn handoff_targets(devices: &[PersonalDevice]) -> Vec<HandoffTarget> {
    devices
        .iter()
        .map(|device| HandoffTarget {
            id: device.id.clone(),
            label: device.label.clone(),
            online: device.online,
            local: device.local,
        })
        .collect()
}

/// The Devices section as the session publishes it: the person's devices as
/// the device fabric reads them, what plays here that one of them can take,
/// the sounds the remote audio service sends, receives and is asked for, and
/// the session another of them offers this one.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DevicesReading {
    pub devices: Vec<PersonalDevice>,
    /// What plays here, titled, while media continuity is on: what a press on
    /// another device sends.
    pub playing: Option<String>,
    /// The sound of this device's that a press on a device would send.
    pub chosen: Option<u64>,
    pub sounds: Vec<SendableSound>,
    pub routes: Vec<SoundRoute>,
    pub asks: Vec<SoundAsk>,
    /// What another of the person's devices was playing, until the person
    /// continues it here: the now-playing card offers it.
    pub offer: Option<MediaOffer>,
    /// Whether this device holds a person's certificate: the device is theirs,
    /// with workspaces of theirs to show.
    #[serde(default)]
    pub signed_in: bool,
    /// The workspaces this device's shell lists, by their key and as the
    /// person reads them.
    #[serde(default)]
    pub workspaces: Vec<DeviceWorkspace>,
    /// Why the last login failed, in the words of whoever refused it; empty
    /// when it did not.
    #[serde(default)]
    pub refused: String,
}

impl DevicesReading {
    /// The Devices section's rows: what waits for an answer, what travels,
    /// what can travel, then the person's other devices.
    #[must_use]
    pub fn rows(&self) -> Vec<HostedRow> {
        let mut rows = remote_audio_rows(&self.sounds, &self.routes, &self.asks, self.chosen);
        rows.extend(device_rows(
            &self.devices,
            self.playing.as_deref(),
            self.chosen.is_some(),
        ));
        rows
    }

    /// The same devices as an accessory's page lists where it can move to.
    #[must_use]
    pub fn targets(&self) -> Vec<HandoffTarget> {
        handoff_targets(&self.devices)
    }

    /// How many devices the fabric knows, this one included.
    #[must_use]
    pub fn count(&self) -> usize {
        self.devices.len()
    }
}

/// One row's face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedRow {
    /// Names the row in the panel's effects: what the desktop knows it by.
    pub key: String,
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    /// The row is the chosen one (the media source the volume follows).
    pub selected: bool,
    pub enabled: bool,
    pub actions: HostedActions,
}

/// The notification centre, as the session publishes it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct NotificationCentre {
    /// How many the person has not read.
    pub count: usize,
    /// Why fresh notifications are held (Do Not Disturb, presenting, a focus schedule); `None`
    /// while they show.
    pub quiet: Option<String>,
    /// The notices the centre keeps, newest first.
    #[serde(default)]
    pub notices: Vec<Notice>,
    /// The live banners, newest first, by their ids: what a shell that draws the stack itself shows.
    #[serde(default)]
    pub banners: Vec<u64>,
    /// The notice up as a banner now, by its id.
    #[serde(default)]
    pub banner: Option<u64>,
    /// Whether a Clear all can still be undone, so the centre offers Undo.
    #[serde(default)]
    pub undo: bool,
}

/// A notice the centre keeps.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Notice {
    /// What the verbs name it by.
    pub number: u64,
    pub title: String,
    pub body: String,
    /// The app's name, or the agent's; empty for the session's own.
    pub from: String,
    /// `info`, `success`, `warning` or `error`.
    pub kind: String,
    pub seen: bool,
    /// What its sender offers, in its sender's order.
    pub actions: Vec<NoticeAction>,
}

/// One action a notice offers.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct NoticeAction {
    pub key: String,
    pub label: String,
}
