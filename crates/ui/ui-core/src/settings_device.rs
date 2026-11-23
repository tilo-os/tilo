//! The device this process reads its settings on: where its device sections are published
//! (`devices/<device>/settings/power`), and where a view's reading of one is read from.

/// The device this process reads its settings on, when its host said ([`read_settings_on`]).
static DEVICE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Read the settings on `device` (its key in lowercase hex): its sections are published at that
/// device's addresses. The session says so from its identity before it publishes any.
pub fn read_settings_on(device: &str) {
    *DEVICE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(device.to_owned());
}

/// The device this process reads its settings on, by its key in lowercase hex: what its host
/// said ([`read_settings_on`]), else the device the identity service its launcher gave it signs
/// as (an app's), else the one the session left the settings read on for the processes outside
/// it; empty when none is known.
#[must_use]
pub fn settings_device() -> String {
    if let Some(device) = DEVICE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return device;
    }
    #[cfg(feature = "local-daemons")]
    if let Some(device) = launched_on() {
        return device;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(device) = settings_store::outside::device() {
        return device;
    }
    String::new()
}

/// The device whose key signs for this process, when its launcher gave it the workspace's
/// identity service: an app runs on the device that launched it. Asked once; the answer holds
/// for the process's life.
#[cfg(feature = "local-daemons")]
fn launched_on() -> Option<String> {
    static LAUNCHED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    LAUNCHED
        .get_or_init(|| {
            let identity = tilo_identity_client::load().ok()?;
            Some(hex::encode(identity.metadata().signing_public_key()))
        })
        .clone()
}

/// `published`, an address of the settings a view names with the device left open
/// (`devices/{device}/settings/power`), on this process's device.
#[must_use]
pub fn on_this_device(published: &str) -> String {
    published.replace(DEVICE_PLACEHOLDER, &settings_device())
}

/// What stands for the device's key in an address of the settings a view names: the device
/// whose settings are read goes there.
pub const DEVICE_PLACEHOLDER: &str = "{device}";
