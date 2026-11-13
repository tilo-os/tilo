//! Blocking Bluetooth operations over `BlueZ`'s D-Bus API: discovery, pairing, bonds. A
//! one-shot setting can shell out, but a scan lives as long as its bus connection and
//! pairing puts a passkey to the person, so these use the client.
//! The pairing agent is the compositor's: approving a device may let it inject input,
//! which an app must not decide.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use tilo_bluez::BluezClient;
pub use tilo_bluez::DeviceInfo;

/// The shared client and the connection a scan is bound to: a connection per call would end discovery.
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

/// The process's one client, for code that needs the connection itself: the compositor serves its
/// pairing agent on it.
pub fn shared_client() -> Result<BluezClient, String> {
    client().cloned()
}

fn block<F, T>(f: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, zbus::Error>>,
{
    crate::block_on(async { f.await.map_err(|e| tilo_bluez::error_text(&e)) })
}

#[derive(Debug)]
pub struct BluetoothFollow {
    task: tilo_rt::JoinHandle<()>,
}

impl Drop for BluetoothFollow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Calls `on_change` on the app runtime whenever `BlueZ` reports a change a Bluetooth page shows, such
/// as headphones that connect by themselves.
pub fn follow_changes(
    on_change: impl Fn() + Send + Sync + 'static,
) -> Result<BluetoothFollow, String> {
    follow_changes_boxed(Box::new(on_change))
}

fn follow_changes_boxed(on_change: Box<dyn Fn() + Send + Sync>) -> Result<BluetoothFollow, String> {
    let runtime = tilo_rt::try_app().map_err(|error| error.to_string())?;
    let task = runtime.spawn(async move {
        let watched = async { BluezClient::connect().await?.watch_changes().await }.await;
        let mut changes = match watched {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(%error, "could not follow Bluetooth's changes");
                return;
            }
        };
        while changes.changed().await.is_some() {
            on_change();
        }
    });
    Ok(BluetoothFollow { task })
}

const BLUETOOTH_CLASS: &str = "/sys/class/bluetooth";

/// Whether this machine has a controller: a desktop without a dongle, and most VMs, have none.
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

pub fn adapter_path() -> Result<String, String> {
    let c = client()?;
    block(async { c.default_adapter().await })?
        .map(|a| a.path)
        .ok_or_else(|| "no Bluetooth adapter on this machine".to_string())
}

pub fn devices() -> Result<Vec<DeviceInfo>, String> {
    let c = client()?;
    block(async { c.devices().await })
}

pub fn paired_devices() -> Result<Vec<DeviceInfo>, String> {
    let c = client()?;
    block(async { c.paired_devices().await })
}

/// Runs until [`stop_scan`] or the process exits.
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

/// Pairs, trusts and connects, which is what a person means by pairing. Blocks while they compare a
/// passkey, so not on a thread that is painting.
pub fn pair(device_path: &str) -> Result<tilo_bluez::PairOutcome, String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.pair_and_connect(&adapter, device_path).await })
}

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

pub fn forget(device_path: &str) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.remove_device(&adapter, device_path).await })
}

pub fn set_trusted(device_path: &str, trusted: bool) -> Result<(), String> {
    let c = client()?;
    block(async { c.set_trusted(device_path, trusted).await })
}

/// Renames through `BlueZ`'s Device1.Alias; there is no local nickname store.
pub fn set_alias(device_path: &str, alias: &str) -> Result<(), String> {
    let c = client()?;
    block(async { c.set_alias(device_path, alias).await })
}

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

/// Makes this machine visible to others. Nothing calls it by default: Tilo pairs by scanning outward.
pub fn set_discoverable(on: bool) -> Result<(), String> {
    let c = client()?;
    let adapter = adapter_path()?;
    block(async { c.set_discoverable(&adapter, on).await })
}

fn unique_device_by_address<'a>(
    devices: &'a [DeviceInfo],
    address: &str,
) -> Result<Option<&'a DeviceInfo>, String> {
    let mut matches = devices
        .iter()
        .filter(|device| device.address.eq_ignore_ascii_case(address));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(format!(
            "Bluetooth address {address} is ambiguous across local adapters"
        ));
    }
    Ok(first)
}

/// Six colon-separated hex octets.
#[must_use]
pub fn valid_address(address: &str) -> bool {
    address.len() == 17
        && address.split(':').count() == 6
        && address
            .split(':')
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Pairs, trusts and connects one local object for an approved handoff address; callers hold their own
/// one-shot local authorization.
pub fn pair_handoff(address: &str) -> Result<(), String> {
    if !valid_address(address) {
        return Err("invalid Bluetooth address".into());
    }
    let devices = devices()?;
    let device = unique_device_by_address(&devices, address)?
        .ok_or_else(|| format!("Bluetooth device {address} is no longer known to BlueZ"))?;
    connected_pair_outcome(pair(&device.path)?)
}

fn connected_pair_outcome(outcome: tilo_bluez::PairOutcome) -> Result<(), String> {
    match outcome {
        tilo_bluez::PairOutcome::PairedAndConnected => Ok(()),
        tilo_bluez::PairOutcome::PairedNotConnected { reason } => Err(format!(
            "Bluetooth pairing completed but the handoff connection failed: {reason}"
        )),
    }
}

/// Forgets the device at `address`: bond, keys and trust go, so it must be paired again.
pub fn forget_address(address: &str) -> Result<(), String> {
    if !valid_address(address) {
        return Err("invalid Bluetooth address".into());
    }
    let devices = devices()?;
    let device = unique_device_by_address(&devices, address)?
        .ok_or_else(|| format!("Bluetooth device {address} is not known to BlueZ"))?;
    forget(&device.path)
}

pub fn connect_address(address: &str) -> Result<(), String> {
    connect(&known_address(address)?.path)
}

pub fn disconnect_address(address: &str) -> Result<(), String> {
    disconnect(&known_address(address)?.path)
}

fn known_address(address: &str) -> Result<DeviceInfo, String> {
    devices()?
        .into_iter()
        .find(|device| device.address.eq_ignore_ascii_case(address))
        .ok_or_else(|| format!("Bluetooth device {address} is not known to BlueZ"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn device(path: &str, address: &str, paired: bool) -> DeviceInfo {
        DeviceInfo {
            path: path.into(),
            address: address.into(),
            paired,
            ..DeviceInfo::default()
        }
    }

    #[test]
    fn handoff_addresses_resolve_uniquely_and_pairing_must_complete() {
        {
            let devices = vec![device(
                "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "AA:BB:CC:DD:EE:FF",
                false,
            )];
            assert_eq!(
                unique_device_by_address(&devices, "aa:bb:cc:dd:ee:ff")
                    .unwrap()
                    .map(|device| device.path.as_str()),
                Some("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF")
            );
        }
        {
            let devices = vec![
                device("/org/bluez/hci0/dev_AA", "AA:BB:CC:DD:EE:FF", true),
                device("/org/bluez/hci1/dev_AA", "AA:BB:CC:DD:EE:FF", false),
            ];
            assert!(
                unique_device_by_address(&devices, "AA:BB:CC:DD:EE:FF")
                    .unwrap_err()
                    .contains("ambiguous")
            );
        }
        {
            assert!(valid_address("AA:BB:CC:DD:EE:FF"));
            assert!(valid_address("aa:bb:cc:dd:ee:ff"));
            assert!(!valid_address("/org/bluez/hci0/dev_AA"));
            assert!(!valid_address("AA:BB:CC:DD:EE"));
            assert!(!valid_address("GG:BB:CC:DD:EE:FF"));
        }
        {
            assert_eq!(
                connected_pair_outcome(tilo_bluez::PairOutcome::PairedAndConnected),
                Ok(())
            );
            assert!(
                connected_pair_outcome(tilo_bluez::PairOutcome::PairedNotConnected {
                    reason: "accessory stayed busy".into(),
                })
                .unwrap_err()
                .contains("accessory stayed busy")
            );
        }
    }

    #[test]
    fn forgetting_a_device_needs_a_real_address() {
        assert!(forget_address("not-an-address").is_err());
        assert!(forget_address("/org/bluez/hci0/dev_AA_BB").is_err());
    }

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
