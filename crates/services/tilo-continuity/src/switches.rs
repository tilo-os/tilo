//! What the person lets of this device continue on their other devices: each facet's switch of
//! their `continuity` settings, under the master switch, each on unless they turned it off. The
//! host's settings watch keeps them current ([`Switches::read`]), and each service reads its own.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use settings_store::continuity_privacy::ContinuityCapability;

/// The person's Continuity switches on this device. Cloning shares them.
#[derive(Clone, Debug, Default)]
pub struct Switches {
    media: Arc<AtomicBool>,
    wifi_settings: Arc<AtomicBool>,
    file_transfer: Arc<AtomicBool>,
    notifications: Arc<AtomicBool>,
    display_input: Arc<AtomicBool>,
}

impl Switches {
    /// Every switch as a person who never touched them has it.
    #[must_use]
    pub fn unset() -> Self {
        let switches = Self::default();
        switches.read(&serde_json::Map::new());
        switches
    }

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
            (&self.wifi_settings, ContinuityCapability::WifiSettings),
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

    /// Whether what plays here may move to the person's other devices.
    #[must_use]
    pub fn media_permitted(&self) -> bool {
        self.media.load(Ordering::Relaxed)
    }

    /// Whether Wi-Fi passphrases and personal settings are shared with the person's devices.
    #[must_use]
    pub fn wifi_settings_permitted(&self) -> bool {
        self.wifi_settings.load(Ordering::Relaxed)
    }

    /// Whether notifications continue on the person's other devices.
    #[must_use]
    pub fn notifications_permitted(&self) -> bool {
        self.notifications.load(Ordering::Relaxed)
    }

    /// Whether the person's other devices may drive this one's keyboard and pointer.
    #[must_use]
    pub fn display_input_permitted(&self) -> bool {
        self.display_input.load(Ordering::Relaxed)
    }

    /// Whether the person lets files they send move to their other devices.
    #[must_use]
    pub fn file_transfer_permitted(&self) -> bool {
        self.file_transfer.load(Ordering::Relaxed)
    }
}
