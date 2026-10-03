//! Blocking Bluetooth operations over `BlueZ`'s D-Bus API.
//!
//! The rest of this crate shells out to command-line tools, which is the
//! right shape for a one-shot setting like screen brightness. Bluetooth is
//! the exception, for two reasons that are properties of `BlueZ` rather than
//! preferences:
//!
//! - **A scan lives as long as its D-Bus connection.** `bluetoothctl scan on`
//!   from a script stops scanning the moment the process exits, so a CLI
//!   wrapper cannot hold a discovery session open while a pane is on screen.
//! - **Pairing is a conversation.** `BlueZ` calls back into a registered agent
//!   object mid-pairing and blocks until it answers. There is no command line
//!   for "the daemon asks you a question".
//!
//! So this module talks to `org.bluez` directly, via `tilo-bluez`, and
//! presents a blocking API for callers that are not async.
//!
//! # Who answers the passkey
//!
//! Not this crate, and not the Settings app. The pairing agent is registered
//! by the compositor, because a pairing prompt authorises a device to inject
//! input events and stream audio — precisely the kind of decision an
//! application must not be able to draw. Calling [`pair`] from a settings
//! pane starts the pairing; the prompt that results appears in OS chrome.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use tilo_bluez::{BluezClient, DeviceInfo};

/// The shared client, and with it the connection whose lifetime a scan is
/// bound to. It is kept once opened: a connection per call would end
/// discovery, which `BlueZ` ties to the connection. The connection's tasks
/// live on the process's app runtime, which outlives every call.
fn client() -> Result<&'static BluezClient, String> {
    static CLIENT: OnceLock<BluezClient> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = crate::block_on(async {
        BluezClient::connect()
            .await
            .map_err(|_| "Bluetooth is unavailable (cannot reach org.bluez)".to_string())
    })?;
    Ok(CLIENT.get_or_init(|| client))
}

/// The process's one `BlueZ` client, for code that needs the connection
/// itself rather than one of the blocking calls here. The compositor
/// serves its pairing agent on it, so a pairing that quick settings or a
/// handoff starts through this module is answered on the connection that
/// asked, and the process never holds a second one.
///
/// Blocking; call it from a worker thread, never from the UI thread or
/// inside an async task.
///
/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus
/// connection is unavailable.
pub fn shared_client() -> Result<BluezClient, String> {
    client().cloned()
}

fn block<F, T>(f: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, zbus::Error>>,
{
    crate::block_on(async { f.await.map_err(|e| tilo_bluez::error_text(&e)) })
}

const BLUETOOTH_CLASS: &str = "/sys/class/bluetooth";

/// Whether this machine has a Bluetooth controller. Desktops without a
/// dongle and most VMs do not; Quick Settings uses this to leave the tile
/// out rather than show a toggle that talks to a missing adapter.
pub fn has_adapter() -> bool {
    has_adapter_in(Path::new(BLUETOOTH_CLASS))
}

fn has_adapter_in(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("hci"))
}

/// The adapter every operation here targets.
///
/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable, the call fails, or no Bluetooth adapter is present.
pub fn adapter_path() -> Result<String, String> {
    let c = client()?;
    block(async { c.default_adapter().await })?
        .map(|a| a.path)
        .ok_or_else(|| "no Bluetooth adapter on this machine".to_string())
}

/// Every device `BlueZ` knows about — paired or merely seen — newest signal
/// first. Unlike the paired-only list, this is what a "nearby devices" view
/// needs.
///
/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the call fails.
pub fn devices() -> Result<Vec<DeviceInfo>, String> {
    let c = client()?;
    block(async { c.devices().await })
}

/// Only the devices that completed pairing.
///
/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the call fails.
pub fn paired_devices() -> Result<Vec<DeviceInfo>, String> {
    let c = client()?;
    block(async { c.paired_devices().await })
}

/// Begin scanning. Runs until [`stop_scan`], or until the process exits.
///
/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the call to start discovery fails.
pub fn start_scan() -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.start_discovery(&adapter).await })
}

/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the call to stop discovery fails.
pub fn stop_scan() -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.stop_discovery(&adapter).await })
}

/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the underlying call fails.
pub fn is_scanning() -> Result<bool, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.is_discovering(&adapter).await })
}

/// Scan for `window`, then stop and return what is in range.
///
/// The bounded form exists because an unbounded scan costs radio time on
/// every device nearby, not just this one.
///
/// # Errors
///
/// Returns an error message if [`start_scan`] or the subsequent [`devices`] call fails.
pub fn scan_for(window: Duration) -> Result<Vec<DeviceInfo>, String> {
    start_scan()?;
    std::thread::sleep(window);
    let found = devices();
    let _ = stop_scan();
    found
}

/// Pair, trust, then connect — what a user means by "pair this device".
///
/// Blocks for as long as the pairing takes, which includes however long the
/// user spends comparing a passkey, so callers must not run this on a thread
/// that is painting.
///
/// # Errors
///
/// Returns an error message if [`adapter_path`] fails, or pairing, trusting, or connecting
/// fails (including a rejected or timed-out passkey).
pub fn pair(device_path: &str) -> Result<tilo_bluez::PairOutcome, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.pair_and_connect(&adapter, device_path).await })
}

/// Abort a pairing that is in flight.
///
/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the call fails.
pub fn cancel_pairing(device_path: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.cancel_pairing(device_path).await })
}

/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the connect call fails.
pub fn connect(device_path: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.connect_device(device_path).await })
}

/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the disconnect call fails.
pub fn disconnect(device_path: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.disconnect_device(device_path).await })
}

/// Forget a device: drops the bond, the keys and the trust, so it must pair
/// again from scratch.
///
/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the call to remove the device fails.
pub fn forget(device_path: &str) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.remove_device(&adapter, device_path).await })
}

/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the call fails.
pub fn set_trusted(device_path: &str, trusted: bool) -> Result<(), String> {
    let c = client()?;
    block(async { c.set_trusted(device_path, trusted).await })
}

/// Rename a device through `BlueZ`'s Device1.Alias property. No parallel local
/// nickname store is maintained.
///
/// # Errors
///
/// Returns an error message if the Bluetooth runtime or `org.bluez` D-Bus connection is
/// unavailable or the call fails.
pub fn set_alias(device_path: &str, alias: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.set_alias(device_path, alias).await })
}

/// Power the controller on or off.
///
/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the call to set powered state fails.
pub fn set_powered(on: bool) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.set_powered(&adapter, on).await })
}

/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the underlying call fails.
pub fn is_powered() -> Result<bool, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.is_powered(&adapter).await })
}

/// Make this machine visible to others.
///
/// Deliberately not called anywhere by default. A permanently discoverable
/// adapter is a standing invitation, and Tilo pairs by scanning *outward*
/// rather than by advertising itself.
///
/// # Errors
///
/// Returns an error message if [`adapter_path`] fails or the call to set discoverable state
/// fails.
pub fn set_discoverable(on: bool) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.set_discoverable(&adapter, on).await })
}

#[cfg(test)]
mod tests {
    use super::has_adapter_in;
    use std::fs;

    #[test]
    fn has_adapter_requires_an_hci_controller() {
        let empty = tempfile::tempdir().unwrap();
        assert!(!has_adapter_in(empty.path()));
        let with_hci = tempfile::tempdir().unwrap();
        fs::create_dir(with_hci.path().join("hci0")).unwrap();
        assert!(has_adapter_in(with_hci.path()));
        let noise = tempfile::tempdir().unwrap();
        fs::create_dir(noise.path().join("rfkill")).unwrap();
        assert!(!has_adapter_in(noise.path()));
    }
}
