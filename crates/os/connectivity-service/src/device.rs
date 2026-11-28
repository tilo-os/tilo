//! The device under the service: what it reads and what it changes. The
//! session's is [`OsDevice`] (`NetworkManager` and `BlueZ` through
//! `os-actions`); a test lends one of its own.

use crate::model::{
    Accessory, Band, Bluetooth, Client, Connectivity, Link, Metered, Mobile, MobileProfile, Modem,
    ModemUnit, Nearby, Network, NetworkWake, SavedEnterprise, SavedIpv4, SavedIpv6, SavedNetwork,
    Tethering, Vpn, VpnPluginKind, VpnProfile, Wifi, WifiNetwork, Wired, ordered,
};

/// A change to make on the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Wifi(bool),
    Airplane(bool),
    Wired(bool),
    MobileData(bool),
    Hotspot(bool),
    /// Name the hotspot, set its password and band, and turn it on.
    ConfigureHotspot {
        name: String,
        password: String,
        band: Band,
    },
    /// Look for networks again.
    Scan,
    /// Join a network, with its password when it asks for one; `hidden` when
    /// it does not broadcast its name.
    Join {
        network: String,
        password: Option<String>,
        hidden: bool,
    },
    Leave {
        network: String,
    },
    /// Join an enterprise (802.1X) network with its identity and the
    /// organization's certificates, and connect.
    Enterprise {
        config: os_actions::EnterpriseWifiConfiguration,
        password: Option<String>,
    },
    /// Forget a saved network.
    Forget {
        profile: String,
    },
    /// Join a saved network by itself, or stop.
    Autoconnect {
        profile: String,
        on: bool,
    },
    /// Keep the open networks this device saves from joining by itself, or let each join as its
    /// own setting says.
    OpenNetworks {
        ask: bool,
    },
    /// Send a saved network's traffic through a proxy, or straight out.
    Proxy {
        profile: String,
        server: Option<String>,
    },
    /// Wake this device when a wake packet arrives over a saved network, or stop.
    Wake {
        profile: String,
        on: bool,
    },
    /// Share a saved Wi-Fi network's password with the person's other devices: only a device that
    /// can seal it to them does ([`OsDevice`] cannot).
    ShareWifi {
        profile: String,
    },
    /// Type the mobile data profile's access point name, or choose it from
    /// the SIM's operator again.
    MobileApn {
        apn: Option<String>,
    },
    /// Let mobile data connect on another operator's network, or stop.
    MobileRoaming(bool),
    /// Count mobile data against a limit, or let the network say.
    MobileMetered {
        metered: Metered,
    },
    Bluetooth(bool),
    Connect {
        address: String,
    },
    Disconnect {
        address: String,
    },
    /// Look for Bluetooth devices, or stop.
    ScanBluetooth(bool),
    /// Pair, trust and connect the device at `address`.
    Pair {
        address: String,
    },
    /// Forget the device at `address`.
    ForgetBluetooth {
        address: String,
    },
    /// Rename the device at `address`, as the person calls it.
    RenameBluetooth {
        address: String,
        name: String,
    },
    /// Let the device at `address` connect by itself, or ask first.
    TrustBluetooth {
        address: String,
        trusted: bool,
    },
    Vpn {
        profile: String,
        on: bool,
    },
    /// Add the VPN in `text`, a configuration file's contents.
    ImportVpn {
        kind: String,
        name: String,
        text: String,
    },
    ForgetVpn {
        profile: String,
    },
    /// Change a VPN profile; what the patch leaves out stays.
    EditVpn {
        profile: String,
        patch: os_actions::network_settings::VpnProfilePatch,
    },
    /// Route every connection over Tor, or stop.
    SetTor {
        on: bool,
    },
    /// How a saved network gets its IPv4 address.
    Ipv4 {
        profile: String,
        config: os_actions::IpConfiguration,
    },
    /// How a saved network gets its IPv6 address.
    Ipv6 {
        profile: String,
        config: os_actions::network_settings::Ipv6Configuration,
    },
    /// The name servers a saved network uses.
    Dns {
        profile: String,
        servers: Vec<String>,
    },
    /// How a saved network is used; what the patch leaves out stays.
    Policy {
        profile: String,
        patch: os_actions::ConnectionPolicyPatch,
    },
    /// Who a work or school network knows this device as.
    Identity {
        profile: String,
        identity: os_actions::network_settings::EnterpriseIdentity,
    },
    /// The password a saved Wi-Fi network joins with.
    WifiPassword {
        profile: String,
        password: String,
    },
    /// Make a profile of an installed plugin's type.
    CreatePluginVpn {
        service: String,
        name: String,
        server: String,
    },
    /// Unlock a SIM with its PIN.
    ModemPin {
        sim: String,
        pin: String,
    },
    /// Unblock a SIM with its PUK, choosing a new PIN.
    ModemPuk {
        sim: String,
        puk: String,
        new_pin: String,
    },
    /// Ask for a SIM's PIN when the device starts, or stop asking.
    ModemPinLock {
        sim: String,
        pin: String,
        on: bool,
    },
    /// Change a SIM's PIN.
    ModemChangePin {
        sim: String,
        old_pin: String,
        new_pin: String,
    },
    /// Use a modem's SIM slot for mobile data.
    ModemUseSlot {
        modem: String,
        slot: u32,
    },
    /// The network types a modem may use.
    ModemModes {
        modem: String,
        modes: os_actions::NetworkModes,
    },
    /// Stand on the operator's network whose MCC and MNC `operator` holds, or on one the modem
    /// chooses when it is `None`.
    ModemRegister {
        modem: String,
        operator: Option<String>,
    },
}

/// What the device reads now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readings {
    pub network: Network,
    pub bluetooth: Bluetooth,
    pub modem: Modem,
    pub vpn: Vpn,
    pub tethering: Tethering,
}

/// The device: reads and changes. Called on the service's worker, never
/// on the session's loop.
pub trait Device: Send + Sync {
    /// Read everything.
    fn read(&self) -> Readings;

    /// Make `change`.
    ///
    /// # Errors
    /// Why the device did not make it.
    fn change(&self, change: &Change) -> Result<(), String>;

    /// The operators the modem at `modem` can reach now. A device without a
    /// modem answers that.
    ///
    /// # Errors
    /// Why the modem could not be asked.
    fn scan_operators(&self, _modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        Err(no_modem())
    }

    /// Send `command` to the network, as `*100#` asks, and read what it answers.
    ///
    /// # Errors
    /// Why the network could not be asked.
    fn send_code(&self, _modem: &str, _command: &str) -> Result<os_actions::CodeAnswer, String> {
        Err(no_modem())
    }

    /// Answer the question the network asked.
    ///
    /// # Errors
    /// Why the network could not be answered.
    fn answer_code(&self, _modem: &str, _response: &str) -> Result<os_actions::CodeAnswer, String> {
        Err(no_modem())
    }

    /// End the code's session with the network.
    ///
    /// # Errors
    /// Why the session could not be ended.
    fn cancel_code(&self, _modem: &str) -> Result<(), String> {
        Err(no_modem())
    }
}

/// Why a device that has no modem cannot be asked anything of one.
fn no_modem() -> String {
    "this device has no modem".to_string()
}

/// The device as `NetworkManager` and `BlueZ` report it.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsDevice;

/// `result` as an option, saying why it is missing in the journal.
fn read_or_log<T>(what: &str, result: Result<T, String>) -> Option<T> {
    result
        .map_err(|error| tracing::debug!(%error, what, "a connectivity reading failed"))
        .ok()
}

/// `profile` with the settings its own page reads and changes.
fn with_vpn_settings(mut profile: VpnProfile) -> VpnProfile {
    let Some(settings) = read_or_log(
        "vpn settings",
        os_actions::network_settings::vpn_profile_settings(&profile.id),
    ) else {
        return profile;
    };
    profile.service = settings.service;
    profile.autoconnect = settings.autoconnect;
    profile.all_traffic = settings.all_traffic;
    profile.addresses = settings.addresses;
    profile.dns = settings.dns;
    profile.mtu = settings.mtu;
    profile.endpoint = settings.endpoint;
    profile.allowed_ips = settings.allowed_ips;
    profile.peer_public_key = settings.peer_public_key;
    profile.keepalive = settings.keepalive;
    profile.tor = settings.tor;
    profile
}

/// The networks this device keeps, each with the settings a person changes.
fn saved_networks() -> Vec<SavedNetwork> {
    let profiles = read_or_log("saved networks", os_actions::list_saved_network_profiles())
        .unwrap_or_default();
    profiles
        .iter()
        .map(SavedNetwork::from)
        .map(with_settings)
        .collect()
}

/// `saved` with the per-profile settings `NetworkManager` has for it.
fn with_settings(mut saved: SavedNetwork) -> SavedNetwork {
    let Some(settings) = read_or_log(
        "saved network settings",
        os_actions::connection_profile_settings(&saved.profile),
    ) else {
        return saved;
    };
    saved.priority = settings.priority.unwrap_or_default();
    if let Some(metered) = settings.metered {
        saved.metered = Metered::from(metered);
    }
    saved.proxy = settings.proxy.and_then(|proxy| proxy.manual);
    saved.hidden = settings.hidden.unwrap_or_default();
    saved.open = settings.open;
    saved.ipv4 = settings.ipv4.as_ref().map(SavedIpv4::from);
    saved.ipv6 = settings.ipv6.as_ref().map(SavedIpv6::from);
    saved.mac = settings.mac.map(|policy| {
        match policy {
            os_actions::MacPolicy::Permanent => "permanent",
            os_actions::MacPolicy::Random => "random",
            os_actions::MacPolicy::Stable => "stable",
        }
        .to_owned()
    });
    saved.enterprise = settings.enterprise.as_ref().map(SavedEnterprise::from);
    saved.wake = read_or_log(
        "network wake",
        os_actions::network_wake::status(&saved.profile),
    )
    .map(|status| NetworkWake::from(&status));
    saved
}

/// The hotspot's name and band, and who shares it while it is on.
fn tethering(available: bool, hotspot: bool) -> Tethering {
    let details = read_or_log("hotspot", os_actions::hotspot_details()).flatten();
    let clients = if hotspot {
        read_or_log("hotspot clients", os_actions::list_hotspot_clients())
            .unwrap_or_default()
            .iter()
            .map(Client::from)
            .collect()
    } else {
        Vec::new()
    };
    Tethering {
        available,
        hotspot,
        name: details
            .as_ref()
            .map(|details| details.ssid.clone())
            .filter(|name| !name.is_empty()),
        band: details.map_or(Band::Automatic, |details| Band::from(details.band)),
        clients,
        ..Tethering::default()
    }
}

/// The Bluetooth devices in reach that are not paired, strongest first.
fn nearby() -> Vec<Nearby> {
    let mut devices: Vec<Nearby> =
        read_or_log("bluetooth devices in reach", os_actions::bluez::devices())
            .unwrap_or_default()
            .iter()
            .filter(|device| !device.paired)
            .map(Nearby::from)
            .collect();
    devices.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.name.cmp(&b.name)));
    devices
}

impl Device for OsDevice {
    #[allow(
        clippy::too_many_lines,
        reason = "one read of every resource this device publishes, in the order it reads them"
    )]
    fn read(&self) -> Readings {
        let radios = read_or_log("radios", os_actions::get_radio_state()).unwrap_or_default();
        let networks = if radios.wifi_enabled {
            read_or_log("wifi networks", os_actions::list_wifi_networks())
                .unwrap_or_default()
                .iter()
                .map(WifiNetwork::from)
                .collect()
        } else {
            Vec::new()
        };
        let wired = read_or_log("wired", os_actions::get_wired())
            .filter(|wired| wired.device.is_some())
            .map(|wired| Wired::from(&wired));
        let connectivity = read_or_log("connectivity", os_actions::get_connectivity())
            .map_or(Connectivity::Unknown, Connectivity::from);
        // A portal's sign-in page is part of the portal: read it while there is one.
        let (portal_uri, portal_problem) = if connectivity == Connectivity::Portal {
            match os_actions::captive_portal_uri() {
                Ok(uri) => (Some(uri), None),
                Err(problem) => (None, Some(problem)),
            }
        } else {
            (None, None)
        };
        let link = read_or_log("link", os_actions::get_link_status()).map(|link| Link::from(&link));
        let vpn_profiles =
            read_or_log("vpn profiles", os_actions::list_vpn_connections()).unwrap_or_default();
        let mut profiles: Vec<VpnProfile> = vpn_profiles
            .iter()
            .map(VpnProfile::from)
            .map(with_vpn_settings)
            .collect();
        // The tunnel's own telemetry, read from the interface it crosses: its counters and,
        // for WireGuard, how long ago it last handshook.
        for (profile, info) in profiles.iter_mut().zip(&vpn_profiles) {
            let Some(interface) = &info.interface else {
                continue;
            };
            if let Some((received, sent)) =
                os_actions::network_settings::interface_transfer(interface)
            {
                profile.received = received;
                profile.sent = sent;
            }
            profile.handshake = os_actions::network_settings::vpn_handshake_secs(interface);
        }
        let adapter = os_actions::bluez::has_adapter();
        let bluetooth_on =
            adapter && read_or_log("bluetooth", os_actions::bluez::is_powered()).unwrap_or(false);
        let accessories = if bluetooth_on {
            read_or_log("bluetooth accessories", os_actions::bluez::paired_devices())
                .unwrap_or_default()
                .iter()
                .map(Accessory::from)
                .collect()
        } else {
            Vec::new()
        };
        let discovering = bluetooth_on
            && read_or_log("bluetooth discovery", os_actions::bluez::is_scanning())
                .unwrap_or(false);
        Readings {
            network: Network {
                wifi: Wifi {
                    available: radios.wifi_available,
                    on: radios.wifi_enabled,
                    hardware_blocked: radios.wifi_hardware_blocked,
                    networks: ordered(networks),
                    scan_problem: None,
                },
                wired,
                mobile: Mobile {
                    available: radios.wwan_available,
                    on: radios.wwan_enabled,
                    profile: read_or_log("mobile profile", os_actions::read_mobile_profile())
                        .flatten()
                        .map(|profile| MobileProfile::from(&profile)),
                },
                airplane_mode: radios.airplane_mode,
                link,
                connectivity,
                portal_uri,
                portal_problem,
                saved: saved_networks(),
                ..Network::default()
            },
            bluetooth: Bluetooth {
                available: adapter,
                on: bluetooth_on,
                accessories,
                nearby: if bluetooth_on { nearby() } else { Vec::new() },
                discovering,
                ..Bluetooth::default()
            },
            modem: Modem {
                available: radios.wwan_available,
                modems: read_or_log("modems", os_actions::read_mobile().map_err(String::from))
                    .unwrap_or_default()
                    .modems
                    .iter()
                    .map(ModemUnit::from)
                    .collect(),
                ..Modem::default()
            },
            vpn: Vpn {
                profiles,
                plugins: os_actions::network_settings::installed_vpn_plugins()
                    .iter()
                    .map(VpnPluginKind::from)
                    .collect(),
                tor_available: os_actions::network_settings::tor_available(),
                tor: os_actions::network_settings::tor_active(),
                ..Vpn::default()
            },
            tethering: tethering(radios.wifi_available, radios.hotspot_active),
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per change the device makes")]
    fn change(&self, change: &Change) -> Result<(), String> {
        use os_actions::RadioChange;
        match change {
            Change::Wifi(on) => radio(RadioChange::Wifi(*on)),
            Change::Airplane(on) => radio(RadioChange::Airplane(*on)),
            Change::Wired(on) => os_actions::set_wired(*on),
            Change::MobileData(on) => os_actions::set_mobile_broadband(*on),
            Change::Hotspot(on) => os_actions::set_hotspot(*on),
            Change::ConfigureHotspot {
                name,
                password,
                band,
            } => os_actions::configure_hotspot(&os_actions::HotspotConfiguration {
                ssid: name.clone(),
                password: password.clone(),
                band: (*band).into(),
            }),
            Change::Scan => {
                os_actions::scan_wifi()?;
                // A search is a deliberate look: ask the daemon to check the
                // internet again too, so the reading says what it answers now
                // rather than what it said before.
                let _ = os_actions::check_connectivity_now();
                Ok(())
            }
            Change::Join {
                network,
                password,
                hidden,
            } => {
                if *hidden {
                    os_actions::network_settings::connect_hidden_wifi(network, password.as_deref())
                } else {
                    os_actions::connect_wifi(network, password.as_deref())
                }
            }
            Change::Leave { network } => os_actions::disconnect_wifi(network),
            Change::Enterprise { config, password } => {
                os_actions::connect_enterprise_wifi(config, password.as_deref().unwrap_or_default())
            }
            Change::Forget { profile } => os_actions::forget_network_profile(profile),
            Change::Autoconnect { profile, on } => {
                os_actions::set_network_autoconnect(profile, *on)
            }
            Change::OpenNetworks { ask } => os_actions::set_open_wifi_autoconnect(!*ask),
            Change::Proxy { profile, server } => os_actions::configure_connection_policy_patch(
                profile,
                &os_actions::ConnectionPolicyPatch {
                    proxy: Some(os_actions::ProxyConfiguration {
                        automatic_url: None,
                        manual: server.clone(),
                        browser_only: false,
                    }),
                    ..os_actions::ConnectionPolicyPatch::default()
                },
            ),
            Change::MobileApn { apn } => {
                let profile = os_actions::modem::mobile_profile_uuid()?;
                os_actions::set_mobile_apn(&profile, apn.as_deref())
            }
            Change::MobileRoaming(on) => {
                let profile = os_actions::modem::mobile_profile_uuid()?;
                os_actions::set_mobile_roaming(&profile, *on)
            }
            Change::MobileMetered { metered } => {
                let profile = os_actions::modem::mobile_profile_uuid()?;
                os_actions::configure_connection_policy_patch(
                    &profile,
                    &os_actions::ConnectionPolicyPatch {
                        metered: Some((*metered).into()),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                )
            }
            Change::Bluetooth(on) => os_actions::bluez::set_powered(*on),
            Change::Connect { address } => os_actions::bluez::connect_address(address),
            Change::Disconnect { address } => os_actions::bluez::disconnect_address(address),
            Change::ScanBluetooth(true) => os_actions::bluez::start_scan(),
            Change::ScanBluetooth(false) => os_actions::bluez::stop_scan(),
            Change::Pair { address } => os_actions::bluez::pair_handoff(address),
            Change::ForgetBluetooth { address } => os_actions::bluez::forget_address(address),
            Change::RenameBluetooth { address, name } => {
                os_actions::bluez::rename_address(address, name)
            }
            Change::TrustBluetooth { address, trusted } => {
                os_actions::bluez::set_trusted_address(address, *trusted)
            }
            Change::Vpn { profile, on } => os_actions::set_vpn_connection_uuid(profile, *on),
            Change::ImportVpn { kind, name, text } => {
                os_actions::network_settings::import_vpn_text(kind, name, text).map(|_| ())
            }
            Change::ForgetVpn { profile } => os_actions::delete_vpn_connection_uuid(profile),
            Change::EditVpn { profile, patch } => {
                os_actions::network_settings::configure_vpn_profile(profile, patch)
            }
            Change::SetTor { on } => os_actions::network_settings::set_tor(*on),
            Change::Ipv4 { profile, config } => os_actions::configure_ipv4(profile, config),
            Change::Ipv6 { profile, config } => {
                os_actions::network_settings::configure_ipv6(profile, config)
            }
            Change::Dns { profile, servers } => {
                os_actions::network_settings::configure_dns(profile, servers)
            }
            Change::Policy { profile, patch } => {
                os_actions::configure_connection_policy_patch(profile, patch)
            }
            Change::Identity { profile, identity } => {
                os_actions::network_settings::configure_enterprise_identity(profile, identity)
            }
            Change::WifiPassword { profile, password } => {
                os_actions::network_settings::set_wifi_profile_password(profile, password)
            }
            Change::Wake { profile, on } => {
                // A problem applying it after the profile saved is the reading's, not a refusal.
                os_actions::network_wake::configure(profile, *on).map(|_| ())
            }
            Change::ShareWifi { .. } => {
                Err("this device has no way to reach the person's other devices".to_owned())
            }
            Change::CreatePluginVpn {
                service,
                name,
                server,
            } => os_actions::network_settings::create_plugin_vpn(service, name, server).map(|_| ()),
            Change::ModemPin { sim, pin } => {
                os_actions::apply_modem_change(&os_actions::ModemChange::SendPin {
                    sim: sim.clone(),
                    pin: zeroize::Zeroizing::new(pin.clone()),
                })
                .map_err(String::from)
            }
            Change::ModemPuk { sim, puk, new_pin } => {
                os_actions::apply_modem_change(&os_actions::ModemChange::SendPuk {
                    sim: sim.clone(),
                    puk: zeroize::Zeroizing::new(puk.clone()),
                    new_pin: zeroize::Zeroizing::new(new_pin.clone()),
                })
                .map_err(String::from)
            }
            Change::ModemPinLock { sim, pin, on } => {
                os_actions::apply_modem_change(&os_actions::ModemChange::SetPinLock {
                    sim: sim.clone(),
                    pin: zeroize::Zeroizing::new(pin.clone()),
                    enabled: *on,
                })
                .map_err(String::from)
            }
            Change::ModemChangePin {
                sim,
                old_pin,
                new_pin,
            } => os_actions::apply_modem_change(&os_actions::ModemChange::ChangePin {
                sim: sim.clone(),
                old_pin: zeroize::Zeroizing::new(old_pin.clone()),
                new_pin: zeroize::Zeroizing::new(new_pin.clone()),
            })
            .map_err(String::from),
            Change::ModemUseSlot { modem, slot } => {
                os_actions::apply_modem_change(&os_actions::ModemChange::UseSlot {
                    modem: modem.clone(),
                    slot: *slot,
                })
                .map_err(String::from)
            }
            Change::ModemModes { modem, modes } => {
                os_actions::apply_modem_change(&os_actions::ModemChange::SetModes {
                    modem: modem.clone(),
                    modes: *modes,
                })
                .map_err(String::from)
            }
            Change::ModemRegister { modem, operator } => {
                os_actions::apply_modem_change(&os_actions::ModemChange::Register {
                    modem: modem.clone(),
                    operator: operator.clone(),
                })
                .map_err(String::from)
            }
        }
    }

    fn scan_operators(&self, modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        os_actions::scan_operators(modem).map_err(String::from)
    }

    fn send_code(&self, modem: &str, command: &str) -> Result<os_actions::CodeAnswer, String> {
        os_actions::send_code(modem, command).map_err(String::from)
    }

    fn answer_code(&self, modem: &str, response: &str) -> Result<os_actions::CodeAnswer, String> {
        os_actions::answer_code(modem, response).map_err(String::from)
    }

    fn cancel_code(&self, modem: &str) -> Result<(), String> {
        os_actions::cancel_code(modem).map_err(String::from)
    }
}

/// Make a radio change and check the radios show it: a switch may hold a
/// radio off although the change was taken.
fn radio(change: os_actions::RadioChange) -> Result<(), String> {
    change.outcome(&change.apply()?)
}
