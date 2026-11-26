//! The person's certified devices, as a role reaches them: a role keeps the shared link and asks
//! it what the person's other devices said, sends one of them an action and shares what this device
//! has with them. The device's daemon holds the endpoint, so the session keeps no link of its own:
//! it acts on what its options hand it, and the daemon serves what the person's devices do for
//! each other.
//!
//! Who a device is, the stamp a fact carries and the fresh ids that name an exchange are portable
//! ([`tilo_network_actor::fabric`]); what a device says of itself is kept in the person's record of
//! their devices, which their daemon holds, and is read from there
//! ([`tilo_speaker_service::devices`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use settings_store::continuity_privacy::ContinuityCapability;

pub use tilo_network_actor::fabric::{DeviceId, FabricStamp, nonzero};

/// What a device is: its [`DeviceAbout`], the fact it keeps under its own key in the person's
/// record of their devices.
pub(crate) const ABOUT: &str = tilo_network_actor::fabric::ABOUT_FACT;

pub use tilo_network_actor::fabric::clock;
pub use tilo_network_actor::fabric::link::FabricLink as PeerLink;
pub use tilo_network_actor::fabric::link::PersonalFabricSendError;
pub use tilo_network_actor::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector,
};
pub use tilo_network_actor::fabric::{
    DeviceAbout, DeviceKind, FabricCapabilities, FabricError, MAX_FABRIC_ACTION_AGE_MS,
    MediaSourceLocator, PersonalFabricAction, PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS,
    REMOTE_AUDIO_FRAME_SAMPLES, REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ,
};
pub use tilo_speaker_service::devices::SharedLink;
pub use tilo_speaker_service::devices::Speakers as FabricLink;

/// What the person lets of this device continue on their other devices: each
/// facet's switch of their `continuity` settings, under the master switch,
/// each on unless they turned it off. The session's settings watch keeps them
/// current ([`Continuity::read`]).
#[derive(Clone, Debug, Default)]
pub struct Continuity {
    media: Arc<AtomicBool>,
    file_transfer: Arc<AtomicBool>,
    notifications: Arc<AtomicBool>,
    display_input: Arc<AtomicBool>,
}

impl Continuity {
    /// Take the switches from a settings document, as Settings writes it.
    pub fn read(&self, document: &serde_json::Map<String, serde_json::Value>) {
        let section = document.get("continuity");
        let on = |name: &str| match section {
            None => settings_store::continuity_privacy::ON_UNLESS_SET,
            Some(serde_json::Value::Object(section)) => section
                .get(name)
                .map_or(settings_store::continuity_privacy::ON_UNLESS_SET, |value| {
                    value.as_bool() == Some(true)
                }),
            Some(_) => false,
        };
        let enabled = on("enabled");
        for (switch, facet) in [
            (&self.media, ContinuityCapability::Media),
            (&self.file_transfer, ContinuityCapability::FileTransfer),
            (&self.notifications, ContinuityCapability::Notifications),
        ] {
            switch.store(enabled && on(facet.key()), Ordering::Relaxed);
        }
        // This device takes the keyboard and pointer of the person's other devices only with
        // Display & input on and its automatic grants left on, as Settings words both.
        self.display_input.store(
            enabled
                && on(ContinuityCapability::DisplayInput.key())
                && on("automatic_display_input"),
            Ordering::Relaxed,
        );
    }

    /// Whether the person lets notifications continue on their other devices.
    pub(crate) fn notifications_permitted(&self) -> bool {
        self.notifications.load(Ordering::Relaxed)
    }

    /// Whether the person's other devices may drive this one's keyboard and pointer.
    pub(crate) fn display_input_permitted(&self) -> bool {
        self.display_input.load(Ordering::Relaxed)
    }

    /// Whether the person lets files they send move to their other devices.
    pub(crate) fn file_transfer_permitted(&self) -> bool {
        self.file_transfer.load(Ordering::Relaxed)
    }
}

/// What this device says of itself that lasts, as its first session writes it: what the person
/// calls it, and what it does for their other devices. Input, display and speaker groups are not
/// offered here, so they are not claimed.
pub(crate) fn about(label: &str) -> DeviceAbout {
    DeviceAbout {
        label: label.to_owned(),
        kind: DeviceKind::Desktop,
        capabilities: FabricCapabilities {
            // What the person copies here reaches their other devices, and what they play here can
            // go on playing on them.
            clipboard: true,
            audio_output: true,
            video_output: true,
            ..FabricCapabilities::default()
        },
    }
}

/// The value `hex` holds, postcard in hex, when it reads as a `T`.
pub(crate) fn decoded<T: serde::de::DeserializeOwned>(hex: &str) -> Option<T> {
    tilo_failure::ignored!(
        "a value no build of this person's wrote is not one of their devices' to read",
        postcard::from_bytes(&hex::decode(hex).ok()?).ok()
    )
}
