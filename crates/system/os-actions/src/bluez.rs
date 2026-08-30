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
//! So this module talks to `org.bluez` directly, via `qos-bluez`, and
//! presents a blocking API for callers that are not async.
//!
//! # Who answers the passkey
//!
//! Not this crate, and not the Settings app. The pairing agent is registered
//! by the compositor, because a pairing prompt authorises a device to inject
//! input events and stream audio — precisely the kind of decision an
//! application must not be able to draw. Calling [`pair`] from a settings
//! pane starts the pairing; the prompt that results appears in OS chrome.

use std::sync::OnceLock;
use std::time::Duration;

use qos_bluez::{BluezClient, DeviceInfo};
use tokio::runtime::Runtime;

/// One shared current-thread-pool runtime for every blocking call.
///
/// Built once: a runtime per call would also mean a D-Bus connection per
/// call, and `BlueZ` ties discovery to the connection.
fn runtime() -> Result<&'static Runtime, String> {
    static RT: OnceLock<Option<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
    .ok_or_else(|| "could not start the Bluetooth runtime".to_string())
}

/// The shared client, and with it the connection whose lifetime a scan is
/// bound to.
fn client() -> Result<&'static BluezClient, String> {
    static CLIENT: OnceLock<Option<BluezClient>> = OnceLock::new();
    let rt = runtime()?;
    CLIENT
        .get_or_init(|| rt.block_on(async { BluezClient::connect().await.ok() }))
        .as_ref()
        .ok_or_else(|| "Bluetooth is unavailable (cannot reach org.bluez)".to_string())
}

fn block<F, T>(f: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, zbus::Error>>,
{
    runtime()?
        .block_on(f)
        .map_err(|e| qos_bluez::error_text(&e))
}

/// The adapter every operation here targets.
pub fn adapter_path() -> Result<String, String> {
    let c = client()?;
    block(async { c.default_adapter().await })?
        .map(|a| a.path)
        .ok_or_else(|| "no Bluetooth adapter on this machine".to_string())
}

/// Every device `BlueZ` knows about — paired or merely seen — newest signal
/// first. Unlike the paired-only list, this is what a "nearby devices" view
/// needs.
pub fn devices() -> Result<Vec<DeviceInfo>, String> {
    let c = client()?;
    block(async { c.devices().await })
}

/// Only the devices that completed pairing.
pub fn paired_devices() -> Result<Vec<DeviceInfo>, String> {
    let c = client()?;
    block(async { c.paired_devices().await })
}

/// Begin scanning. Runs until [`stop_scan`], or until the process exits.
pub fn start_scan() -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.start_discovery(&adapter).await })
}

pub fn stop_scan() -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.stop_discovery(&adapter).await })
}

pub fn is_scanning() -> Result<bool, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.is_discovering(&adapter).await })
}

/// Scan for `window`, then stop and return what is in range.
///
/// The bounded form exists because an unbounded scan costs radio time on
/// every device nearby, not just this one.
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
pub fn pair(device_path: &str) -> Result<qos_bluez::PairOutcome, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.pair_and_connect(&adapter, device_path).await })
}

/// Abort a pairing that is in flight.
pub fn cancel_pairing(device_path: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.cancel_pairing(device_path).await })
}

pub fn connect(device_path: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.connect_device(device_path).await })
}

pub fn disconnect(device_path: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.disconnect_device(device_path).await })
}

/// Forget a device: drops the bond, the keys and the trust, so it must pair
/// again from scratch.
pub fn forget(device_path: &str) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.remove_device(&adapter, device_path).await })
}

pub fn set_trusted(device_path: &str, trusted: bool) -> Result<(), String> {
    let c = client()?;
    block(async { c.set_trusted(device_path, trusted).await })
}

/// Rename a device through BlueZ's Device1.Alias property. No parallel local
/// nickname store is maintained.
pub fn set_alias(device_path: &str, alias: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.set_alias(device_path, alias).await })
}

/// Power the controller on or off.
pub fn set_powered(on: bool) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.set_powered(&adapter, on).await })
}

pub fn is_powered() -> Result<bool, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.is_powered(&adapter).await })
}

/// Make this machine visible to others.
///
/// Deliberately not called anywhere by default. A permanently discoverable
/// adapter is a standing invitation, and qOS pairs by scanning *outward*
/// rather than by advertising itself.
pub fn set_discoverable(on: bool) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.set_discoverable(&adapter, on).await })
}
