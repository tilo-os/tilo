//! The `NetworkManager` client: one reading, a followed reading, an explicit
//! connectivity check, an explicit Wi-Fi scan and the captive-portal URI.
//!
//! Everything goes to `org.freedesktop.NetworkManager` on the **system** bus.
//! Reading its properties is open to every local client under the stock D-Bus
//! policy; `CheckConnectivity` and `RequestScan` are subject to polkit, which
//! may refuse callers outside an active session.

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::watch;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, Proxy};

use crate::model::{
    LinkKind, NetworkState, NmConnectivity, NmState, PrimaryLink, metered_from_raw,
};

/// `NetworkManager`'s bus name.
pub const BUS_NAME: &str = "org.freedesktop.NetworkManager";
/// The root object, which carries the global state.
pub const ROOT_PATH: &str = "/org/freedesktop/NetworkManager";
/// The root object's interface.
pub const ROOT_INTERFACE: &str = "org.freedesktop.NetworkManager";
const ACTIVE_INTERFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const DEVICE_INTERFACE: &str = "org.freedesktop.NetworkManager.Device";
/// A Wi-Fi device's interface: `RequestScan` and the `LastScan` it advances.
pub const WIRELESS_INTERFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
/// Signals buffered per stream before the oldest is dropped. Every signal
/// triggers a full re-read, so a dropped one loses nothing.
const SIGNAL_QUEUE: usize = 64;

/// Why `NetworkManager` could not be asked.
#[derive(Debug, thiserror::Error)]
pub enum NmError {
    #[error("cannot reach NetworkManager: {0}")]
    Bus(#[from] zbus::Error),
    #[error("NetworkManager is not running")]
    NotRunning,
    #[error("NetworkManager returned an unsafe connectivity-check URI")]
    UnsafeUri,
    #[error("NetworkManager manages no Wi-Fi device")]
    NoWifiDevice,
    #[error("the Wi-Fi device went away during the scan")]
    WifiDeviceRemoved,
    #[error("the connection to NetworkManager closed")]
    Disconnected,
}

/// A handle on `NetworkManager` over one D-Bus connection.
#[derive(Clone, Debug)]
pub struct NmClient {
    connection: Connection,
}

impl NmClient {
    /// Connect to the system bus.
    ///
    /// # Errors
    ///
    /// [`NmError::Bus`] when the system bus cannot be reached.
    pub async fn system() -> Result<Self, NmError> {
        Ok(Self::new(Connection::system().await?))
    }

    /// Use an existing connection: the system bus in production, a fake
    /// `NetworkManager` in tests.
    #[must_use]
    pub const fn new(connection: Connection) -> Self {
        Self { connection }
    }

    /// Read the global state once.
    ///
    /// # Errors
    ///
    /// [`NmError::Bus`] when `NetworkManager` is not on the bus or refuses
    /// the read.
    pub async fn state(&self) -> Result<NetworkState, NmError> {
        read_state(&self.connection).await
    }

    /// Follow the global state. The returned watch holds the current reading
    /// at once and is updated on `NetworkManager`'s `PropertiesChanged`
    /// signals; nothing polls. While `NetworkManager` is not on the bus the
    /// reading is [`NetworkState::unknown`], and it recovers when the daemon
    /// returns.
    ///
    /// Must be called inside a tokio runtime; the follower runs there until
    /// every clone of the watch is dropped.
    ///
    /// # Errors
    ///
    /// [`NmError::Bus`] when the signal subscriptions cannot be made.
    pub async fn watch(&self) -> Result<NmWatch, NmError> {
        // Subscribe before the first read so no change can fall between them.
        let changes = MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BUS_NAME)?
                .path(ROOT_PATH)?
                .interface(PROPERTIES_INTERFACE)?
                .member("PropertiesChanged")?
                .arg(0, ROOT_INTERFACE)?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?;
        let owners = name_owner_changes(&self.connection).await?;
        let initial = read_state(&self.connection)
            .await
            .unwrap_or_else(|_| NetworkState::unknown());
        let (sender, receiver) = watch::channel(initial);
        tokio::spawn(follow(self.connection.clone(), changes, owners, sender));
        Ok(NmWatch { receiver })
    }

    /// Ask `NetworkManager` to probe its connectivity-check URI now, and
    /// return the verdict. This sends an HTTP request from the machine, so it
    /// is for a deliberate "check again", never for a routine read.
    ///
    /// # Errors
    ///
    /// [`NmError::Bus`] when `NetworkManager` is unreachable or polkit
    /// refuses this caller a recheck.
    pub async fn check_connectivity_now(&self) -> Result<NmConnectivity, NmError> {
        ensure_running(&self.connection).await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let raw: u32 = root.call("CheckConnectivity", &()).await?;
        Ok(NmConnectivity::from_raw(raw))
    }

    /// Scan for Wi-Fi networks now: ask every Wi-Fi device `NetworkManager`
    /// manages to scan, and return once each has finished. A device's scan
    /// has finished when its `LastScan` moves past the value read before the
    /// request; that is followed by signal, nothing polls.
    ///
    /// A radio scan takes as long as the hardware needs (more than ten
    /// seconds on some adapters), so this is for a deliberate "look again"
    /// on a worker. Reading what is in range never waits for it: the access
    /// points `NetworkManager` already knows are always readable at once.
    ///
    /// # Errors
    ///
    /// [`NmError::NoWifiDevice`] when no device can scan,
    /// [`NmError::Bus`] when `NetworkManager` refuses every request (radio
    /// off, polkit) or the bus closes, [`NmError::NotRunning`] when it leaves
    /// the bus, [`NmError::WifiDeviceRemoved`] when a scanning device goes
    /// away before finishing.
    pub async fn scan_wifi(&self) -> Result<(), NmError> {
        ensure_running(&self.connection).await?;
        // Subscribe before reading `LastScan`, so a scan finishing between
        // the read and the request cannot be missed.
        let mut changes = MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BUS_NAME)?
                .interface(PROPERTIES_INTERFACE)?
                .member("PropertiesChanged")?
                .arg(0, WIRELESS_INTERFACE)?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?;
        let mut removals = MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BUS_NAME)?
                .path(ROOT_PATH)?
                .interface(ROOT_INTERFACE)?
                .member("DeviceRemoved")?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?;
        let mut owners = name_owner_changes(&self.connection).await?;

        let mut pending = self.request_scans().await?;
        while !pending.is_empty() {
            tokio::select! {
                signal = changes.next() => {
                    let Some(signal) = signal else {
                        return Err(NmError::Disconnected);
                    };
                    if let Ok(signal) = signal
                        && let Some((device, scanned)) = last_scan_of(&signal)
                        && pending.get(&device).is_some_and(|before| *before != scanned)
                    {
                        pending.remove(&device);
                    }
                }
                signal = removals.next() => {
                    let Some(signal) = signal else {
                        return Err(NmError::Disconnected);
                    };
                    if let Ok(signal) = signal
                        && let Ok(device) = signal.body().deserialize::<OwnedObjectPath>()
                        && pending.contains_key(device.as_str())
                    {
                        return Err(NmError::WifiDeviceRemoved);
                    }
                }
                // The daemon left (or was replaced): its scans are gone.
                signal = owners.next() => {
                    return Err(if signal.is_some() { NmError::NotRunning } else { NmError::Disconnected });
                }
            }
        }
        Ok(())
    }

    /// Ask every Wi-Fi device to scan; each device asked, with the `LastScan`
    /// it reported before the request.
    async fn request_scans(&self) -> Result<HashMap<String, i64>, NmError> {
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let devices: Vec<OwnedObjectPath> = root.get_property("Devices").await?;
        let mut pending = HashMap::new();
        let mut refused = None;
        for device in devices {
            let wireless = proxy(&self.connection, device.as_str(), WIRELESS_INTERFACE).await?;
            // Not a Wi-Fi device: it has no `LastScan`.
            let Ok(before) = wireless.get_property::<i64>("LastScan").await else {
                continue;
            };
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            match wireless.call_method("RequestScan", &(options,)).await {
                Ok(_) => {
                    pending.insert(device.as_str().to_owned(), before);
                }
                Err(error) => refused = Some(error),
            }
        }
        if pending.is_empty() {
            return Err(refused.map_or(NmError::NoWifiDevice, NmError::from));
        }
        Ok(pending)
    }

    /// `NetworkManager`'s configured connectivity-check URI. Opening it is the
    /// standard captive-portal flow: an intercepting network redirects it to
    /// its sign-in page. Only `http(s)://` URIs are returned.
    ///
    /// # Errors
    ///
    /// [`NmError::Bus`] when it cannot be read, [`NmError::UnsafeUri`] for
    /// any other scheme.
    pub async fn captive_portal_uri(&self) -> Result<String, NmError> {
        ensure_running(&self.connection).await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let uri: String = root.get_property("ConnectivityCheckUri").await?;
        if uri.starts_with("https://") || uri.starts_with("http://") {
            Ok(uri)
        } else {
            Err(NmError::UnsafeUri)
        }
    }
}

/// A followed [`NetworkState`]. Cloning shares the same follower.
#[derive(Clone, Debug)]
pub struct NmWatch {
    receiver: watch::Receiver<NetworkState>,
}

impl NmWatch {
    /// The latest reading.
    #[must_use]
    pub fn current(&self) -> NetworkState {
        self.receiver.borrow().clone()
    }

    /// Wait for the reading to change, and return it. `None` once the
    /// follower has stopped (its bus connection closed); the last reading
    /// it published is then [`NetworkState::unknown`].
    pub async fn changed(&mut self) -> Option<NetworkState> {
        self.receiver.changed().await.ok()?;
        Some(self.receiver.borrow_and_update().clone())
    }
}

/// Follow the global state on the system bus; see [`NmClient::watch`].
///
/// # Errors
///
/// [`NmError::Bus`] when the system bus cannot be reached.
pub async fn watch() -> Result<NmWatch, NmError> {
    NmClient::system().await?.watch().await
}

/// Read the global state once from the system bus.
///
/// # Errors
///
/// [`NmError::Bus`] when `NetworkManager` cannot be asked.
pub async fn state() -> Result<NetworkState, NmError> {
    NmClient::system().await?.state().await
}

/// See [`NmClient::check_connectivity_now`].
///
/// # Errors
///
/// [`NmError::Bus`] when the check cannot be made.
pub async fn check_connectivity_now() -> Result<NmConnectivity, NmError> {
    NmClient::system().await?.check_connectivity_now().await
}

/// See [`NmClient::scan_wifi`].
///
/// # Errors
///
/// As [`NmClient::scan_wifi`].
pub async fn scan_wifi() -> Result<(), NmError> {
    NmClient::system().await?.scan_wifi().await
}

/// See [`NmClient::captive_portal_uri`].
///
/// # Errors
///
/// As [`NmClient::captive_portal_uri`].
pub async fn captive_portal_uri() -> Result<String, NmError> {
    NmClient::system().await?.captive_portal_uri().await
}

/// The device and its new `LastScan`, when `signal` is a Wi-Fi device's
/// `PropertiesChanged` carrying one.
fn last_scan_of(signal: &zbus::Message) -> Option<(String, i64)> {
    let header = signal.header();
    let device = header.path()?.as_str().to_owned();
    let (_, changed, _) = signal
        .body()
        .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        .ok()?;
    let scanned = i64::try_from(changed.get("LastScan")?).ok()?;
    Some((device, scanned))
}

/// `NetworkManager` joining or leaving the bus.
async fn name_owner_changes(connection: &Connection) -> zbus::Result<MessageStream> {
    MessageStream::for_match_rule(
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .path("/org/freedesktop/DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, BUS_NAME)?
            .build(),
        connection,
        Some(SIGNAL_QUEUE),
    )
    .await
}

async fn follow(
    connection: Connection,
    mut changes: MessageStream,
    mut owners: MessageStream,
    sender: watch::Sender<NetworkState>,
) {
    loop {
        let signal = tokio::select! {
            () = sender.closed() => return,
            signal = changes.next() => signal,
            signal = owners.next() => signal,
        };
        if signal.is_none() {
            // The connection is gone; nothing is known any more.
            publish(&sender, NetworkState::unknown());
            return;
        }
        // A daemon that left the bus fails this read, which is the truth:
        // unknown until it returns.
        let state = read_state(&connection)
            .await
            .unwrap_or_else(|_| NetworkState::unknown());
        publish(&sender, state);
    }
}

fn publish(sender: &watch::Sender<NetworkState>, state: NetworkState) {
    sender.send_if_modified(|current| {
        if *current == state {
            false
        } else {
            *current = state;
            true
        }
    });
}

async fn proxy<'a>(
    connection: &Connection,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    zbus::proxy::Builder::new(connection)
        .destination(BUS_NAME)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// Refuse early when `NetworkManager` is not on the bus. A method call to an
/// absent well-known name would ask the bus to activate the daemon and wait
/// out the activation timeout; a reader must not stall on that.
async fn ensure_running(connection: &Connection) -> Result<(), NmError> {
    if !connection.is_bus() {
        return Ok(());
    }
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    let name = zbus::names::BusName::try_from(BUS_NAME).map_err(zbus::Error::from)?;
    if bus.name_has_owner(name).await.map_err(zbus::Error::from)? {
        Ok(())
    } else {
        Err(NmError::NotRunning)
    }
}

async fn read_state(connection: &Connection) -> Result<NetworkState, NmError> {
    ensure_running(connection).await?;
    let root = proxy(connection, ROOT_PATH, ROOT_INTERFACE).await?;
    let state: u32 = root.get_property("State").await?;
    let connectivity: u32 = root.get_property("Connectivity").await?;
    let metered: u32 = root.get_property("Metered").await?;
    let primary: OwnedObjectPath = root.get_property("PrimaryConnection").await?;
    let primary = if primary.as_str() == "/" {
        None
    } else {
        let kind: String = root
            .get_property("PrimaryConnectionType")
            .await
            .unwrap_or_default();
        Some(read_primary(connection, primary.as_str(), &kind).await)
    };
    Ok(NetworkState {
        state: NmState::from_raw(state),
        connectivity: NmConnectivity::from_raw(connectivity),
        metered: metered_from_raw(metered),
        primary,
    })
}

/// The primary connection's name and interface. Either may be missing when
/// the connection is torn down mid-read; the kind still says a link is up.
async fn read_primary(connection: &Connection, path: &str, kind: &str) -> PrimaryLink {
    let mut link = PrimaryLink {
        kind: LinkKind::from_connection_type(kind),
        name: None,
        device: None,
    };
    let Ok(active) = proxy(connection, path, ACTIVE_INTERFACE).await else {
        return link;
    };
    link.name = active
        .get_property::<String>("Id")
        .await
        .ok()
        .filter(|name| !name.is_empty());
    let devices: Vec<OwnedObjectPath> = active.get_property("Devices").await.unwrap_or_default();
    if let Some(device) = devices.first()
        && let Ok(device) = proxy(connection, device.as_str(), DEVICE_INTERFACE).await
    {
        link.device = device
            .get_property::<String>("Interface")
            .await
            .ok()
            .filter(|name| !name.is_empty());
    }
    link
}
