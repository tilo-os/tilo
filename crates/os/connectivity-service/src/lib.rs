//! The session's connectivity service.
//!
//! It publishes four resources, each a reading of the device and the
//! actions that change it:
//!
//! * `network`: Wi-Fi (the radio and the networks in reach, each a child
//!   offering to join or leave it), the networks this device keeps (each a
//!   child offering to join by itself again or be forgotten), the wired
//!   link, mobile data, airplane mode and whether the internet answers;
//! * `bluetooth`: the radio, the paired accessories (each a child offering
//!   to connect, disconnect or be forgotten) and the devices in reach (each
//!   a child offering to be paired);
//! * `vpn`: the VPN profiles, each a child offering to connect or
//!   disconnect it, which a configuration's text adds and removes;
//! * `tethering`: the hotspot sharing this device's connection, its name,
//!   band and the devices on it.
//!
//! Quick settings, Settings, the top bar, the palette, a script and an
//! agent read the same readings and invoke the same actions. An action the
//! device cannot do now (no adapter, a switch holding the radio off) is
//! published unavailable, with why. The device calls run on the service's
//! worker; while one runs its action is `pending` in the reading, and why a
//! change did not happen is its `problem` until the next.
//!
//! The readings follow the device: the session calls [`Connectivity::refresh`]
//! when `NetworkManager` or `BlueZ` say something changed
//! ([`Connectivity::follow`]), and after every change.

pub mod device;
pub mod effects;
tilo_resource::declared_names!();

pub mod model;

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, ApiError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, offer, reading, unavailable};

use device::{Change, Device, Readings};
use effects::{
    BluetoothAccessoryAction, BluetoothAction, BluetoothNearbyAction, ModemAction, NetworkAction,
    SavedNetworkAction, TetheringAction, VpnAction, VpnProfileAction, WifiNetworkAction,
};
use model::{Bluetooth, Modem, Network, Tethering, Vpn};

/// The resources' ids.
pub const NETWORK: &str = "network";
pub const BLUETOOTH: &str = "bluetooth";
pub const VPN: &str = "vpn";
pub const TETHERING: &str = "tethering";
pub const MODEM: &str = "modem";
/// The kind part of a Wi-Fi network's address (`wifi-network:Home`).
pub const WIFI_NETWORK: &str = "wifi-network";
/// The kind part of a saved network's address (`saved-network:<id>`).
pub const SAVED_NETWORK: &str = "saved-network";
/// The kind part of a paired accessory's address (`bluetooth-accessory:<hardware address>`).
pub const BLUETOOTH_ACCESSORY: &str = "bluetooth-accessory";
/// The kind part of a Bluetooth device in reach's address (`bluetooth-nearby:<hardware address>`).
pub const BLUETOOTH_NEARBY: &str = "bluetooth-nearby";
/// The kind part of a VPN profile's address (`vpn-profile:<id>`).
pub const VPN_PROFILE: &str = "vpn-profile";

/// Which resource a change belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Network,
    Modem,
    Bluetooth,
    Vpn,
    Tethering,
}

/// What lands from the worker.
enum Landed {
    /// A fresh reading.
    Read(Box<Readings>),
    /// A change was made (or not), and the device read after it.
    Changed {
        part: Part,
        action: String,
        result: Result<(), String>,
        readings: Box<Readings>,
    },
}

/// The connectivity service.
pub struct Connectivity {
    readings: Readings,
    worker: Worker<dyn Device, Landed>,
}

impl Connectivity {
    /// The service over `device`, reading it once at the start; `wake` is
    /// called whenever something landed for the session to take in.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn new(device: Arc<dyn Device>, wake: Option<Wake>) -> std::io::Result<Self> {
        let service = Self {
            readings: Readings::default(),
            worker: Worker::spawn("tilo-connectivity", device, wake)?,
        };
        service.refresh();
        Ok(service)
    }

    /// Read the device again.
    pub fn refresh(&self) {
        refresh(&self.worker.jobs());
    }

    /// Read the device again whenever `NetworkManager` or `BlueZ` report a
    /// change, for as long as the returned followers live.
    ///
    /// # Errors
    /// The followers did not start.
    pub fn follow(&self) -> Result<Follow, String> {
        let jobs = self.worker.jobs();
        let network = os_actions::network::follow_network_changes(move || refresh(&jobs))?;
        let jobs = self.worker.jobs();
        let bluetooth = os_actions::bluez::follow_changes(move || refresh(&jobs))?;
        let jobs = self.worker.jobs();
        let modem = os_actions::bus_follow::follow_system_services(
            &["org.freedesktop.ModemManager1"],
            move || refresh(&jobs),
        )?;
        Ok(Follow {
            _network: network,
            _bluetooth: bluetooth,
            _modem: modem,
        })
    }

    /// What the device reads now.
    #[must_use]
    pub const fn readings(&self) -> &Readings {
        &self.readings
    }

    fn land(&mut self, landed: Landed) {
        match landed {
            Landed::Read(readings) => self.take(*readings),
            Landed::Changed {
                part,
                action,
                result,
                readings,
            } => {
                self.take(*readings);
                let (pending, problem) = self.status(part);
                if let Some(at) = pending.iter().position(|id| *id == action) {
                    pending.remove(at);
                }
                *problem = result.err();
            }
        }
    }

    /// Take a fresh reading, keeping what is the service's own: the actions
    /// pending and the last problem.
    fn take(&mut self, mut readings: Readings) {
        let old = &mut self.readings;
        readings.network.pending = std::mem::take(&mut old.network.pending);
        readings.network.problem = old.network.problem.take();
        readings.bluetooth.pending = std::mem::take(&mut old.bluetooth.pending);
        readings.bluetooth.problem = old.bluetooth.problem.take();
        readings.modem.pending = std::mem::take(&mut old.modem.pending);
        readings.modem.problem = old.modem.problem.take();
        readings.vpn.pending = std::mem::take(&mut old.vpn.pending);
        readings.vpn.problem = old.vpn.problem.take();
        readings.tethering.pending = std::mem::take(&mut old.tethering.pending);
        readings.tethering.problem = old.tethering.problem.take();
        address_rows(&mut readings);
        self.readings = readings;
    }

    fn status(&mut self, part: Part) -> (&mut Vec<String>, &mut Option<String>) {
        let readings = &mut self.readings;
        match part {
            Part::Network => (&mut readings.network.pending, &mut readings.network.problem),
            Part::Bluetooth => (
                &mut readings.bluetooth.pending,
                &mut readings.bluetooth.problem,
            ),
            Part::Modem => (&mut readings.modem.pending, &mut readings.modem.problem),
            Part::Vpn => (&mut readings.vpn.pending, &mut readings.vpn.problem),
            Part::Tethering => (
                &mut readings.tethering.pending,
                &mut readings.tethering.problem,
            ),
        }
    }

    /// Make `change` on the worker, then read the device back. Whoever asked
    /// hears whether the device made it once it is read back, as the reading
    /// shows it.
    fn start(&mut self, part: Part, action: &str, change: Change) -> Started {
        self.status(part).0.push(action.to_owned());
        let action = action.to_owned();
        let (answer, answered) = tilo_actions::remote::pending();
        self.worker.run(move |device| {
            let result = device.change(&change);
            let readings = Box::new(device.read());
            answer.complete(
                result
                    .clone()
                    .map(|()| ActionOutput::default())
                    .map_err(ApiError::Failed),
            );
            Landed::Changed {
                part,
                action,
                result,
                readings,
            }
        });
        Started::Later(answered)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per action the network offers"
    )]
    fn network(&mut self, action: NetworkAction) -> Result<Started, ApiError> {
        let id = action.id();
        let network = &self.readings.network;
        let change = match action {
            NetworkAction::SetWifi { on } => Change::Wifi(on),
            NetworkAction::ToggleWifi => Change::Wifi(!network.wifi.on),
            NetworkAction::ScanWifi => Change::Scan,
            NetworkAction::ConnectWifi { network, password } => Change::Join {
                network,
                password: Some(password),
                hidden: false,
            },
            NetworkAction::ConnectHiddenWifi { network, password } => Change::Join {
                network,
                password: password.filter(|password| !password.is_empty()),
                hidden: true,
            },
            NetworkAction::ConnectEnterpriseWifi {
                profile,
                ssid,
                identity,
                anonymous_identity,
                ca_certificate,
                method,
                inner_auth,
                client_certificate,
                private_key,
                password,
            } => {
                let ssid = named(ssid, "network's name (ssid)")?;
                Change::Enterprise {
                    config: os_actions::EnterpriseWifiConfiguration {
                        profile_name: given(profile).unwrap_or_else(|| ssid.clone()),
                        ssid,
                        identity: named(identity, "identity")?,
                        anonymous_identity: given(anonymous_identity),
                        ca_certificate: named(ca_certificate, "CA certificate")?,
                        method: eap_method(method)?,
                        phase2_auth: phase2_auth(inner_auth)?,
                        client_certificate: given(client_certificate),
                        private_key: given(private_key),
                    },
                    password: password.filter(|password| !password.is_empty()),
                }
            }
            NetworkAction::SetMobileMetered { metered } => Change::MobileMetered {
                metered: metered_from_key(&metered)?,
            },
            NetworkAction::SetWired { on } => Change::Wired(on),
            NetworkAction::SetMobileData { on } => Change::MobileData(on),
            NetworkAction::SetMobileApn { apn } => Change::MobileApn {
                apn: apn.filter(|apn| !apn.trim().is_empty()),
            },
            NetworkAction::SetMobileRoaming { on } => Change::MobileRoaming(on),
            NetworkAction::SetAirplaneMode { on } => Change::Airplane(on),
            NetworkAction::ToggleAirplaneMode => Change::Airplane(!network.airplane_mode),
            NetworkAction::SetNetworkIpv4 {
                profile,
                automatic,
                addresses,
                gateway,
                dns,
            } => Change::Ipv4 {
                profile,
                config: os_actions::IpConfiguration {
                    automatic,
                    addresses,
                    gateway: given(gateway),
                    dns,
                },
            },
            NetworkAction::SetNetworkIpv6 {
                profile,
                method,
                addresses,
                gateway,
                dns,
            } => Change::Ipv6 {
                profile,
                config: os_actions::network_settings::Ipv6Configuration {
                    method: ipv6_method(&method)?,
                    addresses,
                    gateway: given(gateway),
                    dns,
                },
            },
            NetworkAction::SetNetworkDns { profile, servers } => Change::Dns { profile, servers },
            NetworkAction::SetNetworkPolicy {
                profile,
                metered,
                mac_address,
                proxy,
                proxy_address,
                browser_only,
                priority,
                hidden,
            } => Change::Policy {
                profile,
                patch: os_actions::ConnectionPolicyPatch {
                    metered: metered.as_deref().map(metered_policy).transpose()?,
                    mac: mac_address.as_deref().map(mac_policy).transpose()?,
                    proxy: proxy
                        .as_deref()
                        .map(|kind| proxy_configuration(kind, given(proxy_address), browser_only))
                        .transpose()?,
                    priority: priority
                        .map(|priority| {
                            i32::try_from(priority).map_err(|_| {
                                ApiError::InvalidArguments(format!("{priority} is not a priority"))
                            })
                        })
                        .transpose()?,
                    hidden,
                },
            },
            NetworkAction::SetEnterpriseIdentity {
                profile,
                identity,
                ca_certificate,
            } => Change::Identity {
                profile,
                identity: os_actions::network_settings::EnterpriseIdentity {
                    identity,
                    ca_certificate,
                },
            },
            NetworkAction::SetWifiPassword { profile, password } => {
                Change::WifiPassword { profile, password }
            }
        };
        Ok(self.start(Part::Network, id, change))
    }

    /// `action` on the Wi-Fi network in reach named `name`.
    fn wifi_network(&mut self, name: &str, action: WifiNetworkAction) -> Result<Started, ApiError> {
        let id = action.id();
        let network = self
            .readings
            .network
            .wifi
            .networks
            .iter()
            .find(|n| n.name == name);
        let change = match action {
            WifiNetworkAction::Join => {
                if network.is_some_and(|n| n.secure && !n.known) {
                    return Err(ApiError::Unavailable {
                        code: "password_needed".into(),
                        reason: format!("{name} asks for a password: join it with connect-wifi"),
                    });
                }
                Change::Join {
                    network: name.to_owned(),
                    password: None,
                    hidden: false,
                }
            }
            WifiNetworkAction::Leave => Change::Leave {
                network: name.to_owned(),
            },
        };
        Ok(self.start(Part::Network, id, change))
    }

    /// `action` on the network this device keeps under the id `profile`.
    fn saved_network(
        &mut self,
        profile: &str,
        action: SavedNetworkAction,
    ) -> Result<Started, ApiError> {
        let id = action.id();
        let change = match action {
            SavedNetworkAction::Join => {
                let saved = self.readings.network.saved.iter().find(|s| s.id == profile);
                Change::Join {
                    network: saved.map_or_else(|| profile.to_owned(), |s| s.name.clone()),
                    password: None,
                    hidden: false,
                }
            }
            SavedNetworkAction::SetAutoconnect { on } => Change::Autoconnect {
                profile: profile.to_owned(),
                on,
            },
            SavedNetworkAction::Forget => Change::Forget {
                profile: profile.to_owned(),
            },
            SavedNetworkAction::SetProxy { server } => Change::Proxy {
                profile: profile.to_owned(),
                server: proxy(server)?,
            },
        };
        Ok(self.start(Part::Network, id, change))
    }

    fn bluetooth(&mut self, action: BluetoothAction) -> Started {
        let id = action.id();
        let change = match action {
            BluetoothAction::SetBluetooth { on } => Change::Bluetooth(on),
            BluetoothAction::ToggleBluetooth => Change::Bluetooth(!self.readings.bluetooth.on),
            BluetoothAction::Scan { on } => Change::ScanBluetooth(on),
        };
        self.start(Part::Bluetooth, id, change)
    }

    /// `action` on the paired accessory whose hardware address is `address`.
    fn bluetooth_accessory(&mut self, address: &str, action: BluetoothAccessoryAction) -> Started {
        let id = action.id();
        let address = address.to_owned();
        let change = match action {
            BluetoothAccessoryAction::Connect => Change::Connect { address },
            BluetoothAccessoryAction::Disconnect => Change::Disconnect { address },
            BluetoothAccessoryAction::Forget => Change::ForgetBluetooth { address },
        };
        self.start(Part::Bluetooth, id, change)
    }

    /// `action` on the Bluetooth device in reach whose hardware address is `address`: refused
    /// while the radio is missing or off, when nothing is in reach to pair.
    fn bluetooth_nearby(
        &mut self,
        address: &str,
        action: BluetoothNearbyAction,
    ) -> Result<Started, ApiError> {
        let id = action.id();
        let bluetooth = &self.readings.bluetooth;
        if !bluetooth.available {
            return Err(ApiError::Unavailable {
                code: "no_adapter".into(),
                reason: "This device has no Bluetooth".into(),
            });
        }
        if !bluetooth.on {
            return Err(ApiError::Unavailable {
                code: "bluetooth_off".into(),
                reason: "Bluetooth is off".into(),
            });
        }
        let change = match action {
            BluetoothNearbyAction::Pair => Change::Pair {
                address: address.to_owned(),
            },
        };
        Ok(self.start(Part::Bluetooth, id, change))
    }

    fn vpn(&mut self, action: VpnAction) -> Started {
        let id = action.id();
        let change = match action {
            VpnAction::ImportVpn { kind, name, text } => Change::ImportVpn { kind, name, text },
        };
        self.start(Part::Vpn, id, change)
    }

    /// `action` on the VPN profile whose id is `profile`.
    fn vpn_profile(&mut self, profile: &str, action: VpnProfileAction) -> Started {
        let id = action.id();
        let profile = profile.to_owned();
        let change = match action {
            VpnProfileAction::Connect => Change::Vpn { profile, on: true },
            VpnProfileAction::Disconnect => Change::Vpn { profile, on: false },
            VpnProfileAction::Forget => Change::ForgetVpn { profile },
            VpnProfileAction::Edit {
                name,
                autoconnect,
                all_traffic,
                dns,
                mtu,
                endpoint,
                allowed_ips,
                keepalive,
            } => Change::EditVpn {
                profile,
                patch: os_actions::network_settings::VpnProfilePatch {
                    name,
                    autoconnect,
                    all_traffic,
                    dns,
                    mtu: mtu.map(|mtu| (mtu != 0).then_some(mtu)),
                    endpoint,
                    allowed_ips,
                    keepalive: keepalive.map(|seconds| (seconds != 0).then_some(seconds)),
                },
            },
        };
        self.start(Part::Vpn, id, change)
    }

    fn tethering(&mut self, action: TetheringAction) -> Result<Started, ApiError> {
        let id = action.id();
        let change = match action {
            TetheringAction::SetHotspot { on } => Change::Hotspot(on),
            TetheringAction::ConfigureHotspot {
                name,
                password,
                band,
            } => Change::ConfigureHotspot {
                name,
                password,
                band: band
                    .parse::<model::Band>()
                    .map_err(ApiError::InvalidArguments)?,
            },
        };
        Ok(self.start(Part::Tethering, id, change))
    }
}

/// Following `NetworkManager` and `BlueZ`; it stops when dropped.
pub struct Follow {
    _network: os_actions::network::NetworkFollow,
    _bluetooth: os_actions::bluez::BluetoothFollow,
    _modem: os_actions::bus_follow::ServiceFollow,
}

fn refresh(jobs: &Jobs<dyn Device, Landed>) {
    jobs.run(|device| Landed::Read(Box::new(device.read())));
}

/// A child resource's id: its kind and its key.
fn child_id(kind: &str, key: &str) -> Option<String> {
    (!key.is_empty() && !key.contains(tilo_actions::resource::KEY_SEP))
        .then(|| format!("{kind}:{key}"))
}

/// Each row of the readings that is a resource of its own carries its address, the one its
/// child is published at: a view calls the row's own actions on it, as every caller does.
fn address_rows(readings: &mut Readings) {
    let network = &mut readings.network;
    for wifi in &mut network.wifi.networks {
        wifi.address =
            tilo_resource::Handle::new(child_id(WIFI_NETWORK, &wifi.name).unwrap_or_default());
    }
    for saved in &mut network.saved {
        saved.address =
            tilo_resource::Handle::new(child_id(SAVED_NETWORK, &saved.id).unwrap_or_default());
    }
    for accessory in &mut readings.bluetooth.accessories {
        accessory.handle = tilo_resource::Handle::new(
            child_id(BLUETOOTH_ACCESSORY, &accessory.address).unwrap_or_default(),
        );
    }
    for nearby in &mut readings.bluetooth.nearby {
        nearby.handle = tilo_resource::Handle::new(
            child_id(BLUETOOTH_NEARBY, &nearby.address).unwrap_or_default(),
        );
    }
    for profile in &mut readings.vpn.profiles {
        profile.address =
            tilo_resource::Handle::new(child_id(VPN_PROFILE, &profile.id).unwrap_or_default());
    }
}

/// The proxy server as it is used: `host:port`, or nothing for no proxy. An
/// empty entry is none.
fn proxy(server: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(server) = server.map(|server| server.trim().to_owned()) else {
        return Ok(None);
    };
    if server.is_empty() {
        return Ok(None);
    }
    let host_and_port = server
        .split_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
    if host_and_port {
        Ok(Some(server))
    } else {
        Err(ApiError::InvalidArguments(format!(
            "{server} is not a proxy as host:port"
        )))
    }
}

/// The IPv6 method `method` names.
fn ipv6_method(method: &str) -> Result<os_actions::network_settings::Ipv6Method, ApiError> {
    use os_actions::network_settings::Ipv6Method;
    match method {
        "automatic" => Ok(Ipv6Method::Automatic),
        "manual" => Ok(Ipv6Method::Manual),
        "link-local" => Ok(Ipv6Method::LinkLocal),
        "disabled" => Ok(Ipv6Method::Disabled),
        _ => Err(ApiError::InvalidArguments(format!(
            "{method} is not an IPv6 method: automatic, manual, link-local or disabled"
        ))),
    }
}

/// Whether a network is metered, as `metered` names it.
fn metered_policy(metered: &str) -> Result<os_actions::MeteredPolicy, ApiError> {
    match metered {
        "automatic" => Ok(os_actions::MeteredPolicy::Automatic),
        "metered" => Ok(os_actions::MeteredPolicy::Metered),
        "unmetered" => Ok(os_actions::MeteredPolicy::Unmetered),
        _ => Err(ApiError::InvalidArguments(format!(
            "{metered} is not automatic, metered or unmetered"
        ))),
    }
}

/// The hardware address a network is shown, as `mac` names it.
fn mac_policy(mac: &str) -> Result<os_actions::MacPolicy, ApiError> {
    match mac {
        "permanent" => Ok(os_actions::MacPolicy::Permanent),
        "random" => Ok(os_actions::MacPolicy::Random),
        "stable" => Ok(os_actions::MacPolicy::Stable),
        _ => Err(ApiError::InvalidArguments(format!(
            "{mac} is not permanent, random or stable"
        ))),
    }
}

/// A network's proxy: none, the configuration at `address` (automatic), or
/// the server `address` as host:port (manual).
fn proxy_configuration(
    kind: &str,
    address: Option<String>,
    browser_only: Option<bool>,
) -> Result<os_actions::ProxyConfiguration, ApiError> {
    let browser_only = browser_only.unwrap_or_default();
    let needed = |address: Option<String>| {
        address
            .ok_or_else(|| ApiError::InvalidArguments(format!("a {kind} proxy needs its address")))
    };
    match kind {
        "none" => Ok(os_actions::ProxyConfiguration::default()),
        "automatic" => Ok(os_actions::ProxyConfiguration {
            automatic_url: Some(needed(address)?),
            manual: None,
            browser_only,
        }),
        "manual" => Ok(os_actions::ProxyConfiguration {
            automatic_url: None,
            manual: proxy(Some(needed(address)?))?,
            browser_only,
        }),
        _ => Err(ApiError::InvalidArguments(format!(
            "{kind} is not a proxy: none, automatic or manual"
        ))),
    }
}

/// The given entry as it is used: trimmed, and nothing when it is empty.
fn given(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The entry `field` demands, else a refusal naming it: an enterprise
/// network has nothing to join without it.
fn named(value: Option<String>, field: &str) -> Result<String, ApiError> {
    given(value).ok_or_else(|| {
        ApiError::InvalidArguments(format!("an enterprise network joins with its {field}"))
    })
}

/// The 802.1X method `method` names, PEAP when it names none.
fn eap_method(method: Option<String>) -> Result<os_actions::EnterpriseEapMethod, ApiError> {
    let Some(method) = given(method) else {
        return Ok(os_actions::EnterpriseEapMethod::Peap);
    };
    match method.to_ascii_uppercase().as_str() {
        "PEAP" => Ok(os_actions::EnterpriseEapMethod::Peap),
        "TTLS" => Ok(os_actions::EnterpriseEapMethod::Ttls),
        "TLS" => Ok(os_actions::EnterpriseEapMethod::Tls),
        _ => Err(ApiError::InvalidArguments(format!(
            "{method} is not an 802.1X method: PEAP, TTLS or TLS"
        ))),
    }
}

/// The inner authentication `inner_auth` names, `MSCHAPv2` when it names
/// none.
fn phase2_auth(inner_auth: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(inner_auth) = given(inner_auth) else {
        return Ok(Some("mschapv2".to_owned()));
    };
    let named = inner_auth.to_ascii_lowercase();
    match named.as_str() {
        "mschapv2" | "pap" | "chap" => Ok(Some(named)),
        _ => Err(ApiError::InvalidArguments(format!(
            "{inner_auth} is not an inner authentication: MSCHAPv2, PAP or CHAP"
        ))),
    }
}

/// The network resource as a view of another language reads and changes it: its reading
/// ([`Network`]) as its fields and its actions as its verbs.
#[must_use]
pub fn network_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Network, NetworkAction>("network", "Network")
}

/// A Wi-Fi network in reach as a view of another language calls it: the actions offered on the
/// address its row in the network's reading carries.
#[must_use]
pub fn wifi_network_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::row_surface::<WifiNetworkRow, WifiNetworkAction>()
}

/// A network this device keeps as a view of another language calls it.
#[must_use]
pub fn saved_network_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::row_surface::<SavedNetworkRow, SavedNetworkAction>()
}

/// The Bluetooth resource as a view of another language reads and changes it.
#[must_use]
pub fn bluetooth_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Bluetooth, BluetoothAction>("bluetooth", "Bluetooth")
}

/// The VPN resource as a view of another language reads and changes it: the profiles and whether
/// each is connected, and the verbs that join, import and forget one.
#[must_use]
pub fn vpn_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Vpn, VpnAction>("vpn", "VPN")
}

/// The tethering resource as a view of another language reads and changes it.
#[must_use]
pub fn tethering_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Tethering, TetheringAction>("tethering", "Tethering")
}

/// A paired Bluetooth accessory as a view of another language calls it: the actions offered on
/// the address its row in the Bluetooth reading carries.
#[must_use]
pub fn bluetooth_accessory_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::row_surface::<BluetoothAccessoryRow, BluetoothAccessoryAction>()
}

/// A Bluetooth device in reach as a view of another language calls it.
#[must_use]
pub fn bluetooth_nearby_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::row_surface::<BluetoothNearbyRow, BluetoothNearbyAction>()
}

/// A VPN profile as a view of another language calls it.
#[must_use]
pub fn vpn_profile_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::row_surface::<VpnProfileRow, VpnProfileAction>()
}

/// The modem resource as a view of another language reads and changes it.
#[must_use]
pub fn modem_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Modem, ModemAction>("modem", "Modem")
}

/// The `network` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.network", label = "Network")]
pub struct NetworkReading;

/// The `bluetooth` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.bluetooth", label = "Bluetooth")]
pub struct BluetoothReading;

/// The `modem` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.modem", label = "Modem")]
pub struct ModemReading;

/// The `vpn` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.vpn", label = "VPN")]
pub struct VpnReading;

/// The `tethering` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.tethering", label = "Tethering")]
pub struct TetheringReading;

/// A Wi-Fi network in reach, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.wifi-network",
    label = "Wi-Fi network",
    effects = WifiNetworkAction
)]
pub struct WifiNetworkRow;

/// A network this device keeps, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.saved-network",
    label = "Saved network",
    effects = SavedNetworkAction
)]
pub struct SavedNetworkRow;

/// A paired Bluetooth accessory, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.bluetooth-accessory",
    label = "Bluetooth accessory",
    effects = BluetoothAccessoryAction
)]
pub struct BluetoothAccessoryRow;

/// A Bluetooth device in reach, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.bluetooth-nearby",
    label = "Bluetooth device",
    effects = BluetoothNearbyAction
)]
pub struct BluetoothNearbyRow;

/// A VPN profile, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.vpn-profile",
    label = "VPN profile",
    effects = VpnProfileAction
)]
pub struct VpnProfileRow;

/// The `network` resource reading `network`.
#[must_use]
pub fn network_resource(network: &Network) -> ResourceSpec {
    let mut resource = offer::<NetworkAction>(tilo_resource::typed_resource::<NetworkReading>(
        NETWORK,
        crate::i18n::name("so.tilo.connectivity.network", "Network"),
    ));
    mark_what_the_device_lacks(&mut resource, network);
    resource.with_children(network_children(network))
}

/// Publish what `network` cannot do now (no radio, no modem, Wi-Fi off) as
/// its actions being unavailable, saying why.
fn mark_what_the_device_lacks(resource: &mut ResourceSpec, network: &Network) {
    if !network.wifi.available {
        for action in [
            "set-wifi",
            "toggle-wifi",
            "scan-wifi",
            "connect-wifi",
            "connect-hidden-wifi",
            "connect-enterprise-wifi",
        ] {
            unavailable(resource, action, "no_wifi", "This device has no Wi-Fi");
        }
    } else if network.wifi.hardware_blocked {
        for action in ["set-wifi", "toggle-wifi"] {
            unavailable(
                resource,
                action,
                "hardware_switch",
                "A switch on the device holds Wi-Fi off",
            );
        }
    } else if !network.wifi.on {
        for action in [
            "scan-wifi",
            "connect-wifi",
            "connect-hidden-wifi",
            "connect-enterprise-wifi",
        ] {
            unavailable(resource, action, "wifi_off", "Wi-Fi is off");
        }
    }
    if network.wired.is_none() {
        unavailable(
            resource,
            "set-wired",
            "no_wired",
            "This device has no wired network",
        );
    }
    if !network.mobile.available {
        for action in ["set-mobile-data", "set-mobile-apn", "set-mobile-roaming"] {
            unavailable(
                resource,
                action,
                "no_modem",
                "This device has no mobile data",
            );
        }
    }
    if !network.wifi.available && !network.mobile.available {
        for action in ["set-airplane-mode", "toggle-airplane-mode"] {
            unavailable(
                resource,
                action,
                "no_radio",
                "This device has no wireless radio",
            );
        }
    }
}

/// No argument bound: a row's own action knows the row by the address it is called on.
const NO_ARGUMENTS: [(&str, Value); 0] = [];

/// The networks in reach and the networks this device keeps, each with what
/// pressing it does now.
fn network_children(network: &Network) -> impl Iterator<Item = ResourceSpec> + '_ {
    let reachable = network.wifi.networks.iter().filter_map(|wifi| {
        let id = child_id(WIFI_NETWORK, &wifi.name)?;
        let action = if wifi.connected {
            WifiNetworkAction::LEAVE
        } else {
            WifiNetworkAction::JOIN
        };
        let bindings = tilo_resource::RowBindings::new().bind(action, NO_ARGUMENTS);
        Some(
            tilo_resource::LiveRow::typed_with::<WifiNetworkRow>(id, wifi.name.clone(), &bindings)
                .into_resource(),
        )
    });
    let saved = network.saved.iter().filter_map(|profile| {
        let id = child_id(SAVED_NETWORK, &profile.id)?;
        let profile_id: Value = profile.id.clone().into();
        let bindings = tilo_resource::RowBindings::new()
            .bind(SavedNetworkAction::JOIN, NO_ARGUMENTS)
            .bind(SavedNetworkAction::SET_AUTOCONNECT, NO_ARGUMENTS)
            .bind(SavedNetworkAction::FORGET, NO_ARGUMENTS)
            .bind(SavedNetworkAction::SET_PROXY, NO_ARGUMENTS)
            .bind(NetworkAction::SET_NETWORK_IPV4, [("profile", profile_id.clone())])
            .bind(NetworkAction::SET_NETWORK_IPV6, [("profile", profile_id.clone())])
            .bind(NetworkAction::SET_NETWORK_DNS, [("profile", profile_id.clone())])
            .bind(NetworkAction::SET_NETWORK_POLICY, [("profile", profile_id.clone())])
            .bind(
                NetworkAction::SET_ENTERPRISE_IDENTITY,
                [("profile", profile_id.clone())],
            )
            .bind(NetworkAction::SET_WIFI_PASSWORD, [("profile", profile_id)]);
        Some(
            tilo_resource::LiveRow::typed_with::<SavedNetworkRow>(
                id,
                profile.name.clone(),
                &bindings,
            )
            .into_resource(),
        )
    });
    reachable.chain(saved)
}

/// The `bluetooth` resource reading `bluetooth`.
#[must_use]
pub fn bluetooth_resource(bluetooth: &Bluetooth) -> ResourceSpec {
    let mut resource = offer::<BluetoothAction>(tilo_resource::typed_resource::<BluetoothReading>(
        BLUETOOTH,
        crate::i18n::name("so.tilo.connectivity.bluetooth", "Bluetooth"),
    ));
    if !bluetooth.available {
        for action in ["set-bluetooth", "toggle-bluetooth", "scan"] {
            unavailable(
                &mut resource,
                action,
                "no_adapter",
                "This device has no Bluetooth",
            );
        }
    } else if !bluetooth.on {
        for action in ["scan"] {
            unavailable(&mut resource, action, "bluetooth_off", "Bluetooth is off");
        }
    }
    let paired = bluetooth.accessories.iter().filter_map(|accessory| {
        let id = child_id(BLUETOOTH_ACCESSORY, &accessory.address)?;
        let action = if accessory.connected {
            BluetoothAccessoryAction::DISCONNECT
        } else {
            BluetoothAccessoryAction::CONNECT
        };
        let bindings = tilo_resource::RowBindings::new()
            .bind(action, NO_ARGUMENTS)
            .bind(BluetoothAccessoryAction::FORGET, NO_ARGUMENTS);
        Some(
            tilo_resource::LiveRow::typed_with::<BluetoothAccessoryRow>(
                id,
                accessory.name.clone(),
                &bindings,
            )
            .into_resource(),
        )
    });
    let nearby = bluetooth.nearby.iter().filter_map(|nearby| {
        let id = child_id(BLUETOOTH_NEARBY, &nearby.address)?;
        let bindings =
            tilo_resource::RowBindings::new().bind(BluetoothNearbyAction::PAIR, NO_ARGUMENTS);
        Some(
            tilo_resource::LiveRow::typed_with::<BluetoothNearbyRow>(
                id,
                nearby.name.clone(),
                &bindings,
            )
            .into_resource(),
        )
    });
    resource.with_children(paired.chain(nearby))
}

/// The `vpn` resource reading `vpn`.
#[must_use]
pub fn vpn_resource(vpn: &Vpn) -> ResourceSpec {
    let resource = offer::<VpnAction>(tilo_resource::typed_resource::<VpnReading>(
        VPN,
        crate::i18n::name("so.tilo.connectivity.vpn", "VPN"),
    ));
    let children = vpn.profiles.iter().filter_map(|profile| {
        let id = child_id(VPN_PROFILE, &profile.id)?;
        let action = if profile.connected {
            VpnProfileAction::DISCONNECT
        } else {
            VpnProfileAction::CONNECT
        };
        let bindings = tilo_resource::RowBindings::new()
            .bind(action, NO_ARGUMENTS)
            .bind(VpnProfileAction::EDIT, NO_ARGUMENTS)
            .bind(VpnProfileAction::FORGET, NO_ARGUMENTS);
        Some(
            tilo_resource::LiveRow::typed_with::<VpnProfileRow>(
                id,
                profile.name.clone(),
                &bindings,
            )
            .into_resource(),
        )
    });
    resource.with_children(children)
}

/// The `tethering` resource reading `tethering`.
#[must_use]
pub fn tethering_resource(tethering: &Tethering) -> ResourceSpec {
    let mut resource = offer::<TetheringAction>(tilo_resource::typed_resource::<TetheringReading>(
        TETHERING,
        crate::i18n::name("so.tilo.connectivity.tethering", "Tethering"),
    ));
    if !tethering.available {
        for action in ["set-hotspot", "configure-hotspot"] {
            unavailable(
                &mut resource,
                action,
                "no_wifi",
                "This device has no Wi-Fi to share over",
            );
        }
    }
    resource
}

impl Service for Connectivity {
    fn resources(&self) -> Vec<ResourceSpec> {
        let readings = &self.readings;
        vec![
            network_resource(&readings.network),
            bluetooth_resource(&readings.bluetooth),
            modem_resource(&readings.modem),
            vpn_resource(&readings.vpn),
            tethering_resource(&readings.tethering),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let readings = &self.readings;
        match resource {
            NETWORK => Some(reading(&readings.network)),
            BLUETOOTH => Some(reading(&readings.bluetooth)),
            MODEM => Some(reading(&readings.modem)),
            VPN => Some(reading(&readings.vpn)),
            TETHERING => Some(reading(&readings.tethering)),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let published = self.resources();
        // A row that is a resource of its own (a Wi-Fi network, a saved network, a Bluetooth
        // device, a VPN profile) takes its own actions, the row named by its address.
        let (parent, key) = resource
            .split_once(':')
            .map_or((resource, ""), |(kind, key)| (kind, key));
        match parent {
            WIFI_NETWORK => {
                let action = decode::<WifiNetworkAction>(&published, resource, action, arguments)?;
                self.wifi_network(key, action)
            }
            SAVED_NETWORK => {
                let action = decode::<SavedNetworkAction>(&published, resource, action, arguments)?;
                self.saved_network(key, action)
            }
            NETWORK => {
                let action = decode::<NetworkAction>(&published, resource, action, arguments)?;
                self.network(action)
            }
            MODEM => {
                let action = decode::<ModemAction>(&published, resource, action, arguments)?;
                self.modem(action)
            }
            BLUETOOTH => {
                let action = decode::<BluetoothAction>(&published, resource, action, arguments)?;
                Ok(self.bluetooth(action))
            }
            BLUETOOTH_ACCESSORY => {
                let action =
                    decode::<BluetoothAccessoryAction>(&published, resource, action, arguments)?;
                Ok(self.bluetooth_accessory(key, action))
            }
            BLUETOOTH_NEARBY => {
                let action =
                    decode::<BluetoothNearbyAction>(&published, resource, action, arguments)?;
                self.bluetooth_nearby(key, action)
            }
            VPN => {
                let action = decode::<VpnAction>(&published, resource, action, arguments)?;
                Ok(self.vpn(action))
            }
            VPN_PROFILE => {
                let action = decode::<VpnProfileAction>(&published, resource, action, arguments)?;
                Ok(self.vpn_profile(key, action))
            }
            TETHERING => {
                let action = decode::<TetheringAction>(&published, resource, action, arguments)?;
                self.tethering(action)
            }
            _ => Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            ))),
        }
    }

    fn poll(&mut self) -> bool {
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        for landed in landed {
            self.land(landed);
        }
        changed
    }

    fn settle(&mut self) {
        for landed in self.worker.settle() {
            self.land(landed);
        }
    }
}

#[cfg(test)]
mod tests;

/// The `modem` resource reading `modem`.
#[must_use]
pub fn modem_resource(modem: &Modem) -> ResourceSpec {
    let mut resource = offer::<ModemAction>(tilo_resource::typed_resource::<ModemReading>(
        MODEM,
        crate::i18n::name("so.tilo.connectivity.modem", "Modem"),
    ));
    if !modem.available {
        for action in [
            "unlock-sim",
            "unblock-sim",
            "set-sim-pin-lock",
            "change-sim-pin",
            "use-sim",
            "set-preferred-network",
        ] {
            unavailable(
                &mut resource,
                action,
                "no_modem",
                "This device has no mobile modem",
            );
        }
    }
    resource
}

/// The metering `metered` names: `automatic`, `metered` or `unmetered`.
fn metered_from_key(metered: &str) -> Result<model::Metered, ApiError> {
    match metered.trim().to_ascii_lowercase().as_str() {
        "automatic" | "auto" | "" => Ok(model::Metered::Automatic),
        "metered" => Ok(model::Metered::Metered),
        "unmetered" => Ok(model::Metered::Unmetered),
        other => Err(ApiError::InvalidArguments(format!(
            "{other} is not a metering: automatic, metered or unmetered"
        ))),
    }
}

/// The entry `field` demands, else a refusal naming it.
fn demanded(value: Option<String>, field: &str) -> Result<String, ApiError> {
    given(value).ok_or_else(|| ApiError::InvalidArguments(format!("no {field} was given")))
}

/// The network types `key` names (`5g-4g-3g`, optionally `-prefer-4g`) as
/// `ModemManager` bits.
fn network_modes(key: &str) -> Result<os_actions::NetworkModes, String> {
    let generation_bit = |generation: u8| match generation {
        5 => Some(1 << 4),
        4 => Some(1 << 3),
        3 => Some(1 << 2),
        2 => Some(1 << 1),
        _ => None,
    };
    let (allowed, preferred) = key
        .trim()
        .split_once("-prefer-")
        .map_or_else(|| (key.trim(), None), |(allowed, preferred)| {
            (allowed, Some(preferred))
        });
    let mut allowed_bits = 0_u32;
    for part in allowed.split('-').filter(|part| !part.is_empty()) {
        let generation = part
            .trim_end_matches('g')
            .parse::<u8>()
            .map_err(|_| format!("{part} is not a network generation"))?;
        allowed_bits |= generation_bit(generation)
            .ok_or_else(|| format!("{part} is not a network generation: 5g, 4g, 3g or 2g"))?;
    }
    if allowed_bits == 0 {
        return Err("no network generation is named".into());
    }
    let preferred_bits = match preferred {
        Some(preferred) => {
            let generation = preferred
                .trim_end_matches('g')
                .parse::<u8>()
                .map_err(|_| format!("{preferred} is not a network generation"))?;
            generation_bit(generation)
                .filter(|bit| allowed_bits & bit != 0)
                .ok_or_else(|| format!("{preferred} is not one of the listed generations"))?
        }
        None => 0,
    };
    Ok(os_actions::NetworkModes {
        allowed: allowed_bits,
        preferred: preferred_bits,
    })
}

impl Connectivity {
    fn modem(&mut self, action: ModemAction) -> Result<Started, ApiError> {
        let id = action.id();
        let change = match action {
            ModemAction::RefreshModems => {
                refresh(&self.worker.jobs());
                return Ok(Started::Accepted);
            }
            ModemAction::UnlockSim { sim, pin } => {
                let pin = demanded(pin, "PIN")?;
                let sim =
                    self.sim_path(sim.as_deref(), |modem| modem.lock == model::SimLock::Pin)?;
                Change::ModemPin { sim, pin }
            }
            ModemAction::UnblockSim { sim, puk, pin } => {
                let puk = demanded(puk, "PUK")?;
                let new_pin = demanded(pin, "new PIN")?;
                let sim =
                    self.sim_path(sim.as_deref(), |modem| modem.lock == model::SimLock::Puk)?;
                Change::ModemPuk { sim, puk, new_pin }
            }
            ModemAction::SetSimPinLock { sim, on, pin } => {
                let pin = demanded(pin, "PIN")?;
                let sim = self.sim_path(sim.as_deref(), model::ModemUnit::usable)?;
                Change::ModemPinLock { sim, pin, on }
            }
            ModemAction::ChangeSimPin {
                sim,
                old_pin,
                new_pin,
            } => {
                let old_pin = demanded(old_pin, "current PIN")?;
                let new_pin = demanded(new_pin, "new PIN")?;
                let sim = self.sim_path(sim.as_deref(), model::ModemUnit::usable)?;
                Change::ModemChangePin {
                    sim,
                    old_pin,
                    new_pin,
                }
            }
            ModemAction::UseSim { sim } => {
                let (modem, slot) = self.sim_slot(&sim)?;
                Change::ModemUseSlot { modem, slot }
            }
            ModemAction::SetPreferredNetwork { modem, network } => {
                let modes = network_modes(&network).map_err(ApiError::InvalidArguments)?;
                let path = self.modem_path(modem.as_deref(), &network)?;
                Change::ModemModes { modem: path, modes }
            }
        };
        Ok(self.start(Part::Modem, id, change))
    }

    fn sim_path(
        &self,
        wanted: Option<&str>,
        fallback: impl Fn(&model::ModemUnit) -> bool,
    ) -> Result<String, ApiError> {
        let modems = &self.readings.modem.modems;
        if let Some(wanted) = wanted {
            return modems
                .iter()
                .flat_map(|modem| &modem.sims)
                .find(|sim| sim.path == wanted || sim.label == wanted)
                .map(|sim| sim.path.clone())
                .ok_or_else(|| ApiError::Unavailable {
                    code: "no_such_sim".into(),
                    reason: format!("{wanted} is not a SIM of this device"),
                });
        }
        modems
            .iter()
            .filter(|modem| fallback(modem))
            .find_map(|modem| modem.active_sim().map(|sim| sim.path.clone()))
            .ok_or_else(|| ApiError::Unavailable {
                code: "no_sim".into(),
                reason: "no SIM of this device takes the change".into(),
            })
    }

    fn sim_slot(&self, sim: &str) -> Result<(String, u32), ApiError> {
        for modem in &self.readings.modem.modems {
            let Some(held) = modem
                .sims
                .iter()
                .find(|held| held.path == sim || held.label == sim)
            else {
                continue;
            };
            return held.slot.map_or_else(
                || {
                    Err(ApiError::Unavailable {
                        code: "no_slot".into(),
                        reason: "this SIM is not in a slot that can be chosen".into(),
                    })
                },
                |slot| Ok((modem.path.clone(), slot)),
            );
        }
        Err(ApiError::Unavailable {
            code: "no_such_sim".into(),
            reason: format!("{sim} is not a SIM of this device"),
        })
    }

    fn modem_path(&self, wanted: Option<&str>, network: &str) -> Result<String, ApiError> {
        let modems = &self.readings.modem.modems;
        let offers =
            |modem: &model::ModemUnit| modem.network_types.iter().any(|key| key == network);
        modems
            .iter()
            .filter(|modem| wanted.is_none_or(|wanted| modem.path == wanted))
            .find(|modem| offers(modem))
            .map(|modem| modem.path.clone())
            .ok_or_else(|| ApiError::Unavailable {
                code: "no_such_network_type".into(),
                reason: format!("no modem offers {network}"),
            })
    }
}
