//! The `BlueZ` client: adapter control, discovery, pairing, trust, removal.
//!
//! Everything goes through `org.bluez` on the **system** bus. Fedora's default D-Bus policy
//! (`/usr/share/dbus-1/system.d/bluetooth.conf`) grants `send_destination="org.bluez"` to
//! every user and `send_interface="org.bluez.Agent1"` to root (`bluetoothd` calling back into
//! our agent), so the whole flow runs in the user's session with no privileged helper.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::sync::watch;
use tracing::{debug, warn};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule, MessageStream};

use crate::agent::DeviceNames;
use crate::model::{
    AdapterInfo, DeviceInfo, adapter_from_interfaces, device_from_interfaces, sort_devices,
};

const DEST: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
const BATTERY_IFACE: &str = "org.bluez.Battery1";
/// A device's properties a person sees change: its name and kind, whether
/// it is paired, trusted, blocked or connected. Its signal strength, which
/// moves all through discovery, is not among them.
const SHOWN_DEVICE: &[&str] = &[
    "Alias",
    "Name",
    "Icon",
    "Paired",
    "Bonded",
    "Trusted",
    "Blocked",
    "Connected",
];
/// An adapter's properties a person sees change.
const SHOWN_ADAPTER: &[&str] = &["Powered", "Discovering", "Alias"];
/// Signals buffered per stream before the oldest is dropped. Every signal
/// only moves a count a reader re-reads on, so a dropped one loses nothing.
const SIGNAL_QUEUE: usize = 64;

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

    /// Follow what a Bluetooth page shows: a device or an adapter changing
    /// a property a person sees ([`SHOWN_DEVICE`], [`SHOWN_ADAPTER`], a
    /// battery's level), a device appearing or going, and `BlueZ` leaving or
    /// joining the bus. The returned watch counts them; a reader reads the
    /// adapter again when the count moves. Nothing polls.
    ///
    /// Must be called inside a tokio runtime; the follower runs there until
    /// every clone of the watch is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the signal subscriptions cannot be made.
    pub async fn watch_changes(&self) -> zbus::Result<BluezChanges> {
        let properties = MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(DEST)?
                .interface(PROPS_IFACE)?
                .member("PropertiesChanged")?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?;
        let objects = MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(DEST)?
                .interface(OBJECT_MANAGER_IFACE)?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?;
        let owners = MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")?
                .path("/org/freedesktop/DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .arg(0, DEST)?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?;
        let (sender, receiver) = watch::channel(0);
        tokio::spawn(follow_changes(
            properties,
            futures_util::stream::select(objects, owners),
            sender,
        ));
        Ok(BluezChanges { receiver })
    }

    /// Every adapter `BlueZ` knows about, in path order.
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

    pub async fn set_powered(&self, adapter: &str, on: bool) -> zbus::Result<()> {
        self.set_adapter_prop(adapter, "Powered", on).await
    }

    pub async fn is_powered(&self, adapter: &str) -> zbus::Result<bool> {
        self.adapter_prop(adapter, "Powered").await
    }

    /// Make this machine visible to others. Off by default and never left on — a permanently
    /// discoverable adapter is a standing invitation.
    pub async fn set_discoverable(&self, adapter: &str, on: bool) -> zbus::Result<()> {
        self.set_adapter_prop(adapter, "Discoverable", on).await
    }

    /// Begin scanning. The scan runs until [`Self::stop_discovery`] or until this client's
    /// connection closes.
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

    pub async fn connect_device(&self, device: &str) -> zbus::Result<()> {
        let proxy = self.device_proxy(device).await?;
        proxy.call_method("Connect", &()).await?;
        Ok(())
    }

    pub async fn disconnect_device(&self, device: &str) -> zbus::Result<()> {
        let proxy = self.device_proxy(device).await?;
        proxy.call_method("Disconnect", &()).await?;
        Ok(())
    }

    /// Mark a paired device trusted. Trust is what lets a keyboard reconnect without the user
    /// re-authorising every profile (`BlueZ` answers `AuthorizeService` itself for trusted
    /// devices) — a real grant, so it's set explicitly after a successful pairing rather than
    /// implied by it.
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

/// The changes [`BluezClient::watch_changes`] follows, counted. Cloning
/// shares the same follower.
#[derive(Clone, Debug)]
pub struct BluezChanges {
    receiver: watch::Receiver<u64>,
}

impl BluezChanges {
    /// How many changes there were so far.
    #[must_use]
    pub fn count(&self) -> u64 {
        *self.receiver.borrow()
    }

    /// Wait for the next change, and return how many there were so far.
    /// `None` once the follower has stopped (its bus connection closed).
    pub async fn changed(&mut self) -> Option<u64> {
        self.receiver.changed().await.ok()?;
        Some(*self.receiver.borrow_and_update())
    }
}

/// Count the property changes a person sees and every other signal, until
/// the streams end or nobody watches.
async fn follow_changes(
    mut properties: MessageStream,
    mut others: futures_util::stream::Select<MessageStream, MessageStream>,
    sender: watch::Sender<u64>,
) {
    loop {
        let counts = tokio::select! {
            () = sender.closed() => return,
            signal = properties.next() => match signal {
                Some(Ok(signal)) => shows(&signal),
                Some(Err(_)) => false,
                None => return,
            },
            signal = others.next() => {
                if signal.is_none() {
                    return;
                }
                true
            }
        };
        if counts {
            sender.send_modify(|count| *count = count.wrapping_add(1));
        }
    }
}

/// Whether a `PropertiesChanged` changed what a Bluetooth page shows.
fn shows(signal: &zbus::Message) -> bool {
    let Ok((interface, changed, invalidated)) =
        signal
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    else {
        return false;
    };
    shown_change(&interface, changed.keys().chain(&invalidated))
}

/// Whether properties `names` of `interface` are among what a Bluetooth
/// page shows.
fn shown_change<'a>(interface: &str, mut names: impl Iterator<Item = &'a String>) -> bool {
    let shown = match interface {
        DEVICE_IFACE => SHOWN_DEVICE,
        ADAPTER_IFACE => SHOWN_ADAPTER,
        BATTERY_IFACE => return true,
        _ => return false,
    };
    names.any(|name| shown.contains(&name.as_str()))
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
        {
            assert_eq!(
                bluez_error_text("org.bluez.Error.Failed", None),
                "Bluetooth error: Failed"
            );
        }
    }

    /// A device the fake serves: whether it is connected, and its transmit
    /// power, which a page does not show.
    struct FakeDevice {
        connected: bool,
        tx_power: i16,
    }

    #[zbus::interface(name = "org.bluez.Device1")]
    impl FakeDevice {
        #[zbus(property)]
        fn connected(&self) -> bool {
            self.connected
        }

        #[zbus(property)]
        fn tx_power(&self) -> i16 {
            self.tx_power
        }
    }

    const DEVICE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";

    #[test]
    fn only_what_a_page_shows_counts_as_a_change() {
        let names = |names: &[&str]| -> Vec<String> {
            names.iter().map(|name| (*name).to_owned()).collect()
        };
        assert!(shown_change(DEVICE_IFACE, names(&["Connected"]).iter()));
        assert!(shown_change(DEVICE_IFACE, names(&["RSSI", "Alias"]).iter()));
        assert!(!shown_change(
            DEVICE_IFACE,
            names(&["RSSI", "ManufacturerData"]).iter()
        ));
        assert!(shown_change(ADAPTER_IFACE, names(&["Powered"]).iter()));
        assert!(!shown_change(ADAPTER_IFACE, names(&["Class"]).iter()));
        assert!(shown_change(BATTERY_IFACE, names(&["Percentage"]).iter()));
        assert!(!shown_change(
            "org.bluez.MediaPlayer1",
            names(&["Position"]).iter()
        ));
    }

    /// Against a fake `BlueZ` over a socket pair: a device connecting by
    /// itself is counted, its transmit power moving is not, and the watch
    /// ends with the connection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_change_watch_counts_a_device_connecting_by_itself() {
        let bound = Duration::from_secs(60);
        let (server, client) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(server)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(
                DEVICE,
                FakeDevice {
                    connected: false,
                    tx_power: 0,
                },
            )
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(client).p2p().build();
        let (server, client) = tokio::join!(server, client);
        let (server, client) = (server.unwrap(), client.unwrap());
        let mut changes = BluezClient::with_connection(client, DeviceNames::new())
            .watch_changes()
            .await
            .unwrap();
        assert_eq!(changes.count(), 0);

        let device = server
            .object_server()
            .interface::<_, FakeDevice>(DEVICE)
            .await
            .unwrap();
        let emitter = device.signal_emitter();
        device.get_mut().await.tx_power = 4;
        device.get().await.tx_power_changed(emitter).await.unwrap();
        device.get_mut().await.connected = true;
        device.get().await.connected_changed(emitter).await.unwrap();
        let counted = tokio::time::timeout(bound, changes.changed())
            .await
            .expect("connecting reaches the watch")
            .expect("the follower is still running");
        assert_eq!(counted, 1);
        // The transmit power came first on the wire: had it counted, it
        // would be by now.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(changes.count(), 1, "transmit power is not a change");

        // The interface holds the server's connection too.
        drop((device, server));
        tokio::time::timeout(bound, async { while changes.changed().await.is_some() {} })
            .await
            .expect("the watch ends with the connection");
    }
}
