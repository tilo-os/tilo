//! The `BlueZ` client: adapter control, discovery, pairing, trust, removal.
//!
//! Everything goes through `org.bluez` on the **system** bus. Fedora's default D-Bus policy
//! (`/usr/share/dbus-1/system.d/bluetooth.conf`) grants `send_destination="org.bluez"` to
//! every user and `send_interface="org.bluez.Agent1"` to root (`bluetoothd` calling back into
//! our agent), so the whole flow runs in the user's session with no privileged helper.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use tracing::{debug, warn};
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};

use crate::agent::DeviceNames;
use crate::model::{
    AdapterInfo, DeviceInfo, adapter_from_interfaces, device_from_interfaces, sort_devices,
};

const DEST: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";

/// The `a{oa{sa{sv}}}` shape `GetManagedObjects` returns.
type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// A connected `BlueZ` handle. Hold this for as long as discovery should run: `BlueZ` ties a
/// discovery session to the D-Bus connection that started it and stops scanning the moment
/// that connection goes away — a fire-and-forget `StartDiscovery` scans for microseconds.
#[derive(Clone)]
pub struct BluezClient {
    connection: Connection,
    names: DeviceNames,
}

impl BluezClient {
    /// Connect to the system bus.
    ///
    /// # Errors
    ///
    /// Returns an error if the system D-Bus socket can't be reached or the
    /// connection handshake fails.
    pub async fn connect() -> zbus::Result<Self> {
        let connection = Connection::system().await?;
        Ok(Self::with_connection(connection, DeviceNames::new()))
    }

    /// Build on an existing connection — used when the agent and the client must share one
    /// connection so `BlueZ` sees a single client.
    pub fn with_connection(connection: Connection, names: DeviceNames) -> Self {
        Self { connection, names }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The shared display-name cache the pairing agent reads from.
    pub fn names(&self) -> DeviceNames {
        self.names.clone()
    }

    async fn managed_objects(&self) -> zbus::Result<ManagedObjects> {
        let proxy = zbus::Proxy::new(&self.connection, DEST, "/", OBJECT_MANAGER_IFACE).await?;
        let reply = proxy.call_method("GetManagedObjects", &()).await?;
        reply.body().deserialize()
    }

    /// Every adapter `BlueZ` knows about, in path order.
    ///
    /// # Errors
    ///
    /// Returns an error if the `GetManagedObjects` D-Bus call fails (for
    /// example, `bluetoothd` is not running).
    pub async fn adapters(&self) -> zbus::Result<Vec<AdapterInfo>> {
        let objects = self.managed_objects().await?;
        let mut out: Vec<AdapterInfo> = objects
            .iter()
            .filter_map(|(p, ifs)| adapter_from_interfaces(p.as_str(), ifs))
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The adapter to drive. Almost every machine has exactly one; we take the lowest-numbered
    /// so the choice is stable across boots.
    ///
    /// # Errors
    ///
    /// Returns an error if listing adapters ([`Self::adapters`]) fails.
    pub async fn default_adapter(&self) -> zbus::Result<Option<AdapterInfo>> {
        Ok(self.adapters().await?.into_iter().next())
    }

    /// Every device on every adapter, sorted for display, with the name cache refreshed as a
    /// side effect so the pairing agent can title its prompts without calling back into a
    /// waiting daemon.
    ///
    /// # Errors
    ///
    /// Returns an error if the `GetManagedObjects` D-Bus call fails.
    pub async fn devices(&self) -> zbus::Result<Vec<DeviceInfo>> {
        let objects = self.managed_objects().await?;
        let mut out: Vec<DeviceInfo> = objects
            .iter()
            .filter_map(|(p, ifs)| device_from_interfaces(p.as_str(), ifs))
            .collect();
        for d in &out {
            self.names.set(&d.path, &d.name);
        }
        sort_devices(&mut out);
        Ok(out)
    }

    /// Only the devices that have completed pairing.
    ///
    /// # Errors
    ///
    /// Returns an error if listing devices ([`Self::devices`]) fails.
    pub async fn paired_devices(&self) -> zbus::Result<Vec<DeviceInfo>> {
        Ok(self
            .devices()
            .await?
            .into_iter()
            .filter(|d| d.paired)
            .collect())
    }

    async fn adapter_prop<T>(&self, path: &str, name: &str) -> zbus::Result<T>
    where
        T: TryFrom<OwnedValue>,
        T::Error: Into<zbus::Error>,
    {
        let proxy =
            zbus::Proxy::new(&self.connection, DEST, path.to_string(), ADAPTER_IFACE).await?;
        proxy.get_property(name).await
    }

    async fn set_adapter_prop(&self, path: &str, name: &str, value: bool) -> zbus::Result<()> {
        let proxy = zbus::Proxy::new(&self.connection, DEST, path.to_string(), PROPS_IFACE).await?;
        proxy
            .call_method(
                "Set",
                &(ADAPTER_IFACE, name, zbus::zvariant::Value::from(value)),
            )
            .await?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if `adapter` is not a valid object path or `BlueZ`
    /// rejects the `Powered` property write.
    pub async fn set_powered(&self, adapter: &str, on: bool) -> zbus::Result<()> {
        self.set_adapter_prop(adapter, "Powered", on).await
    }

    /// # Errors
    ///
    /// Returns an error if `adapter` is not a valid object path or reading
    /// the `Powered` property fails.
    pub async fn is_powered(&self, adapter: &str) -> zbus::Result<bool> {
        self.adapter_prop(adapter, "Powered").await
    }

    /// Make this machine visible to others. Off by default and never left on — a permanently
    /// discoverable adapter is a standing invitation.
    ///
    /// # Errors
    ///
    /// Returns an error if `adapter` is not a valid object path or `BlueZ`
    /// rejects the `Discoverable` property write.
    pub async fn set_discoverable(&self, adapter: &str, on: bool) -> zbus::Result<()> {
        self.set_adapter_prop(adapter, "Discoverable", on).await
    }

    /// # Errors
    ///
    /// Returns an error if `adapter` is not a valid object path or `BlueZ`
    /// rejects the `Pairable` property write.
    pub async fn set_pairable(&self, adapter: &str, on: bool) -> zbus::Result<()> {
        self.set_adapter_prop(adapter, "Pairable", on).await
    }

    /// Begin scanning. The scan runs until [`Self::stop_discovery`] or until this client's
    /// connection closes.
    ///
    /// # Errors
    ///
    /// Returns an error if powering on the adapter fails, or if
    /// `StartDiscovery` fails for a reason other than a scan already being
    /// in progress.
    pub async fn start_discovery(&self, adapter: &str) -> zbus::Result<()> {
        // Powering on first: StartDiscovery on a powered-off adapter fails with
        // `org.bluez.Error.NotReady`, which reads as "scanning is broken" rather than "Bluetooth is off".
        if !self.is_powered(adapter).await.unwrap_or(true) {
            self.set_powered(adapter, true).await?;
        }
        let proxy =
            zbus::Proxy::new(&self.connection, DEST, adapter.to_string(), ADAPTER_IFACE).await?;
        match proxy.call_method("StartDiscovery", &()).await {
            Ok(_) => Ok(()),
            // Already scanning is success, not an error — two panes asking to scan should not fight.
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.InProgress" =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// # Errors
    ///
    /// Returns an error if `StopDiscovery` fails for a reason other than
    /// there being no scan to stop, or someone else's session owning it.
    pub async fn stop_discovery(&self, adapter: &str) -> zbus::Result<()> {
        let proxy =
            zbus::Proxy::new(&self.connection, DEST, adapter.to_string(), ADAPTER_IFACE).await?;
        match proxy.call_method("StopDiscovery", &()).await {
            Ok(_) => Ok(()),
            // Not scanning, or someone else's session — nothing to undo.
            Err(zbus::Error::MethodError(name, _, _))
                if matches!(
                    name.as_str(),
                    "org.bluez.Error.Failed" | "org.bluez.Error.NotAuthorized"
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// # Errors
    ///
    /// Returns an error if `adapter` is not a valid object path or reading
    /// the `Discovering` property fails.
    pub async fn is_discovering(&self, adapter: &str) -> zbus::Result<bool> {
        self.adapter_prop(adapter, "Discovering").await
    }

    async fn device_proxy(&self, device: &str) -> zbus::Result<zbus::Proxy<'_>> {
        zbus::Proxy::new(&self.connection, DEST, device.to_string(), DEVICE_IFACE).await
    }

    /// Pair with a device. This is the call that drives the agent: `BlueZ` will invoke our
    /// `org.bluez.Agent1` object one or more times before returning, and pairing only completes
    /// if those calls are answered. The reply can take as long as the user takes to compare a
    /// passkey, so this deliberately imposes no short timeout of its own.
    ///
    /// # Errors
    ///
    /// Returns an error if pairing fails for a reason other than the device
    /// already being paired.
    pub async fn pair(&self, device: &str) -> zbus::Result<()> {
        let proxy = self.device_proxy(device).await?;
        match proxy.call_method("Pair", &()).await {
            Ok(_) => Ok(()),
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.AlreadyExists" =>
            {
                debug!(target: "tilo::bluez", device, "already paired");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Abort an in-flight pairing.
    ///
    /// # Errors
    ///
    /// Returns an error if `CancelPairing` fails for a reason other than
    /// there being no pairing in progress.
    pub async fn cancel_pairing(&self, device: &str) -> zbus::Result<()> {
        let proxy = self.device_proxy(device).await?;
        match proxy.call_method("CancelPairing", &()).await {
            Ok(_) => Ok(()),
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.DoesNotExist" =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// # Errors
    ///
    /// Returns an error if `device` is not a valid object path or the
    /// `Connect` call fails.
    pub async fn connect_device(&self, device: &str) -> zbus::Result<()> {
        let proxy = self.device_proxy(device).await?;
        proxy.call_method("Connect", &()).await?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if `device` is not a valid object path or the
    /// `Disconnect` call fails.
    pub async fn disconnect_device(&self, device: &str) -> zbus::Result<()> {
        let proxy = self.device_proxy(device).await?;
        proxy.call_method("Disconnect", &()).await?;
        Ok(())
    }

    /// Mark a paired device trusted. Trust is what lets a keyboard reconnect without the user
    /// re-authorising every profile (`BlueZ` answers `AuthorizeService` itself for trusted
    /// devices) — a real grant, so it's set explicitly after a successful pairing rather than
    /// implied by it.
    ///
    /// # Errors
    ///
    /// Returns an error if `device` is not a valid object path or `BlueZ`
    /// rejects the `Trusted` property write.
    pub async fn set_trusted(&self, device: &str, trusted: bool) -> zbus::Result<()> {
        let proxy =
            zbus::Proxy::new(&self.connection, DEST, device.to_string(), PROPS_IFACE).await?;
        proxy
            .call_method(
                "Set",
                &(
                    DEVICE_IFACE,
                    "Trusted",
                    zbus::zvariant::Value::from(trusted),
                ),
            )
            .await?;
        Ok(())
    }

    /// Set the user-visible Device1 Alias. This is the BlueZ-owned rename,
    /// not a second Settings-side nickname database, so every Bluetooth UI
    /// and application sees the same name.
    ///
    /// # Errors
    ///
    /// Returns an error if `alias` is empty after trimming, `device` is not
    /// a valid object path, or `BlueZ` rejects the `Alias` property write.
    pub async fn set_alias(&self, device: &str, alias: &str) -> zbus::Result<()> {
        let alias = alias.trim();
        if alias.is_empty() {
            return Err(zbus::Error::Failure(
                "A Bluetooth name cannot be empty".into(),
            ));
        }
        let proxy =
            zbus::Proxy::new(&self.connection, DEST, device.to_string(), PROPS_IFACE).await?;
        proxy
            .call_method(
                "Set",
                &(DEVICE_IFACE, "Alias", zbus::zvariant::Value::from(alias)),
            )
            .await?;
        self.names.set(device, alias);
        Ok(())
    }

    /// Forget a device: drops the bond, the keys and the trust. The device must pair again from scratch.
    ///
    /// # Errors
    ///
    /// Returns an error if `device` is not a valid object path, or
    /// `RemoveDevice` fails for a reason other than the device already
    /// being gone.
    pub async fn remove_device(&self, adapter: &str, device: &str) -> zbus::Result<()> {
        let proxy =
            zbus::Proxy::new(&self.connection, DEST, adapter.to_string(), ADAPTER_IFACE).await?;
        let path = ObjectPath::try_from(device.to_string())?;
        match proxy.call_method("RemoveDevice", &(&path,)).await {
            Ok(_) => {
                self.names.remove(device);
                Ok(())
            }
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.DoesNotExist" =>
            {
                self.names.remove(device);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Pair, then connect, then trust — the sequence a user means by "pair this device".
    /// Ordering is deliberate: trust is granted only *after* pairing succeeded, so a rejected
    /// pairing never leaves a trusted entry; connect failures are reported but don't undo the
    /// pairing (plenty of devices pair and then need a button press to connect).
    ///
    /// # Errors
    ///
    /// Returns an error if pairing ([`Self::pair`]) or trusting
    /// ([`Self::set_trusted`]) the device fails. A failure to connect after
    /// a successful pairing is not an error here — it's reported through the
    /// returned [`PairOutcome`].
    pub async fn pair_and_connect(&self, adapter: &str, device: &str) -> zbus::Result<PairOutcome> {
        // Scanning while pairing starves the radio and makes pairing flaky, so stop first — what
        // `bluetoothctl` users are told to do by hand.
        let _ = self.stop_discovery(adapter).await;
        self.pair(device).await?;
        self.set_trusted(device, true).await?;
        match self.connect_device(device).await {
            Ok(()) => Ok(PairOutcome::PairedAndConnected),
            Err(e) => {
                warn!(target: "tilo::bluez", device, error = %e, "paired but could not connect");
                Ok(PairOutcome::PairedNotConnected {
                    reason: error_text(&e),
                })
            }
        }
    }

    /// Watch for device add/remove/property changes, refreshing the name cache, and call
    /// `on_change` whenever the device list may have moved. Runs until the connection drops.
    ///
    /// # Errors
    ///
    /// Returns an error if subscribing to `BlueZ`'s `InterfacesAdded` or
    /// `InterfacesRemoved` signals fails.
    pub async fn watch_devices<F>(&self, mut on_change: F) -> zbus::Result<()>
    where
        F: FnMut(Vec<DeviceInfo>) + Send,
    {
        let proxy = zbus::Proxy::new(&self.connection, DEST, "/", OBJECT_MANAGER_IFACE).await?;
        let mut added = proxy.receive_signal("InterfacesAdded").await?;
        let mut removed = proxy.receive_signal("InterfacesRemoved").await?;
        loop {
            tokio::select! {
                Some(_) = added.next() => {}
                Some(_) = removed.next() => {}
                else => break,
            }
            if let Ok(devices) = self.devices().await {
                on_change(devices);
            }
        }
        Ok(())
    }
}

/// What `pair_and_connect` achieved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairOutcome {
    PairedAndConnected,
    /// Paired successfully, but the link did not come up. Common and recoverable — the row
    /// shows a Connect button.
    PairedNotConnected {
        reason: String,
    },
}

/// Turn a `BlueZ` D-Bus error into something worth putting in front of a person. `BlueZ`'s own
/// messages are terse but accurate, so we keep them and only translate the ones whose wire
/// name says more than the text.
pub fn error_text(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, detail, _) => {
            bluez_error_text(name.as_str(), detail.as_deref())
        }
        other => other.to_string(),
    }
}

/// The mapping itself, over the wire name and detail `BlueZ` sent. Split out so it's exercised
/// directly rather than through a hand-built `zbus::Error` (whose `MethodError` carries a
/// real `Message` that can't be fabricated in a test).
pub fn bluez_error_text(name: &str, detail: Option<&str>) -> String {
    let short = name.rsplit('.').next().unwrap_or(name);
    match short {
        "AuthenticationCanceled" => "Pairing was cancelled.".to_string(),
        "AuthenticationFailed" => "Pairing failed — the passkey did not match.".to_string(),
        "AuthenticationRejected" => "The device refused the pairing.".to_string(),
        "AuthenticationTimeout" => "The device stopped responding.".to_string(),
        "ConnectionAttemptFailed" => {
            "Could not connect — the device may be out of range or already connected to something else."
                .to_string()
        }
        "NotReady" => "Bluetooth is turned off.".to_string(),
        "NotAuthorized" => "Not allowed to control Bluetooth.".to_string(),
        _ => detail.map_or_else(|| format!("Bluetooth error: {short}"), std::string::ToString::to_string),
    }
}

/// How long to leave a scan running before it stops on its own. Discovery costs radio time
/// and battery on every nearby device, so it's not left on indefinitely.
pub const DISCOVERY_WINDOW: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_errors_read_as_english_or_the_daemons_own_message() {
        // Phase `pairing_failures_read_as_english_not_dbus_names`.
        {
            assert_eq!(
                bluez_error_text("org.bluez.Error.AuthenticationFailed", None),
                "Pairing failed — the passkey did not match."
            );
            assert_eq!(
                bluez_error_text("org.bluez.Error.NotReady", None),
                "Bluetooth is turned off."
            );
            assert_eq!(
                bluez_error_text("org.bluez.Error.AuthenticationCanceled", None),
                "Pairing was cancelled."
            );
        }
        // Phase `unmapped_errors_keep_the_daemons_own_message`.
        // An error we have not special-cased still surfaces `BlueZ`'s own detail rather than a bare
        // wire name.
        {
            assert_eq!(
                bluez_error_text(
                    "org.bluez.Error.Failed",
                    Some("br-connection-profile-unavailable")
                ),
                "br-connection-profile-unavailable"
            );
        }
        // Phase `unmapped_errors_without_detail_still_name_something`.
        {
            assert_eq!(
                bluez_error_text("org.bluez.Error.Failed", None),
                "Bluetooth error: Failed"
            );
        }
    }
}
