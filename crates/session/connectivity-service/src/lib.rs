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
use effects::{BluetoothAction, NetworkAction, TetheringAction, VpnAction};
use model::{Bluetooth, Network, Tethering, Vpn};

/// The resources' ids.
pub const NETWORK: &str = "network";
pub const BLUETOOTH: &str = "bluetooth";
pub const VPN: &str = "vpn";
pub const TETHERING: &str = "tethering";

/// Which resource a change belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Network,
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
        Ok(Follow {
            _network: network,
            _bluetooth: bluetooth,
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
        readings.vpn.pending = std::mem::take(&mut old.vpn.pending);
        readings.vpn.problem = old.vpn.problem.take();
        readings.tethering.pending = std::mem::take(&mut old.tethering.pending);
        readings.tethering.problem = old.tethering.problem.take();
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

    fn network(&mut self, action: NetworkAction) -> Result<Started, ApiError> {
        let id = action.id();
        let network = &self.readings.network;
        let change = match action {
            NetworkAction::SetWifi { on } => Change::Wifi(on),
            NetworkAction::ToggleWifi => Change::Wifi(!network.wifi.on),
            NetworkAction::ScanWifi => Change::Scan,
            NetworkAction::JoinWifi { network: name } => {
                let known = network.wifi.networks.iter().find(|n| n.name == name);
                if known.is_some_and(|n| n.secure && !n.known) {
                    return Err(ApiError::Unavailable {
                        code: "password_needed".into(),
                        reason: format!("{name} asks for a password: join it with connect-wifi"),
                    });
                }
                Change::Join {
                    network: name,
                    password: None,
                    hidden: false,
                }
            }
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
            NetworkAction::DisconnectWifi { network } => Change::Leave { network },
            NetworkAction::ForgetNetwork { profile } => Change::Forget { profile },
            NetworkAction::SetNetworkAutoconnect { profile, on } => {
                Change::Autoconnect { profile, on }
            }
            NetworkAction::SetNetworkProxy { profile, server } => Change::Proxy {
                profile,
                server: proxy(server)?,
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

    fn bluetooth(&mut self, action: BluetoothAction) -> Started {
        let id = action.id();
        let change = match action {
            BluetoothAction::SetBluetooth { on } => Change::Bluetooth(on),
            BluetoothAction::ToggleBluetooth => Change::Bluetooth(!self.readings.bluetooth.on),
            BluetoothAction::Connect { address } => Change::Connect { address },
            BluetoothAction::Disconnect { address } => Change::Disconnect { address },
            BluetoothAction::Scan { on } => Change::ScanBluetooth(on),
            BluetoothAction::Pair { address } => Change::Pair { address },
            BluetoothAction::Forget { address } => Change::ForgetBluetooth { address },
        };
        self.start(Part::Bluetooth, id, change)
    }

    fn vpn(&mut self, action: VpnAction) -> Started {
        let id = action.id();
        let change = match action {
            VpnAction::SetVpn { profile, on } => Change::Vpn { profile, on },
            VpnAction::ImportVpn { kind, name, text } => Change::ImportVpn { kind, name, text },
            VpnAction::ForgetVpn { profile } => Change::ForgetVpn { profile },
            VpnAction::EditVpn {
                profile,
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
}

fn refresh(jobs: &Jobs<dyn Device, Landed>) {
    jobs.run(|device| Landed::Read(Box::new(device.read())));
}

/// A child resource's id: its kind and its key.
fn child_id(kind: &str, key: &str) -> Option<String> {
    (!key.is_empty() && !key.contains(tilo_actions::resource::KEY_SEP))
        .then(|| format!("{kind}:{key}"))
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

/// The `network` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.network", label = "Network")]
pub struct NetworkReading;

/// The `bluetooth` resource's declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.connectivity.bluetooth", label = "Bluetooth")]
pub struct BluetoothReading;

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
    effects = NetworkAction
)]
pub struct WifiNetworkRow;

/// A network this device keeps, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.saved-network",
    label = "Saved network",
    effects = NetworkAction
)]
pub struct SavedNetworkRow;

/// A paired Bluetooth accessory, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.bluetooth-accessory",
    label = "Bluetooth accessory",
    effects = BluetoothAction
)]
pub struct BluetoothAccessoryRow;

/// A Bluetooth device in reach, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.bluetooth-nearby",
    label = "Bluetooth device",
    effects = BluetoothAction
)]
pub struct BluetoothNearbyRow;

/// A VPN profile, as a declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.connectivity.vpn-profile",
    label = "VPN profile",
    effects = VpnAction
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

/// The networks in reach and the networks this device keeps, each with what
/// pressing it does now.
fn network_children(network: &Network) -> impl Iterator<Item = ResourceSpec> + '_ {
    let reachable = network.wifi.networks.iter().filter_map(|wifi| {
        let id = child_id("wifi-network", &wifi.name)?;
        let action = if wifi.connected {
            NetworkAction::DISCONNECT_WIFI
        } else {
            NetworkAction::JOIN_WIFI
        };
        let bindings =
            tilo_resource::RowBindings::new().bind(action, [("network", wifi.name.clone().into())]);
        Some(
            tilo_resource::LiveRow::typed_with::<WifiNetworkRow>(id, wifi.name.clone(), &bindings)
                .into_resource(),
        )
    });
    let saved = network.saved.iter().filter_map(|profile| {
        let id = child_id("saved-network", &profile.id)?;
        let mut bindings = tilo_resource::RowBindings::new()
            .bind(
                NetworkAction::SET_NETWORK_AUTOCONNECT,
                [
                    ("profile", profile.id.clone().into()),
                    ("on", (!profile.autoconnect).into()),
                ],
            )
            .bind(
                NetworkAction::FORGET_NETWORK,
                [("profile", profile.id.clone().into())],
            );
        for action in [
            NetworkAction::SET_NETWORK_IPV4,
            NetworkAction::SET_NETWORK_IPV6,
            NetworkAction::SET_NETWORK_DNS,
            NetworkAction::SET_NETWORK_POLICY,
            NetworkAction::SET_ENTERPRISE_IDENTITY,
            NetworkAction::SET_WIFI_PASSWORD,
        ] {
            bindings = bindings.bind(action, [("profile", profile.id.clone().into())]);
        }
        if profile.proxy.is_some() {
            bindings = bindings.bind(
                NetworkAction::SET_NETWORK_PROXY,
                [
                    ("profile", profile.id.clone().into()),
                    ("server", Value::Null),
                ],
            );
        }
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
        for action in [
            "set-bluetooth",
            "toggle-bluetooth",
            "scan",
            "pair",
            "forget",
        ] {
            unavailable(
                &mut resource,
                action,
                "no_adapter",
                "This device has no Bluetooth",
            );
        }
    } else if !bluetooth.on {
        for action in ["scan", "pair"] {
            unavailable(&mut resource, action, "bluetooth_off", "Bluetooth is off");
        }
    }
    let paired = bluetooth.accessories.iter().filter_map(|accessory| {
        let id = child_id("bluetooth-accessory", &accessory.address)?;
        let action = if accessory.connected {
            BluetoothAction::DISCONNECT
        } else {
            BluetoothAction::CONNECT
        };
        let address = accessory.address.clone();
        let bindings = tilo_resource::RowBindings::new()
            .bind(action, [("address", address.clone().into())])
            .bind(BluetoothAction::FORGET, [("address", address.into())]);
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
        let id = child_id("bluetooth-nearby", &nearby.address)?;
        let bindings = tilo_resource::RowBindings::new().bind(
            BluetoothAction::PAIR,
            [("address", nearby.address.clone().into())],
        );
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
    let mut resource = offer::<VpnAction>(tilo_resource::typed_resource::<VpnReading>(
        VPN,
        crate::i18n::name("so.tilo.connectivity.vpn", "VPN"),
    ));
    if vpn.profiles.is_empty() {
        unavailable(
            &mut resource,
            "set-vpn",
            "no_profile",
            "This device has no VPN profile",
        );
    }
    let children = vpn.profiles.iter().filter_map(|profile| {
        let id = child_id("vpn-profile", &profile.id)?;
        let bindings = tilo_resource::RowBindings::new()
            .bind(
                VpnAction::SET_VPN,
                [
                    ("profile", profile.id.clone().into()),
                    ("on", (!profile.connected).into()),
                ],
            )
            .bind(
                VpnAction::FORGET_VPN,
                [("profile", profile.id.clone().into())],
            )
            .bind(
                VpnAction::EDIT_VPN,
                [("profile", profile.id.clone().into())],
            );
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
            vpn_resource(&readings.vpn),
            tethering_resource(&readings.tethering),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let readings = &self.readings;
        match resource {
            NETWORK => Some(reading(&readings.network)),
            BLUETOOTH => Some(reading(&readings.bluetooth)),
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
        // A child's action belongs to its parent's vocabulary.
        let parent = resource.split_once(':').map_or(resource, |(kind, _)| kind);
        match parent {
            NETWORK | "wifi-network" | "saved-network" => {
                let action = decode::<NetworkAction>(&published, resource, action, arguments)?;
                self.network(action)
            }
            BLUETOOTH | "bluetooth-accessory" | "bluetooth-nearby" => {
                let action = decode::<BluetoothAction>(&published, resource, action, arguments)?;
                Ok(self.bluetooth(action))
            }
            VPN | "vpn-profile" => {
                let action = decode::<VpnAction>(&published, resource, action, arguments)?;
                Ok(self.vpn(action))
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
