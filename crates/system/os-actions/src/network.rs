//! Wi-Fi, radios, VPNs and saved networks, through `NetworkManager`'s D-Bus client
//! [`tilo_nm`].

use std::future::Future;
use std::time::Duration;

use tilo_nm::{
    AccessPoint, ActiveConnection, Device, DeviceKind, IpFamily, NmClient, NmError, Profile,
    Radios, Settings,
};
use zeroize::Zeroizing;

pub const HOTSPOT_PROFILE: &str = "Tilo Hotspot";

/// Association, authentication and an address; beyond this a join counts as failed.
const WIFI_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const HOTSPOT_TIMEOUT: Duration = Duration::from_secs(30);
/// A work or school network adds the 802.1X exchange to a join.
const ENTERPRISE_CONNECT_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) fn with_nm<T, F>(work: impl FnOnce(NmClient) -> F) -> Result<T, String>
where
    F: Future<Output = Result<T, NmError>>,
{
    crate::block_on(async move {
        let nm = NmClient::system()
            .await
            .map_err(|error| error.to_string())?;
        work(nm).await.map_err(|error| error.to_string())
    })
}

async fn profile_named(nm: &NmClient, id: &str) -> Result<Profile, NmError> {
    nm.profiles()
        .await?
        .into_iter()
        .find(|profile| profile.settings.id() == id)
        .ok_or_else(|| NmError::NotFound(format!("no saved network is called {id}")))
}

async fn profile_or_add(
    nm: &NmClient,
    id: &str,
    new: impl FnOnce() -> Settings + Send,
) -> Result<Profile, NmError> {
    if let Ok(profile) = profile_named(nm, id).await {
        return Ok(profile);
    }
    let uuid = nm.add(new()).await?;
    nm.profile(&uuid).await
}

async fn active_where(
    nm: &NmClient,
    matches: impl Fn(&ActiveConnection) -> bool,
) -> Result<ActiveConnection, NmError> {
    nm.active_connections()
        .await?
        .into_iter()
        .find(matches)
        .ok_or_else(|| NmError::NotFound("that connection is not active".into()))
}

async fn wifi_device(nm: &NmClient) -> Result<Device, NmError> {
    nm.devices()
        .await?
        .into_iter()
        .find(|device| device.kind == DeviceKind::Wifi)
        .ok_or(NmError::NoWifiDevice)
}

// Six independently meaningful flags, not a bitset in disguise: each is read and written by name
// across the shell and Settings.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent, individually named radio/availability flag read \
              across shell and apps/settings; grouping them would only relocate the same state"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioState {
    pub airplane_mode: bool,
    pub wifi_available: bool,
    /// Really on: `NetworkManager` has it enabled and no kill switch blocks it, which is not the last
    /// request.
    pub wifi_enabled: bool,
    /// A physical switch holds Wi-Fi off; no program can turn it on.
    pub wifi_hardware_blocked: bool,
    pub wwan_available: bool,
    pub wwan_enabled: bool,
    pub hotspot_active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnConnectionInfo {
    pub name: String,
    /// Consumers use this to associate locally provisioned VPN material; names are mutable labels.
    pub uuid: String,
    pub active: bool,
    pub kind: String,
}

pub fn get_radio_state() -> Result<RadioState, String> {
    SystemRadio.radio_state()
}

/// Whether this device runs a network service at all: `NetworkManager` is on the system bus, and
/// without it a caller leaves out what only it can answer.
#[must_use]
pub fn network_service_present() -> bool {
    crate::block_on(async {
        Ok::<bool, String>(match NmClient::system().await {
            Ok(nm) => nm.running().await,
            Err(_) => false,
        })
    })
    .unwrap_or(false)
}

fn nm_radio_state() -> Result<RadioState, String> {
    with_nm(|nm| async move {
        let radios = nm.radios().await?;
        let devices = nm.devices().await?;
        let active = nm.active_connections().await.unwrap_or_default();
        Ok(radio_state_of(radios, &devices, &active))
    })
}

/// Corrects `NetworkManager`'s view by the kernel's switches: a radio a switch blocks is off whatever
/// its flag says.
fn with_kill_switches(mut radios: RadioState, wlan: crate::rfkill::RfkillState) -> RadioState {
    if wlan.present {
        radios.wifi_enabled &= wlan.allows();
        radios.wifi_hardware_blocked = wlan.hard_blocked;
        radios.airplane_mode = every_radio_off(&radios);
    }
    radios
}

/// The radios as `NetworkManager` has them: its switches, the devices it manages and whether the
/// hotspot is up.
fn radio_state_of(radios: Radios, devices: &[Device], active: &[ActiveConnection]) -> RadioState {
    let has = |kind: DeviceKind| devices.iter().any(|device| device.kind == kind);
    let hotspot_active = active
        .iter()
        .any(|connection| connection.id == HOTSPOT_PROFILE && connection.kind == "802-11-wireless");
    let mut state = RadioState {
        wifi_available: has(DeviceKind::Wifi),
        wifi_enabled: radios.wifi_enabled,
        wwan_available: has(DeviceKind::Modem),
        wwan_enabled: radios.wwan_enabled,
        hotspot_active,
        ..RadioState::default()
    };
    state.airplane_mode = every_radio_off(&state);
    state
}

fn every_radio_off(radios: &RadioState) -> bool {
    (radios.wifi_available || radios.wwan_available)
        && (!radios.wifi_available || !radios.wifi_enabled)
        && (!radios.wwan_available || !radios.wwan_enabled)
}

const SYS_CLASS: &str = "/sys/class";

/// A wireless PHY or a WLAN interface, as the kernel lists them: a few directory listings answer it
/// without asking `NetworkManager`.
#[must_use]
pub fn has_wifi_radio() -> bool {
    has_wifi_radio_in(std::path::Path::new(SYS_CLASS))
}

/// A WWAN device or a network interface that is one, as the kernel lists them.
#[must_use]
pub fn has_wwan_modem() -> bool {
    has_wwan_modem_in(std::path::Path::new(SYS_CLASS))
}

fn has_wifi_radio_in(class: &std::path::Path) -> bool {
    has_entries(&class.join("ieee80211"))
        || interfaces(class).any(|interface| {
            interface.join("wireless").exists()
                || device_type(&interface).as_deref() == Some("wlan")
        })
}

fn has_wwan_modem_in(class: &std::path::Path) -> bool {
    has_entries(&class.join("wwan"))
        || interfaces(class).any(|interface| device_type(&interface).as_deref() == Some("wwan"))
}

fn has_entries(directory: &std::path::Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.any(|entry| entry.is_ok()))
}

fn interfaces(class: &std::path::Path) -> impl Iterator<Item = std::path::PathBuf> {
    std::fs::read_dir(class.join("net"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
}

fn device_type(interface: &std::path::Path) -> Option<String> {
    let uevent = std::fs::read_to_string(interface.join("uevent")).ok()?;
    uevent
        .lines()
        .find_map(|line| line.strip_prefix("DEVTYPE="))
        .map(str::to_owned)
}

pub fn set_airplane_mode(enabled: bool) -> Result<(), String> {
    if enabled {
        with_nm(|nm| async move {
            nm.set_wifi_enabled(false).await?;
            nm.set_wwan_enabled(false).await
        })
    } else {
        // Restores radios one by one, so Bluetooth stays `BlueZ`'s and `NetworkManager` never changes
        // an unrelated subsystem.
        set_wifi_radio(true)?;
        let _ = with_nm(|nm| async move { nm.set_wwan_enabled(true).await });
        Ok(())
    }
}

pub fn set_mobile_broadband(enabled: bool) -> Result<(), String> {
    crate::modem::set_mobile_data(enabled)
}

/// On first use its profile is made with a random WPA passphrase that Tilo never shows or logs.
pub fn set_hotspot(enabled: bool) -> Result<(), String> {
    with_nm(|nm| async move {
        if !enabled {
            let active = active_where(&nm, |active| active.id == HOTSPOT_PROFILE).await?;
            return nm.deactivate(&active).await;
        }
        if let Ok(profile) = profile_named(&nm, HOTSPOT_PROFILE).await {
            return nm
                .activate(&profile, None, None, Some(HOTSPOT_TIMEOUT))
                .await;
        }
        let device = wifi_device(&nm).await?;
        let password = random_passphrase().map_err(NmError::Refused)?;
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        let ssid = format!("Hotspot-{}", host.trim());
        let mut settings = Settings::default();
        let ssid = ssid.get(..32).unwrap_or(&ssid);
        hotspot_settings(&mut settings, ssid, &password, HotspotBand::Automatic);
        settings.set("connection", "id", HOTSPOT_PROFILE);
        settings.set("connection", "type", "802-11-wireless");
        settings.set("connection", "autoconnect", false);
        settings.set("ipv4", "method", "shared");
        settings.set("ipv6", "method", "ignore");
        nm.add_and_activate(settings, &device.path, None, HOTSPOT_TIMEOUT)
            .await
            .map(drop)
    })
}

fn random_passphrase() -> Result<Zeroizing<String>, String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzACDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = Zeroizing::new([0_u8; 12]);
    getrandom::fill(&mut *bytes)
        .map_err(|error| format!("no randomness for a passphrase: {error}"))?;
    Ok(Zeroizing::new(
        bytes
            .iter()
            .filter_map(|byte| {
                let at = usize::from(*byte).checked_rem(ALPHABET.len())?;
                ALPHABET.get(at).copied().map(char::from)
            })
            .collect(),
    ))
}

fn hotspot_settings(settings: &mut Settings, ssid: &str, password: &str, band: HotspotBand) {
    settings.set("802-11-wireless", "ssid", ssid.as_bytes().to_vec());
    settings.set("802-11-wireless", "mode", "ap");
    match band {
        HotspotBand::Automatic => settings.remove("802-11-wireless", "band"),
        HotspotBand::TwoPointFourGhz => settings.set("802-11-wireless", "band", "bg"),
        HotspotBand::FiveGhz => settings.set("802-11-wireless", "band", "a"),
    }
    settings.set("802-11-wireless-security", "key-mgmt", "wpa-psk");
    settings.set("802-11-wireless-security", "psk", password);
}

pub fn list_vpn_connections() -> Result<Vec<VpnConnectionInfo>, String> {
    with_nm(|nm| async move {
        let profiles = nm.profiles().await?;
        let active = nm.active_connections().await.unwrap_or_default();
        Ok(profiles
            .into_iter()
            .filter(|profile| tilo_nm::is_vpn_type(&profile.settings.kind()))
            .map(|profile| {
                let uuid = profile.settings.uuid();
                VpnConnectionInfo {
                    name: profile.settings.id(),
                    active: active.iter().any(|connection| connection.uuid == uuid),
                    uuid,
                    kind: profile.settings.kind(),
                }
            })
            .collect())
    })
}

/// Up returns once `NetworkManager` has taken the request; the connection's progress is followed like
/// any other change.
async fn switch_profile(nm: &NmClient, profile: &Profile, enabled: bool) -> Result<(), NmError> {
    if enabled {
        nm.activate(profile, None, None, None).await
    } else {
        let active = active_where(nm, |active| active.profile == profile.path).await?;
        nm.deactivate(&active).await
    }
}

pub fn set_vpn_connection(name: &str, enabled: bool) -> Result<(), String> {
    if name.is_empty() || name.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
        return Err("invalid VPN connection name".into());
    }
    with_nm(|nm| async move {
        let profile = profile_named(&nm, name).await?;
        switch_profile(&nm, &profile, enabled).await
    })
}

/// By immutable UUID, so a security-relevant local credential stays bound to the profile and not to a
/// name.
pub fn set_vpn_connection_uuid(uuid: &str, enabled: bool) -> Result<(), String> {
    if uuid.is_empty() || uuid.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err("invalid VPN connection UUID".into());
    }
    with_nm(|nm| async move {
        let profile = nm.profile(uuid).await?;
        switch_profile(&nm, &profile, enabled).await
    })
}

pub fn import_vpn_config(kind: &str, path: &str) -> Result<(), String> {
    crate::network_settings::import_path_finding_uuid(kind, std::path::Path::new(path)).map(drop)
}

/// By its exact connection name; errors for an invalid name or when `NetworkManager` refuses.
pub fn delete_vpn_connection(name: &str) -> Result<(), String> {
    if name.is_empty() || name.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err("invalid VPN connection name".into());
    }
    with_nm(|nm| async move {
        let profile = profile_named(&nm, name).await?;
        nm.delete(&profile).await
    })
}

pub fn delete_vpn_connection_uuid(uuid: &str) -> Result<(), String> {
    if uuid.is_empty() || uuid.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err("invalid VPN connection UUID".into());
    }
    with_nm(|nm| async move {
        let profile = nm.profile(uuid).await?;
        nm.delete(&profile).await
    })
}

/// Authentication advertised by an access point. `Unknown` is fail-closed: callers must not treat an
/// unfamiliar suite as an ordinary one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum WifiSecurity {
    Open,
    Wpa2,
    Wpa3,
    Owe,
    Enterprise,
    #[default]
    Unknown,
}

impl TryFrom<String> for WifiSecurity {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "open" => Ok(Self::Open),
            "wpa2" => Ok(Self::Wpa2),
            "wpa3" => Ok(Self::Wpa3),
            "owe" => Ok(Self::Owe),
            "enterprise" => Ok(Self::Enterprise),
            "unknown" => Ok(Self::Unknown),
            _ => Err(format!("unsupported Wi-Fi security mode {value:?}")),
        }
    }
}

impl From<WifiSecurity> for String {
    fn from(value: WifiSecurity) -> Self {
        match value {
            WifiSecurity::Open => "open",
            WifiSecurity::Wpa2 => "wpa2",
            WifiSecurity::Wpa3 => "wpa3",
            WifiSecurity::Owe => "owe",
            WifiSecurity::Enterprise => "enterprise",
            WifiSecurity::Unknown => "unknown",
        }
        .to_string()
    }
}

impl WifiSecurity {
    /// What an access point advertises: its privacy flag and the key management suites of its WPA and
    /// RSN elements.
    #[must_use]
    pub fn of_access_point(point: &AccessPoint) -> Self {
        use tilo_nm::model::{
            AP_KEY_8021X, AP_KEY_EAP_SUITE_B_192, AP_KEY_OWE, AP_KEY_OWE_TRANSITION, AP_KEY_PSK,
            AP_KEY_SAE, AP_PRIVACY,
        };
        let suites = point.wpa_flags | point.rsn_flags;
        if suites == 0 {
            return if point.flags & AP_PRIVACY == 0 {
                Self::Open
            } else {
                // WEP: nothing Tilo offers to join.
                Self::Unknown
            };
        }
        if suites & (AP_KEY_8021X | AP_KEY_EAP_SUITE_B_192) != 0 {
            Self::Enterprise
        } else if suites & (AP_KEY_OWE | AP_KEY_OWE_TRANSITION) != 0 {
            Self::Owe
        } else if suites & AP_KEY_SAE != 0 {
            Self::Wpa3
        } else if point.rsn_flags & AP_KEY_PSK != 0 {
            Self::Wpa2
        } else {
            Self::Unknown
        }
    }

    #[must_use]
    pub fn is_secure(self) -> bool {
        !matches!(self, Self::Open)
    }

    /// Only personal WPA profiles can be recreated from a name and a passphrase: OWE has no secret,
    /// enterprise needs identity, EAP and CA policy.
    #[must_use]
    pub fn supports_portable_passphrase(self) -> bool {
        matches!(self, Self::Wpa2 | Self::Wpa3)
    }

    /// Stable domain-separation byte for encrypted continuity envelopes.
    #[must_use]
    pub const fn continuity_tag(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Wpa2 => 1,
            Self::Wpa3 => 2,
            Self::Owe => 3,
            Self::Enterprise => 4,
            Self::Unknown => 255,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetworkInfo {
    pub ssid: String,
    /// Bucket `0..=4`, from [`signal_pct_to_bucket`].
    pub signal: u8,
    pub secure: bool,
    /// Unlike `secure`, this is enough to decide whether password continuity is valid for the network.
    pub security: WifiSecurity,
    pub connected: bool,
    /// A saved profile exists: this machine has joined it before and connects without asking its
    /// password.
    pub known: bool,
}

/// Turns the Wi-Fi radio on or off and says how it really stands afterwards. Two layers hold it:
/// `NetworkManager`'s flag and the kernel's soft block, which a hard block overrides.
pub fn set_wifi_radio(enabled: bool) -> Result<RadioState, String> {
    set_wifi_radio_with(&SystemRadio, enabled)
}

/// Succeeds only when the radio really ended up in the requested state; see [`set_wifi_radio`].
pub fn set_wifi(enabled: bool) -> Result<(), String> {
    let radios = set_wifi_radio(enabled)?;
    wifi_change_outcome(enabled, &radios)
}

pub fn wifi_change_outcome(enabled: bool, radios: &RadioState) -> Result<(), String> {
    if radios.wifi_enabled == enabled {
        Ok(())
    } else if enabled && radios.wifi_hardware_blocked {
        Err("Wi-Fi is switched off with the hardware switch".into())
    } else if enabled {
        Err("Wi-Fi stayed off".into())
    } else {
        Err("Wi-Fi stayed on".into())
    }
}

/// The two layers that hold Wi-Fi, behind one seam so the sequence in [`set_wifi_radio_with`] is tested
/// against a fake `NetworkManager`.
trait WifiRadioHost {
    fn kill_switches(&self) -> crate::rfkill::RfkillState;
    fn lift_soft_block(&self) -> Result<(), String>;
    fn set_wireless_enabled(&self, enabled: bool) -> Result<(), String>;
    fn radio_state(&self) -> Result<RadioState, String>;
}

struct SystemRadio;

impl WifiRadioHost for SystemRadio {
    fn kill_switches(&self) -> crate::rfkill::RfkillState {
        crate::rfkill::state(crate::rfkill::RadioKind::Wlan)
    }
    fn lift_soft_block(&self) -> Result<(), String> {
        crate::rfkill::unblock(crate::rfkill::RadioKind::Wlan)
    }
    fn set_wireless_enabled(&self, enabled: bool) -> Result<(), String> {
        with_nm(|nm| async move { nm.set_wifi_enabled(enabled).await })
    }
    fn radio_state(&self) -> Result<RadioState, String> {
        Ok(with_kill_switches(nm_radio_state()?, self.kill_switches()))
    }
}

fn set_wifi_radio_with(host: &dyn WifiRadioHost, enabled: bool) -> Result<RadioState, String> {
    if enabled {
        let switches = host.kill_switches();
        if switches.soft_blocked {
            // `NetworkManager` lifts a soft block itself when its flag goes from off to on, but not
            // when the flag is already on and another program blocked the radio.
            if let Err(error) = host.lift_soft_block() {
                tracing::warn!(target: "tilo::os_actions", %error, "could not lift the Wi-Fi soft block");
            }
        }
    }
    host.set_wireless_enabled(enabled)?;
    host.radio_state()
}

pub fn get_wifi() -> Result<bool, String> {
    wifi_from_radios(&get_radio_state()?)
}

/// Read from a snapshot already taken, so a caller holding one does not ask `NetworkManager` again.
pub fn wifi_from_radios(radios: &RadioState) -> Result<bool, String> {
    if !radios.wifi_available {
        return Err("no Wi-Fi adapter on this machine".into());
    }
    Ok(radios.wifi_enabled)
}

/// The networks in range as `NetworkManager` last saw them: a read that never waits for the radio.
pub fn list_wifi_networks() -> Result<Vec<WifiNetworkInfo>, String> {
    with_nm(|nm| async move {
        let mut networks = networks_of(&nm.access_points().await?);
        // A failed read leaves them unknown, which only means a password may be asked for.
        if let Ok(profiles) = nm.profiles().await {
            mark_known(&mut networks, &profiles);
        }
        Ok(networks)
    })
}

fn mark_known(networks: &mut [WifiNetworkInfo], profiles: &[Profile]) {
    for network in networks {
        network.known = network.connected
            || profiles
                .iter()
                .any(|profile| is_wifi_profile_for(&profile.settings, &network.ssid));
    }
}

fn is_wifi_profile_for(settings: &Settings, ssid: &str) -> bool {
    settings.kind() == "802-11-wireless"
        && settings.bytes("802-11-wireless", "ssid") == ssid.as_bytes()
}

/// Returns once every Wi-Fi device's scan finished, so the list read after it shows the new result.
pub fn scan_wifi() -> Result<(), String> {
    crate::block_on(async { tilo_nm::scan_wifi().await.map_err(|e| e.to_string()) })
}

/// One entry per network name: access points sharing a name are one network, as strong as its
/// strongest.
fn networks_of(points: &[AccessPoint]) -> Vec<WifiNetworkInfo> {
    let mut networks: Vec<WifiNetworkInfo> = Vec::new();
    for point in points {
        if point.ssid.trim().is_empty() {
            continue;
        }
        let security = WifiSecurity::of_access_point(point);
        let entry = WifiNetworkInfo {
            ssid: point.ssid.clone(),
            signal: signal_pct_to_bucket(point.strength),
            secure: security.is_secure(),
            security,
            connected: point.active,
            known: false,
        };
        if let Some(existing) = networks.iter_mut().find(|e| e.ssid == entry.ssid) {
            if entry.connected || (entry.signal > existing.signal && !existing.connected) {
                existing.security = entry.security;
                existing.secure = entry.secure;
            }
            if entry.signal > existing.signal {
                existing.signal = entry.signal;
            }
            if entry.connected {
                existing.connected = true;
            }
        } else {
            networks.push(entry);
        }
    }
    networks
}

fn signal_pct_to_bucket(pct: u8) -> u8 {
    match pct {
        0..=20 => 1,
        21..=45 => 2,
        46..=70 => 3,
        _ => 4,
    }
}

/// A connection's traffic as its interface counts it: the interface, the profile on it now, and the
/// bytes moved since it attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceTraffic {
    /// The kernel interface (`wlp2s0`).
    pub interface: String,
    pub uuid: String,
    pub name: String,
    pub wired: bool,
    pub traffic: crate::modem::Traffic,
}

/// Every Wi-Fi and wired connection up now, from their interfaces' counters; the hotspot this device
/// shares is left out, as that is other people's traffic.
pub fn read_interface_traffic() -> Result<Vec<InterfaceTraffic>, String> {
    let (devices, active) =
        with_nm(|nm| async move { Ok((nm.devices().await?, nm.active_connections().await?)) })?;
    Ok(interface_traffic(&devices, &active, |interface| {
        let read = |name: &str| {
            std::fs::read_to_string(format!("/sys/class/net/{interface}/statistics/{name}"))
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()
        };
        Some(crate::modem::Traffic {
            received: read("rx_bytes")?,
            sent: read("tx_bytes")?,
        })
    }))
}

fn interface_traffic(
    devices: &[Device],
    active: &[ActiveConnection],
    counters: impl Fn(&str) -> Option<crate::modem::Traffic>,
) -> Vec<InterfaceTraffic> {
    devices
        .iter()
        .filter(|device| matches!(device.kind, DeviceKind::Wifi | DeviceKind::Ethernet))
        .filter_map(|device| {
            let connection = active.iter().find(|connection| {
                Some(&connection.path) == device.active.as_ref()
                    && connection.state == tilo_nm::ActiveState::Activated
                    && connection.id != HOTSPOT_PROFILE
            })?;
            Some(InterfaceTraffic {
                interface: device.interface.clone(),
                uuid: connection.uuid.clone(),
                name: connection.id.clone(),
                wired: device.kind == DeviceKind::Ethernet,
                traffic: counters(&device.interface)?,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Offline,
    Wired,
    Wifi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    pub kind: LinkKind,
    /// The SSID on Wi-Fi, the profile name on Ethernet; `None` when offline.
    pub name: Option<String>,
    /// Bucket `1..=4` on Wi-Fi, `0` on wired and offline, on [`WifiNetworkInfo::signal`]'s scale.
    pub signal: u8,
}

impl LinkStatus {
    pub fn offline() -> Self {
        Self {
            kind: LinkKind::Offline,
            name: None,
            signal: 0,
        }
    }
    pub fn is_online(&self) -> bool {
        !matches!(self.kind, LinkKind::Offline)
    }
}

#[derive(Debug)]
pub struct NetworkFollow {
    task: tilo_rt::JoinHandle<()>,
}

impl Drop for NetworkFollow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Calls `on_change` on the app runtime whenever `NetworkManager` reports a change that network
/// settings shows.
pub fn follow_network_changes(
    on_change: impl Fn() + Send + Sync + 'static,
) -> Result<NetworkFollow, String> {
    follow_network_changes_boxed(Box::new(on_change))
}

fn follow_network_changes_boxed(
    on_change: Box<dyn Fn() + Send + Sync>,
) -> Result<NetworkFollow, String> {
    let runtime = tilo_rt::try_app().map_err(|error| error.to_string())?;
    let task = runtime.spawn(async move {
        let watched = async { tilo_nm::NmClient::system().await?.watch_changes().await }.await;
        let mut changes = match watched {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(%error, "could not follow NetworkManager's changes");
                return;
            }
        };
        while changes.changed().await.is_some() {
            on_change();
        }
    });
    Ok(NetworkFollow { task })
}

/// One D-Bus read, never a scan; long-lived readers follow [`tilo_nm::watch`] instead.
pub fn get_link_status() -> Result<LinkStatus, String> {
    let state = crate::block_on(async { tilo_nm::state().await.map_err(|e| e.to_string()) })?;
    Ok(link_status_from_state(&state, wireless_link_quality_pct()))
}

/// The top bar's summary for one reading: the primary connection, the one the person's traffic leaves
/// on.
pub fn link_status_from_state(
    state: &tilo_nm::NetworkState,
    wireless_quality_pct: Option<u8>,
) -> LinkStatus {
    let Some(primary) = &state.primary else {
        return LinkStatus::offline();
    };
    let name = primary.name.clone();
    match primary.kind {
        tilo_nm::LinkKind::Wifi => LinkStatus {
            kind: LinkKind::Wifi,
            name,
            // Bucket 4 until /proc/net/wireless refines it, so a connected radio never draws as "no
            // signal".
            signal: wireless_quality_pct.map_or(4, signal_pct_to_bucket),
        },
        tilo_nm::LinkKind::Wired | tilo_nm::LinkKind::Mobile | tilo_nm::LinkKind::Other => {
            LinkStatus {
                kind: LinkKind::Wired,
                name,
                signal: 0,
            }
        }
    }
}

/// Best link quality across every wireless interface, as a percentage; `/proc/net/wireless` reports on
/// the driver's own scale.
pub fn wireless_link_quality_pct() -> Option<u8> {
    let raw = std::fs::read_to_string("/proc/net/wireless").ok()?;
    parse_proc_net_wireless(&raw)
}

fn parse_proc_net_wireless(raw: &str) -> Option<u8> {
    let mut best: Option<u8> = None;
    // The first two lines are headers.
    for line in raw.lines().skip(2) {
        let Some((_iface, rest)) = line.split_once(':') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        // status, then link quality.
        let Some(_status) = fields.next() else {
            continue;
        };
        let Some(quality) = fields.next() else {
            continue;
        };
        let Ok(q) = quality.trim_end_matches('.').parse::<f32>() else {
            continue;
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0.0..=100.0 immediately above, so the rounded value always \
                      fits in u8 with no sign loss"
        )]
        let pct = if q <= 70.0 {
            ((q / 70.0) * 100.0).round().clamp(0.0, 100.0) as u8
        } else {
            q.round().clamp(0.0, 100.0) as u8
        };
        best = Some(best.map_or(pct, |b: u8| b.max(pct)));
    }
    best
}

/// A password travels in the D-Bus call that saves it with the profile, never in argv.
pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    join_wifi(ssid, password, false)
}

/// Joins `ssid` through its saved profile when there is one, taking `password` as its new one, else
/// through a new profile.
pub(crate) fn join_wifi(ssid: &str, password: Option<&str>, hidden: bool) -> Result<(), String> {
    if let Some(password) = password
        && password.chars().any(|c| matches!(c, '\n' | '\r'))
    {
        return Err("Wi-Fi password contains an invalid line break".into());
    }
    let password = password.map(|password| Zeroizing::new(password.to_owned()));
    with_nm(|nm| async move {
        let device = wifi_device(&nm).await?;
        let point = if hidden {
            None
        } else {
            let point = nm
                .access_points()
                .await?
                .into_iter()
                .filter(|point| point.device == device.path && point.ssid == ssid)
                .max_by_key(|point| point.strength)
                .ok_or_else(|| {
                    NmError::NotFound(format!("no Wi-Fi network called {ssid} is in range"))
                })?;
            Some(point)
        };
        let security = point.as_ref().map(WifiSecurity::of_access_point);
        let specific = point.as_ref().map(|point| point.path.as_str());
        let saved = nm
            .profiles()
            .await?
            .into_iter()
            .find(|profile| is_wifi_profile_for(&profile.settings, ssid));
        if let Some(profile) = saved {
            if let Some(password) = &password {
                let mut settings = profile.settings.clone();
                wifi_secret(&mut settings, security, password).map_err(NmError::Refused)?;
                nm.update(&profile, &settings).await?;
            }
            return nm
                .activate(
                    &profile,
                    Some(&device.path),
                    specific,
                    Some(WIFI_CONNECT_TIMEOUT),
                )
                .await;
        }
        let mut settings = Settings::default();
        settings.set("connection", "id", ssid);
        settings.set("connection", "type", "802-11-wireless");
        settings.set("802-11-wireless", "ssid", ssid.as_bytes().to_vec());
        if hidden {
            settings.set("802-11-wireless", "hidden", true);
        }
        if let Some(password) = &password {
            wifi_secret(&mut settings, security, password).map_err(NmError::Refused)?;
        }
        nm.add_and_activate(settings, &device.path, specific, WIFI_CONNECT_TIMEOUT)
            .await
            .map(drop)
    })
}

/// Puts `password` into the profile as the key its network's `security` takes; `None` is a hidden
/// network, taken to be WPA2/WPA3.
fn wifi_secret(
    settings: &mut Settings,
    security: Option<WifiSecurity>,
    password: &str,
) -> Result<(), String> {
    const SECURITY: &str = "802-11-wireless-security";
    let key_mgmt = match security {
        None | Some(WifiSecurity::Wpa2) => "wpa-psk",
        Some(WifiSecurity::Wpa3) => "sae",
        Some(WifiSecurity::Open | WifiSecurity::Owe) => {
            return Err("this network takes no password".into());
        }
        Some(WifiSecurity::Enterprise) => {
            return Err("this network needs a work or school sign-in".into());
        }
        Some(WifiSecurity::Unknown) => {
            return Err("this network's security is not supported".into());
        }
    };
    // A saved profile that already names its key management keeps it: a transition network joined as
    // WPA2 stays WPA2.
    if settings.string(SECURITY, "key-mgmt").is_none() {
        settings.set(SECURITY, "key-mgmt", key_mgmt);
    }
    settings.set(SECURITY, "psk", password);
    Ok(())
}

/// Disconnects the `NetworkManager` connection; it does not forget the profile or affect other radios.
pub fn disconnect_wifi(ssid: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err("Wi-Fi name is invalid".into());
    }
    with_nm(|nm| async move {
        let active = active_where(&nm, |active| active.id == ssid).await?;
        nm.deactivate(&active).await
    })
}

/// Values mirror the daemon's own, portal state included, rather than being remapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectivityState {
    #[default]
    Unknown,
    Offline,
    Portal,
    Limited,
    Full,
}

impl ConnectivityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Connectivity not checked",
            Self::Offline => "Offline",
            Self::Portal => "Sign-in required",
            Self::Limited => "Limited connectivity",
            Self::Full => "Connected to the internet",
        }
    }
}

impl From<tilo_nm::NmConnectivity> for ConnectivityState {
    fn from(value: tilo_nm::NmConnectivity) -> Self {
        match value {
            tilo_nm::NmConnectivity::Unknown => Self::Unknown,
            tilo_nm::NmConnectivity::None => Self::Offline,
            tilo_nm::NmConnectivity::Portal => Self::Portal,
            tilo_nm::NmConnectivity::Limited => Self::Limited,
            tilo_nm::NmConnectivity::Full => Self::Full,
        }
    }
}

/// A read: it never makes the daemon probe the internet; [`check_connectivity_now`] does that
/// deliberately.
pub fn get_connectivity() -> Result<ConnectivityState, String> {
    crate::block_on(async {
        tilo_nm::state()
            .await
            .map(|state| state.connectivity.into())
            .map_err(|e| e.to_string())
    })
}

/// An HTTP probe of the daemon's configured URI, for an explicit "check again".
pub fn check_connectivity_now() -> Result<ConnectivityState, String> {
    let check = crate::block_on(async {
        tilo_nm::check_connectivity_now()
            .await
            .map(ConnectivityState::from)
            .map_err(|e| e.to_string())
    });
    connectivity_from(check, get_connectivity)
}

fn connectivity_from(
    check: Result<ConnectivityState, String>,
    cached: impl FnOnce() -> Result<ConnectivityState, String>,
) -> Result<ConnectivityState, String> {
    match check {
        Ok(state) => Ok(state),
        Err(check_error) => cached().map_err(|_| check_error),
    }
}

/// `NetworkManager`'s configured connectivity-check URI. Opening it is the standard captive-portal
/// flow.
pub fn captive_portal_uri() -> Result<String, String> {
    crate::block_on(async {
        tilo_nm::captive_portal_uri()
            .await
            .map_err(|e| e.to_string())
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedNetworkProfile {
    pub name: String,
    pub uuid: String,
    pub kind: String,
    pub autoconnect: bool,
    /// When this device last connected with it, in seconds since the Unix epoch; `None` when it never
    /// has.
    pub last_used: Option<u64>,
}

/// The only profile fields eligible for personal-device roaming. The UUID is deliberately absent: it is
/// a machine-local lookup capability.
pub struct PortableWifiProfileSecret {
    pub ssid: String,
    pub security: WifiSecurity,
    pub password: Zeroizing<String>,
}

/// One profile as portable Wi-Fi: only the network name, its key management and its passphrase are
/// read.
pub fn portable_wifi_profile_secret(uuid: &str) -> Result<PortableWifiProfileSecret, String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move {
        let profile = nm.profile(uuid).await?;
        let settings = &profile.settings;
        let ssid = String::from_utf8(settings.bytes("802-11-wireless", "ssid")).unwrap_or_default();
        let key_mgmt = settings
            .string("802-11-wireless-security", "key-mgmt")
            .unwrap_or_default();
        // Read the secret only for a profile that may travel.
        portable_profile(&settings.kind(), &ssid, &key_mgmt, "x").map_err(NmError::Refused)?;
        let secrets = nm.secrets(&profile, "802-11-wireless-security").await?;
        let password = Zeroizing::new(
            secrets
                .string("802-11-wireless-security", "psk")
                .unwrap_or_default(),
        );
        portable_profile(&settings.kind(), &ssid, &key_mgmt, &password).map_err(NmError::Refused)
    })
}

fn portable_profile(
    kind: &str,
    ssid: &str,
    key_mgmt: &str,
    password: &str,
) -> Result<PortableWifiProfileSecret, String> {
    if kind != "802-11-wireless" {
        return Err("only Wi-Fi profiles can be shared across devices".into());
    }
    if ssid.is_empty() || ssid.len() > runtime_wifi_ssid_limit() {
        return Err("the saved Wi-Fi profile has an invalid network name".into());
    }
    let security = match key_mgmt.to_ascii_lowercase().as_str() {
        "wpa-psk" | "wpa-none" => WifiSecurity::Wpa2,
        "sae" => WifiSecurity::Wpa3,
        "wpa-eap" | "ieee8021x" => {
            return Err("enterprise Wi-Fi identity and certificates stay device-local".into());
        }
        "none" | "owe" | "" => {
            return Err("this Wi-Fi profile has no portable passphrase".into());
        }
        _ => return Err("unrecognized Wi-Fi security cannot be shared safely".into()),
    };
    if password.is_empty() || password.len() > 512 {
        return Err("the saved Wi-Fi passphrase is missing or too large".into());
    }
    Ok(PortableWifiProfileSecret {
        ssid: ssid.to_string(),
        security,
        password: Zeroizing::new(password.to_string()),
    })
}

const fn runtime_wifi_ssid_limit() -> usize {
    256
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MeteredPolicy {
    #[default]
    Automatic,
    Metered,
    Unmetered,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacPolicy {
    Permanent,
    Random,
    #[default]
    Stable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyConfiguration {
    pub automatic_url: Option<String>,
    /// `NetworkManager` keeps proxies as auto-configuration only, so it is stored as the one-line
    /// script the daemon's PAC handler reads.
    pub manual: Option<String>,
    pub browser_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionPolicy {
    pub metered: MeteredPolicy,
    pub mac: Option<MacPolicy>,
    pub proxy: ProxyConfiguration,
}

/// `None` means `NetworkManager` returned a value this UI does not understand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionProfileSettings {
    pub metered: Option<MeteredPolicy>,
    pub mac: Option<MacPolicy>,
    pub proxy: Option<ProxyConfiguration>,
    pub ipv4: Option<IpConfiguration>,
    /// Which saved network wins when several are in range: higher first.
    pub priority: Option<i32>,
    /// A Wi-Fi network that does not broadcast its name.
    pub hidden: Option<bool>,
    pub ipv6: Option<crate::network_settings::Ipv6Configuration>,
    /// Who a work or school (802.1X) network knows this device as.
    pub enterprise: Option<crate::network_settings::EnterpriseIdentity>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionPolicyPatch {
    pub metered: Option<MeteredPolicy>,
    pub mac: Option<MacPolicy>,
    pub proxy: Option<ProxyConfiguration>,
    pub priority: Option<i32>,
    pub hidden: Option<bool>,
}

pub fn configure_connection_policy_patch(
    uuid: &str,
    patch: &ConnectionPolicyPatch,
) -> Result<(), String> {
    validate_uuid(uuid)?;
    if let Some(url) = patch
        .proxy
        .as_ref()
        .and_then(|p| p.automatic_url.as_deref())
        && (url.len() > 2048 || !(url.starts_with("https://") || url.starts_with("http://")))
    {
        return Err("automatic proxy URL must use HTTP or HTTPS and be at most 2048 bytes".into());
    }
    if let Some(server) = patch.proxy.as_ref().and_then(|p| p.manual.as_deref())
        && !crate::network_settings::valid_proxy_server(server)
    {
        return Err(
            "a proxy server is a host name or address and a port: proxy.example:8080".into(),
        );
    }
    let patch = patch.clone();
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            apply_policy(settings, &patch);
            Ok(())
        })
        .await
    })
}

fn apply_policy(settings: &mut Settings, patch: &ConnectionPolicyPatch) {
    if let Some(priority) = patch.priority {
        settings.set(
            "connection",
            "autoconnect-priority",
            priority.clamp(-999, 999),
        );
    }
    if let Some(hidden) = patch.hidden {
        settings.set("802-11-wireless", "hidden", hidden);
    }
    if let Some(value) = patch.metered {
        let metered: i32 = match value {
            MeteredPolicy::Automatic => 0,
            MeteredPolicy::Metered => 1,
            MeteredPolicy::Unmetered => 2,
        };
        settings.set("connection", "metered", metered);
    }
    if let Some(proxy) = &patch.proxy {
        let script = proxy
            .manual
            .as_deref()
            .map(crate::network_settings::manual_proxy_script)
            .unwrap_or_default();
        let automatic = proxy.automatic_url.is_some() || proxy.manual.is_some();
        settings.set("proxy", "method", i32::from(automatic));
        let url = if proxy.manual.is_some() {
            None
        } else {
            proxy.automatic_url.as_deref()
        };
        match url {
            Some(url) => settings.set("proxy", "pac-url", url),
            None => settings.remove("proxy", "pac-url"),
        }
        if script.is_empty() {
            settings.remove("proxy", "pac-script");
        } else {
            settings.set("proxy", "pac-script", script);
        }
        settings.set("proxy", "browser-only", proxy.browser_only);
    }
    if let Some(mac) = patch.mac {
        // The text form; the older byte form would contradict it.
        settings.remove("802-11-wireless", "cloned-mac-address");
        settings.set(
            "802-11-wireless",
            "assigned-mac-address",
            match mac {
                MacPolicy::Permanent => "permanent",
                MacPolicy::Random => "random",
                MacPolicy::Stable => "stable-ssid",
            },
        );
    }
}

pub fn configure_connection_policy(uuid: &str, policy: &ConnectionPolicy) -> Result<(), String> {
    configure_connection_policy_patch(
        uuid,
        &ConnectionPolicyPatch {
            metered: Some(policy.metered),
            mac: policy.mac,
            proxy: Some(policy.proxy.clone()),
            ..ConnectionPolicyPatch::default()
        },
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionDiagnostics {
    pub network_manager_state: String,
    pub connectivity: ConnectivityState,
    pub device: Option<String>,
    pub address: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub recommendations: Vec<String>,
}

/// The daemon's state and connectivity, and the addresses, gateway and name servers of the device the
/// primary connection leaves on.
pub fn connection_diagnostics() -> Result<ConnectionDiagnostics, String> {
    with_nm(|nm| async move {
        let state = nm.state().await?;
        let mut out = ConnectionDiagnostics {
            network_manager_state: match state.state {
                tilo_nm::NmState::Asleep => "Asleep",
                tilo_nm::NmState::Disconnected => "Disconnected",
                tilo_nm::NmState::Disconnecting | tilo_nm::NmState::Connecting => "Connecting",
                tilo_nm::NmState::ConnectedLocal => "Connected (local)",
                tilo_nm::NmState::ConnectedSite => "Connected (site)",
                tilo_nm::NmState::ConnectedGlobal => "Connected",
                tilo_nm::NmState::Unknown => "Unknown",
            }
            .to_string(),
            connectivity: state.connectivity.into(),
            ..ConnectionDiagnostics::default()
        };
        let interface = state.primary.and_then(|primary| primary.device);
        let device = match &interface {
            Some(interface) => nm
                .devices()
                .await?
                .into_iter()
                .find(|device| device.interface == *interface),
            None => None,
        };
        if let Some(device) = device {
            let ip = nm
                .ip_details(&device.path, IpFamily::V4)
                .await
                .unwrap_or_default();
            out.device = Some(device.interface);
            out.address = ip.addresses.into_iter().next();
            out.gateway = ip.gateway;
            out.dns = ip.dns;
        }
        Ok(out)
    })
    .map(|mut out| {
        add_recommendations(&mut out);
        out
    })
}

fn add_recommendations(out: &mut ConnectionDiagnostics) {
    match out.connectivity {
        ConnectivityState::Portal => out
            .recommendations
            .push("Open the network sign-in page, then run diagnostics again.".into()),
        ConnectivityState::Limited if out.gateway.is_none() => out
            .recommendations
            .push("No default gateway was received; check DHCP or the manual IP settings.".into()),
        ConnectivityState::Limited if out.dns.is_empty() => out
            .recommendations
            .push("No DNS server was received; check DNS settings on this connection.".into()),
        ConnectivityState::Offline => out.recommendations.push(
            "No connection is active; check the cable, radio, or selected mobile SIM.".into(),
        ),
        _ => {}
    }
}

pub fn list_saved_network_profiles() -> Result<Vec<SavedNetworkProfile>, String> {
    with_nm(|nm| async move {
        Ok(nm
            .profiles()
            .await?
            .iter()
            .filter_map(|profile| saved_profile(&profile.settings))
            .collect())
    })
}

/// `None` for a profile of another kind (VPN, bridge, loopback).
fn saved_profile(settings: &Settings) -> Option<SavedNetworkProfile> {
    let kind = settings.kind();
    matches!(
        kind.as_str(),
        "802-11-wireless" | "802-3-ethernet" | "gsm" | "cdma"
    )
    .then(|| SavedNetworkProfile {
        name: settings.id(),
        uuid: settings.uuid(),
        kind,
        autoconnect: settings.autoconnect(),
        last_used: settings.timestamp(),
    })
}

pub(crate) fn validate_uuid(uuid: &str) -> Result<(), String> {
    (uuid.len() == 36 && uuid::Uuid::try_parse(uuid).is_ok())
        .then_some(())
        .ok_or_else(|| "invalid NetworkManager profile UUID".into())
}

pub fn forget_network_profile(uuid: &str) -> Result<(), String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move {
        let profile = nm.profile(uuid).await?;
        nm.delete(&profile).await
    })
}

pub fn set_network_autoconnect(uuid: &str, enabled: bool) -> Result<(), String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            settings.set("connection", "autoconnect", enabled);
            Ok(())
        })
        .await
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpConfiguration {
    pub automatic: bool,
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

pub(crate) fn valid_ip_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b':' | b'/' | b'%'))
}

/// The UUID is the stable identity; user-facing names are never selectors.
pub fn configure_ipv4(uuid: &str, config: &IpConfiguration) -> Result<(), String> {
    validate_uuid(uuid)?;
    if config.addresses.len() > 16
        || config.dns.len() > 16
        || config.addresses.iter().any(|v| !valid_ip_token(v))
        || config
            .gateway
            .as_deref()
            .is_some_and(|v| !valid_ip_token(v))
        || config.dns.iter().any(|v| !valid_ip_token(v))
    {
        return Err("invalid IP address, prefix, gateway, or DNS server".into());
    }
    if !config.automatic && config.addresses.is_empty() {
        return Err("manual IPv4 needs at least one address with a prefix".into());
    }
    let config = config.clone();
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            let family = IpFamily::V4;
            settings.set(
                family.setting(),
                "method",
                if config.automatic { "auto" } else { "manual" },
            );
            settings.set_addresses(family, &config.addresses)?;
            settings.set_gateway(family, config.gateway.as_deref())?;
            settings.set_dns(family, &config.dns)?;
            settings.set(family.setting(), "ignore-auto-dns", !config.dns.is_empty());
            Ok(())
        })
        .await
    })
}

pub fn connection_profile_settings(uuid: &str) -> Result<ConnectionProfileSettings, String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move { Ok(profile_settings_of(&nm.profile(uuid).await?.settings)) })
}

fn proxy_of(settings: &Settings) -> Option<ProxyConfiguration> {
    let browser_only = settings.boolean("proxy", "browser-only").unwrap_or(false);
    match settings.int("proxy", "method").unwrap_or(0) {
        0 => Some(ProxyConfiguration {
            automatic_url: None,
            manual: None,
            browser_only,
        }),
        1 => {
            let url = settings.string("proxy", "pac-url").unwrap_or_default();
            let script = settings.string("proxy", "pac-script").unwrap_or_default();
            if url.is_empty()
                && let Some(server) = crate::network_settings::manual_proxy_of(&script)
            {
                Some(ProxyConfiguration {
                    automatic_url: None,
                    manual: Some(server),
                    browser_only,
                })
            } else {
                ((url.starts_with("http://") || url.starts_with("https://")) && url.len() <= 2048)
                    .then_some(ProxyConfiguration {
                        automatic_url: Some(url),
                        manual: None,
                        browser_only,
                    })
            }
        }
        _ => None,
    }
}

/// `None` for each value this device does not understand.
fn profile_settings_of(settings: &Settings) -> ConnectionProfileSettings {
    let metered = match settings.metered() {
        0 => Some(MeteredPolicy::Automatic),
        1 => Some(MeteredPolicy::Metered),
        2 => Some(MeteredPolicy::Unmetered),
        _ => None,
    };
    let mac = match settings
        .string("802-11-wireless", "assigned-mac-address")
        .as_deref()
    {
        Some("stable" | "stable-ssid") => Some(MacPolicy::Stable),
        Some("random") => Some(MacPolicy::Random),
        Some("permanent" | "preserve") => Some(MacPolicy::Permanent),
        _ => None,
    };
    let ipv4 = match settings.method(IpFamily::V4).as_deref() {
        Some(method @ ("auto" | "manual")) => {
            let addresses = settings.addresses(IpFamily::V4);
            let dns = settings.dns(IpFamily::V4);
            (addresses.len() <= 16
                && dns.len() <= 16
                && addresses.iter().chain(&dns).all(|v| valid_ip_token(v)))
            .then(|| IpConfiguration {
                automatic: method == "auto",
                addresses,
                gateway: settings.gateway(IpFamily::V4),
                dns,
            })
        }
        _ => None,
    };
    ConnectionProfileSettings {
        metered,
        mac,
        proxy: proxy_of(settings),
        ipv4,
        priority: Some(
            settings
                .int("connection", "autoconnect-priority")
                .unwrap_or(0),
        ),
        hidden: settings.has("802-11-wireless").then(|| {
            settings
                .boolean("802-11-wireless", "hidden")
                .unwrap_or(false)
        }),
        ipv6: crate::network_settings::ipv6_of(settings),
        enterprise: crate::network_settings::enterprise_identity_of(settings),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HotspotBand {
    #[default]
    Automatic,
    TwoPointFourGhz,
    FiveGhz,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotConfiguration {
    pub ssid: String,
    pub password: String,
    pub band: HotspotBand,
}

impl HotspotConfiguration {
    /// Errors for an empty `ssid`, one over 32 bytes or with a control character, or a `password`
    /// outside 8–63 characters.
    pub fn validate(&self) -> Result<(), String> {
        if self.ssid.is_empty() || self.ssid.len() > 32 || self.ssid.chars().any(char::is_control) {
            return Err("hotspot name must be 1–32 bytes".into());
        }
        if !(8..=63).contains(&self.password.len()) || self.password.chars().any(char::is_control) {
            return Err("hotspot password must be 8–63 printable characters".into());
        }
        Ok(())
    }

    /// Escaping follows the WIFI schema Android and iOS scanners read.
    pub fn qr_payload(&self) -> Result<String, String> {
        self.validate()?;
        let escape = |value: &str| {
            value
                .replace('\\', "\\\\")
                .replace(';', "\\;")
                .replace(',', "\\,")
                .replace(':', "\\:")
        };
        Ok(format!(
            "WIFI:T:WPA;S:{};P:{};;",
            escape(&self.ssid),
            escape(&self.password)
        ))
    }
}

/// As far as it can be read back: its name and band, never its passphrase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotDetails {
    pub ssid: String,
    pub band: HotspotBand,
}

/// `None` until a hotspot has been configured.
pub fn hotspot_details() -> Result<Option<HotspotDetails>, String> {
    with_nm(|nm| async move {
        let Ok(profile) = profile_named(&nm, HOTSPOT_PROFILE).await else {
            return Ok(None);
        };
        let settings = &profile.settings;
        Ok(Some(HotspotDetails {
            ssid: String::from_utf8(settings.bytes("802-11-wireless", "ssid")).unwrap_or_default(),
            band: match settings.string("802-11-wireless", "band").as_deref() {
                Some("a") => HotspotBand::FiveGhz,
                Some("bg") => HotspotBand::TwoPointFourGhz,
                _ => HotspotBand::Automatic,
            },
        }))
    })
}

pub fn configure_hotspot(config: &HotspotConfiguration) -> Result<(), String> {
    config.validate()?;
    let config = config.clone();
    with_nm(|nm| async move {
        let profile = profile_or_add(&nm, HOTSPOT_PROFILE, || {
            let mut settings = Settings::default();
            settings.set("connection", "id", HOTSPOT_PROFILE);
            settings.set("connection", "type", "802-11-wireless");
            settings.set("connection", "autoconnect", false);
            settings.set("ipv4", "method", "shared");
            settings.set("ipv6", "method", "ignore");
            hotspot_settings(&mut settings, &config.ssid, &config.password, config.band);
            settings
        })
        .await?;
        let mut settings = profile.settings.clone();
        hotspot_settings(&mut settings, &config.ssid, &config.password, config.band);
        nm.update(&profile, &settings).await?;
        nm.activate(&profile, None, None, Some(HOTSPOT_TIMEOUT))
            .await
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotClient {
    pub mac: String,
    pub signal_dbm: Option<i32>,
}

pub fn list_hotspot_clients() -> Result<Vec<HotspotClient>, String> {
    let iface = with_nm(|nm| async move {
        profile_named(&nm, HOTSPOT_PROFILE).await?;
        let Ok(active) = active_where(&nm, |active| active.id == HOTSPOT_PROFILE).await else {
            return Ok(String::new());
        };
        let devices = nm.devices().await?;
        Ok(devices
            .into_iter()
            .find(|device| active.devices.contains(&device.path))
            .map(|device| device.interface)
            .unwrap_or_default())
    })?;
    if iface.is_empty()
        || iface
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && b != b'_' && b != b'-')
    {
        return Ok(Vec::new());
    }
    let out = crate::process::run_capture("iw", &["dev", &iface, "station", "dump"])?;
    Ok(parse_hotspot_clients(&out))
}

fn parse_hotspot_clients(out: &str) -> Vec<HotspotClient> {
    let mut clients = Vec::new();
    for block in out.split("Station ").skip(1) {
        let mac = block
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if mac.len() != 17 {
            continue;
        }
        let signal_dbm = block.lines().find_map(|line| {
            line.trim()
                .strip_prefix("signal:")?
                .split_ascii_whitespace()
                .next()?
                .parse()
                .ok()
        });
        clients.push(HotspotClient { mac, signal_dbm });
    }
    clients
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnterpriseWifiConfiguration {
    pub profile_name: String,
    pub ssid: String,
    pub identity: String,
    pub anonymous_identity: Option<String>,
    pub ca_certificate: String,
    pub method: EnterpriseEapMethod,
    pub phase2_auth: Option<String>,
    pub client_certificate: Option<String>,
    pub private_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateInfo {
    pub subject: String,
    pub expires: String,
    pub sha256_fingerprint: String,
}

pub fn inspect_certificate(path: &str) -> Result<CertificateInfo, String> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() || !p.is_file() {
        return Err("certificate must be an existing absolute file".into());
    }
    let metadata =
        std::fs::metadata(p).map_err(|error| format!("cannot read certificate: {error}"))?;
    if metadata.len() > 1024 * 1024 {
        return Err("certificate must be no larger than 1 MiB".into());
    }
    let bytes = std::fs::read(p).map_err(|error| format!("cannot read certificate: {error}"))?;
    let info = tilo_crypto::inspect_certificate(&bytes)?;
    Ok(CertificateInfo {
        subject: info.subject,
        expires: info.expires,
        sha256_fingerprint: info.sha256_fingerprint,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnterpriseEapMethod {
    #[default]
    Peap,
    Ttls,
    Tls,
}

impl EnterpriseWifiConfiguration {
    fn validate(&self) -> Result<(), String> {
        let safe = |value: &str, max: usize| {
            !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        };
        if !safe(&self.profile_name, 64) || !safe(&self.ssid, 32) || !safe(&self.identity, 253) {
            return Err("enterprise Wi-Fi name, SSID, or identity is invalid".into());
        }
        let ca = std::path::Path::new(&self.ca_certificate);
        if !ca.is_absolute() || !ca.is_file() {
            return Err("choose an existing absolute CA certificate file".into());
        }
        if self.method == EnterpriseEapMethod::Tls {
            for (label, path) in [
                ("client certificate", self.client_certificate.as_deref()),
                ("private key", self.private_key.as_deref()),
            ] {
                if !path.is_some_and(|p| {
                    let p = std::path::Path::new(p);
                    p.is_absolute() && p.is_file()
                }) {
                    return Err(format!("choose an existing absolute {label} file"));
                }
            }
        } else if !matches!(
            self.phase2_auth.as_deref().unwrap_or("mschapv2"),
            "mschapv2" | "pap" | "chap"
        ) {
            return Err("unsupported enterprise inner-authentication method".into());
        }
        if self
            .anonymous_identity
            .as_deref()
            .is_some_and(|value| !safe(value, 253))
        {
            return Err("anonymous identity is invalid".into());
        }
        Ok(())
    }
}

/// Configures a PEAP/MSCHAPv2 802.1X profile; a CA certificate is mandatory, so the server is verified.
pub fn connect_enterprise_wifi(
    config: &EnterpriseWifiConfiguration,
    password: &str,
) -> Result<(), String> {
    config.validate()?;
    let _ca = inspect_certificate(&config.ca_certificate)?;
    if let Some(client) = config.client_certificate.as_deref() {
        let _ = inspect_certificate(client)?;
    }
    if config.method != EnterpriseEapMethod::Tls
        && (password.is_empty() || password.chars().any(|c| matches!(c, '\n' | '\r')))
    {
        return Err("enterprise Wi-Fi password is empty or invalid".into());
    }
    let config = config.clone();
    let password = Zeroizing::new(password.to_owned());
    with_nm(|nm| async move {
        let profile = profile_or_add(&nm, &config.profile_name, || {
            let mut settings = Settings::default();
            settings.set("connection", "id", config.profile_name.as_str());
            settings.set("connection", "type", "802-11-wireless");
            settings.set("802-11-wireless", "ssid", config.ssid.as_bytes().to_vec());
            settings
        })
        .await?;
        let mut settings = profile.settings.clone();
        enterprise_settings(&mut settings, &config);
        nm.update(&profile, &settings).await?;
        let key = if config.method == EnterpriseEapMethod::Tls {
            "private-key-password"
        } else {
            "password"
        };
        let secrets = std::collections::HashMap::from([(
            "802-1x".to_owned(),
            std::collections::HashMap::from([(key.to_owned(), password)]),
        )]);
        nm.activate_with_secrets(&profile, secrets, ENTERPRISE_CONNECT_TIMEOUT)
            .await
    })
}

/// A work or school network's 802.1X settings, its secrets not saved.
fn enterprise_settings(settings: &mut Settings, config: &EnterpriseWifiConfiguration) {
    const EAP: &str = "802-1x";
    settings.set("802-11-wireless-security", "key-mgmt", "wpa-eap");
    let method = match config.method {
        EnterpriseEapMethod::Peap => "peap",
        EnterpriseEapMethod::Ttls => "ttls",
        EnterpriseEapMethod::Tls => "tls",
    };
    settings.set(EAP, "eap", vec![method.to_owned()]);
    settings.set(EAP, "identity", config.identity.as_str());
    match config
        .anonymous_identity
        .as_deref()
        .filter(|v| !v.is_empty())
    {
        Some(anonymous) => settings.set(EAP, "anonymous-identity", anonymous),
        None => settings.remove(EAP, "anonymous-identity"),
    }
    if config.method == EnterpriseEapMethod::Tls {
        settings.remove(EAP, "phase2-auth");
    } else {
        settings.set(
            EAP,
            "phase2-auth",
            config.phase2_auth.as_deref().unwrap_or("mschapv2"),
        );
    }
    settings.set_path(EAP, "ca-cert", &config.ca_certificate);
    settings.set_path(
        EAP,
        "client-cert",
        config.client_certificate.as_deref().unwrap_or(""),
    );
    settings.set_path(
        EAP,
        "private-key",
        config.private_key.as_deref().unwrap_or(""),
    );
    // NM_SETTING_SECRET_FLAG_NOT_SAVED: asked for at every connection.
    settings.set(EAP, "password-flags", 2_u32);
    settings.set(EAP, "private-key-password-flags", 2_u32);
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WiredInfo {
    pub connected: bool,
    pub device: Option<String>,
    pub speed: Option<String>,
    pub ipv4: Option<String>,
}

/// A default (`connected: false`) answer when no ethernet device is present, so Settings need not
/// branch.
pub fn get_wired() -> Result<WiredInfo, String> {
    with_nm(|nm| async move {
        let Some(device) = wired_device(&nm).await? else {
            return Ok(WiredInfo::default());
        };
        let speed = nm.wired_speed(&device.path).await.ok().flatten();
        let ipv4 = nm
            .ip_details(&device.path, IpFamily::V4)
            .await
            .ok()
            .and_then(|ip| ip.addresses.into_iter().next())
            .and_then(|address| address.split('/').next().map(str::to_owned));
        Ok(WiredInfo {
            connected: device.is_activated(),
            device: Some(device.interface),
            speed: speed.map(|speed| format!("{speed} Mb/s")),
            ipv4,
        })
    })
}

async fn wired_device(nm: &NmClient) -> Result<Option<Device>, NmError> {
    Ok(nm
        .devices()
        .await?
        .into_iter()
        .find(|device| device.kind == DeviceKind::Ethernet))
}

/// Scoped to one device: it cannot disable Wi-Fi, a VPN or `NetworkManager` itself.
pub fn set_wired(enabled: bool) -> Result<(), String> {
    with_nm(|nm| async move {
        let device = wired_device(&nm)
            .await?
            .ok_or_else(|| NmError::NotFound("no wired network interface found".into()))?;
        if device.is_activated() == enabled {
            return Ok(());
        }
        if enabled {
            nm.connect(&device).await
        } else {
            nm.disconnect(&device).await
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(ssid: &str, strength: u8, rsn: u32, active: bool) -> AccessPoint {
        AccessPoint {
            path: format!("/ap/{ssid}/{strength}"),
            device: "/dev/wifi".into(),
            ssid: ssid.into(),
            strength,
            flags: u32::from(rsn != 0),
            wpa_flags: 0,
            rsn_flags: rsn,
            frequency: 2412,
            max_bitrate: 54_000,
            active,
        }
    }

    #[test]
    fn access_points_become_one_network_per_name_with_its_security() {
        let nets = networks_of(&[
            point("Home_Network", 80, 0x100, true),
            point("Cafe_Guest", 55, 0, false),
            point("Neighbour_5G", 30, 0x100, false),
            point("", 90, 0x100, false),
        ]);
        let seen: Vec<_> = nets
            .iter()
            .map(|n| (n.ssid.as_str(), n.signal, n.secure, n.connected))
            .collect();
        assert_eq!(
            seen,
            [
                ("Home_Network", 4, true, true),
                ("Cafe_Guest", 3, false, false),
                ("Neighbour_5G", 2, true, false),
            ],
            "a hidden network has no name to list"
        );
        assert_eq!(nets[0].security, WifiSecurity::Wpa2);
        assert_eq!(nets[1].security, WifiSecurity::Open);

        // A transition network: the access point in use names it.
        let nets = networks_of(&[
            point("Transition", 90, 0x100, false),
            point("Transition", 40, 0x400, true),
        ]);
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].security, WifiSecurity::Wpa3);
        assert!(nets[0].connected);
        assert_eq!(nets[0].signal, 4);

        for (flags, wpa, rsn, expected) in [
            (0, 0, 0, WifiSecurity::Open),
            (1, 0, 0x100, WifiSecurity::Wpa2),
            (1, 0, 0x500, WifiSecurity::Wpa3),
            (1, 0, 0x800, WifiSecurity::Owe),
            (1, 0, 0x300, WifiSecurity::Enterprise),
            (1, 0x100, 0, WifiSecurity::Unknown),
            (1, 0, 0, WifiSecurity::Unknown),
        ] {
            let ap = AccessPoint {
                flags,
                wpa_flags: wpa,
                rsn_flags: rsn,
                ..point("x", 50, 0, false)
            };
            assert_eq!(
                WifiSecurity::of_access_point(&ap),
                expected,
                "{flags} {wpa:#x} {rsn:#x}"
            );
        }
        assert!(WifiSecurity::Wpa2.supports_portable_passphrase());
        assert!(WifiSecurity::Wpa3.supports_portable_passphrase());
        assert!(!WifiSecurity::Enterprise.supports_portable_passphrase());
        assert!(!WifiSecurity::Owe.supports_portable_passphrase());
    }

    #[test]
    fn signal_buckets_and_wifi_availability() {
        assert_eq!(signal_pct_to_bucket(0), 1);
        assert_eq!(signal_pct_to_bucket(20), 1);
        assert_eq!(signal_pct_to_bucket(35), 2);
        assert_eq!(signal_pct_to_bucket(60), 3);
        assert_eq!(signal_pct_to_bucket(95), 4);
        let radios = RadioState::default();
        assert_eq!(
            wifi_from_radios(&radios).unwrap_err(),
            "no Wi-Fi adapter on this machine"
        );
        let on = RadioState {
            wifi_available: true,
            wifi_enabled: true,
            ..RadioState::default()
        };
        assert!(wifi_from_radios(&on).unwrap());
    }

    #[test]
    fn a_password_is_put_in_as_the_key_its_network_takes() {
        let mut settings = Settings::default();
        wifi_secret(&mut settings, Some(WifiSecurity::Wpa3), "hunter22").unwrap();
        assert_eq!(
            settings
                .string("802-11-wireless-security", "key-mgmt")
                .as_deref(),
            Some("sae")
        );
        assert_eq!(
            settings
                .string("802-11-wireless-security", "psk")
                .as_deref(),
            Some("hunter22")
        );
        // A saved profile keeps the key management it was joined with.
        wifi_secret(&mut settings, Some(WifiSecurity::Wpa2), "new-pass").unwrap();
        assert_eq!(
            settings
                .string("802-11-wireless-security", "key-mgmt")
                .as_deref(),
            Some("sae")
        );
        let mut hidden = Settings::default();
        wifi_secret(&mut hidden, None, "hunter22").unwrap();
        assert_eq!(
            hidden
                .string("802-11-wireless-security", "key-mgmt")
                .as_deref(),
            Some("wpa-psk")
        );
        for refused in [
            WifiSecurity::Open,
            WifiSecurity::Enterprise,
            WifiSecurity::Unknown,
        ] {
            assert!(wifi_secret(&mut Settings::default(), Some(refused), "x").is_err());
        }
    }

    #[test]
    fn portable_profiles_keep_only_personal_wifi_secrets() {
        let profile = portable_profile(
            "802-11-wireless",
            "Home network",
            "wpa-psk",
            "correct horse battery staple",
        )
        .unwrap();
        assert_eq!(profile.ssid, "Home network");
        assert_eq!(profile.security, WifiSecurity::Wpa2);
        assert_eq!(profile.password.as_str(), "correct horse battery staple");
        assert!(portable_profile("802-11-wireless", "Work", "wpa-eap", "secret").is_err());
        assert!(portable_profile("802-3-ethernet", "LAN", "wpa-psk", "secret").is_err());
        assert!(portable_profile("802-11-wireless", "Cafe", "none", "secret").is_err());
        assert!(portable_profile("802-11-wireless", "Home", "sae", "").is_err());
    }

    #[test]
    fn a_policy_written_into_a_profile_reads_back_the_same() {
        let mut settings = Settings::default();
        settings.set("connection", "type", "802-11-wireless");
        settings.set(
            "802-11-wireless",
            "cloned-mac-address",
            vec![2_u8, 0, 0, 0, 0, 1],
        );
        apply_policy(
            &mut settings,
            &ConnectionPolicyPatch {
                metered: Some(MeteredPolicy::Metered),
                mac: Some(MacPolicy::Stable),
                proxy: Some(ProxyConfiguration {
                    automatic_url: None,
                    manual: Some("proxy.example:8080".into()),
                    browser_only: true,
                }),
                priority: Some(5_000),
                hidden: Some(true),
            },
        );
        assert_eq!(settings.get("802-11-wireless", "cloned-mac-address"), None);
        let read = profile_settings_of(&settings);
        assert_eq!(read.metered, Some(MeteredPolicy::Metered));
        assert_eq!(read.mac, Some(MacPolicy::Stable));
        assert_eq!(
            read.proxy,
            Some(ProxyConfiguration {
                automatic_url: None,
                manual: Some("proxy.example:8080".into()),
                browser_only: true,
            })
        );
        assert_eq!(read.priority, Some(999), "clamped");
        assert_eq!(read.hidden, Some(true));

        apply_policy(
            &mut settings,
            &ConnectionPolicyPatch {
                proxy: Some(ProxyConfiguration {
                    automatic_url: Some("https://proxy.example/pac".into()),
                    manual: None,
                    browser_only: false,
                }),
                ..ConnectionPolicyPatch::default()
            },
        );
        assert_eq!(
            profile_settings_of(&settings)
                .proxy
                .and_then(|proxy| proxy.automatic_url)
                .as_deref(),
            Some("https://proxy.example/pac")
        );
    }

    #[test]
    fn values_this_ui_does_not_understand_read_as_none() {
        let mut settings = Settings::default();
        settings.set("connection", "metered", 3_i32);
        settings.set(
            "802-11-wireless",
            "assigned-mac-address",
            "02:00:00:00:00:01",
        );
        settings.set("proxy", "method", 2_i32);
        settings.set("ipv4", "method", "shared");
        let read = profile_settings_of(&settings);
        assert_eq!(
            (read.metered, read.mac, read.proxy, read.ipv4),
            (None, None, None, None)
        );

        let wired = profile_settings_of(&Settings::default());
        assert_eq!(wired.metered, Some(MeteredPolicy::Automatic), "the default");
        assert_eq!(wired.hidden, None, "a wired profile has no hiding");
        assert_eq!(
            wired.proxy,
            Some(ProxyConfiguration::default()),
            "no proxy by default"
        );
    }

    #[test]
    fn saved_networks_are_wifi_ethernet_and_mobile_profiles() {
        let profile = |kind: &str| {
            let mut settings = Settings::default();
            settings.set("connection", "id", "Office: Lab");
            settings.set("connection", "uuid", "12345678-1234-1234-1234-123456789abc");
            settings.set("connection", "type", kind);
            settings
        };
        let saved = saved_profile(&profile("802-11-wireless")).unwrap();
        assert_eq!(saved.name, "Office: Lab");
        assert!(saved.autoconnect, "the default");
        assert_eq!(saved.last_used, None);
        assert!(saved_profile(&profile("wireguard")).is_none());
        assert!(saved_profile(&profile("gsm")).is_some());
    }

    #[test]
    fn connectivity_hotspots_and_their_clients() {
        {
            use tilo_nm::NmConnectivity;
            for (raw, want) in [
                (NmConnectivity::Unknown, ConnectivityState::Unknown),
                (NmConnectivity::None, ConnectivityState::Offline),
                (NmConnectivity::Portal, ConnectivityState::Portal),
                (NmConnectivity::Limited, ConnectivityState::Limited),
                (NmConnectivity::Full, ConnectivityState::Full),
            ] {
                assert_eq!(ConnectivityState::from(raw), want, "{raw:?}");
            }
        }
        {
            let refused = || {
                Err::<ConnectivityState, String>("Not authorized to recheck connectivity.".into())
            };
            assert_eq!(
                connectivity_from(refused(), || Ok(ConnectivityState::Full)),
                Ok(ConnectivityState::Full)
            );
            assert_eq!(
                connectivity_from(Ok(ConnectivityState::Portal), || unreachable!()),
                Ok(ConnectivityState::Portal)
            );
            assert_eq!(
                connectivity_from(refused(), || Err("NetworkManager not running".into())),
                Err("Not authorized to recheck connectivity.".into())
            );
        }
        {
            let config = HotspotConfiguration {
                ssid: "Tilo; Guest".into(),
                password: "hello:world".into(),
                band: HotspotBand::Automatic,
            };
            assert_eq!(
                config.qr_payload().unwrap(),
                "WIFI:T:WPA;S:Tilo\\; Guest;P:hello\\:world;;"
            );
            let mut settings = Settings::default();
            hotspot_settings(
                &mut settings,
                &config.ssid,
                &config.password,
                HotspotBand::FiveGhz,
            );
            assert_eq!(settings.bytes("802-11-wireless", "ssid"), b"Tilo; Guest");
            assert_eq!(
                settings.string("802-11-wireless", "band").as_deref(),
                Some("a")
            );
            hotspot_settings(
                &mut settings,
                &config.ssid,
                &config.password,
                HotspotBand::Automatic,
            );
            assert_eq!(settings.string("802-11-wireless", "band"), None);
            let passphrase = random_passphrase().unwrap();
            assert_eq!(passphrase.len(), 12);
            assert!(passphrase.chars().all(|c| c.is_ascii_alphanumeric()));
        }
        {
            let clients =
                parse_hotspot_clients("Station aa:bb:cc:dd:ee:ff (on wlan0)\n\tsignal: -51 dBm\n");
            assert_eq!(clients[0].mac, "AA:BB:CC:DD:EE:FF");
            assert_eq!(clients[0].signal_dbm, Some(-51));
        }
    }

    #[test]
    fn connection_settings_are_validated_before_asking_networkmanager() {
        assert!(!valid_ip_token("1.2.3.4;reboot"));
        let config = IpConfiguration {
            automatic: false,
            ..Default::default()
        };
        // A syntactically valid id fails on the missing address before any bus is touched.
        let err = configure_ipv4("12345678-1234-1234-1234-123456789abc", &config).unwrap_err();
        assert!(err.contains("at least one address"));
        let policy = ConnectionPolicy {
            proxy: ProxyConfiguration {
                automatic_url: Some("file:///tmp/proxy.pac".into()),
                manual: None,
                browser_only: false,
            },
            ..Default::default()
        };
        let err = configure_connection_policy("12345678-1234-1234-1234-123456789abc", &policy)
            .unwrap_err();
        assert!(err.contains("HTTP or HTTPS"));
    }

    #[test]
    fn enterprise_settings_keep_their_secrets_unsaved() {
        let mut settings = Settings::default();
        enterprise_settings(
            &mut settings,
            &EnterpriseWifiConfiguration {
                profile_name: "Campus".into(),
                ssid: "eduroam".into(),
                identity: "ada@example.org".into(),
                anonymous_identity: None,
                ca_certificate: "/etc/pki/campus.pem".into(),
                method: EnterpriseEapMethod::Peap,
                phase2_auth: None,
                client_certificate: None,
                private_key: None,
            },
        );
        assert_eq!(settings.strings("802-1x", "eap"), ["peap"]);
        assert_eq!(
            settings.string("802-1x", "phase2-auth").as_deref(),
            Some("mschapv2")
        );
        assert_eq!(
            settings.path("802-1x", "ca-cert").as_deref(),
            Some("/etc/pki/campus.pem")
        );
        assert_eq!(settings.uint("802-1x", "password-flags"), Some(2));
        assert_eq!(settings.string("802-1x", "password"), None);
        assert_eq!(settings.get("802-1x", "client-cert"), None);
    }
}

#[cfg(test)]
mod link_status_tests {
    use super::*;

    fn primary(kind: tilo_nm::LinkKind, name: &str) -> tilo_nm::NetworkState {
        tilo_nm::NetworkState {
            state: tilo_nm::NmState::ConnectedGlobal,
            connectivity: tilo_nm::NmConnectivity::Full,
            metered: Some(false),
            primary: Some(tilo_nm::PrimaryLink {
                kind,
                name: Some(name.into()),
                device: None,
            }),
        }
    }

    #[test]
    fn link_status_follows_the_primary_connection() {
        {
            let s = link_status_from_state(
                &primary(tilo_nm::LinkKind::Wired, "Wired connection 1"),
                Some(90),
            );
            assert_eq!(s.kind, LinkKind::Wired);
            assert_eq!(s.name.as_deref(), Some("Wired connection 1"));
            assert_eq!(s.signal, 0, "a cable has no signal strength to report");
        }
        {
            let s =
                link_status_from_state(&primary(tilo_nm::LinkKind::Wifi, "Cafe WiFi"), Some(70));
            assert_eq!(s.kind, LinkKind::Wifi);
            assert_eq!(s.name.as_deref(), Some("Cafe WiFi"));
            assert_eq!(s.signal, 3, "70% is bucket 3 on the shared signal scale");
        }
        {
            let s = link_status_from_state(&primary(tilo_nm::LinkKind::Wifi, "Cafe WiFi"), None);
            assert!(s.signal > 0, "a connected radio never reads as no-signal");
        }
        {
            let s = link_status_from_state(&primary(tilo_nm::LinkKind::Mobile, "Carrier"), None);
            assert!(s.is_online());
            assert_eq!(s.name.as_deref(), Some("Carrier"));
        }
        {
            let connecting = tilo_nm::NetworkState {
                state: tilo_nm::NmState::Connecting,
                ..tilo_nm::NetworkState::unknown()
            };
            assert_eq!(
                link_status_from_state(&connecting, Some(80)),
                LinkStatus::offline()
            );
            assert!(!link_status_from_state(&tilo_nm::NetworkState::unknown(), None).is_online());
        }
    }

    /// Real `/proc/net/wireless`, wext scale (quality out of 70).
    const PROC_WIRELESS: &str = "Inter-| sta-|   Quality        |   Discarded packets\n\
 face | tus | link level noise |  nwid  crypt   frag  retry   misc\n\
wlp2s0: 0000   49.  -61.  -256        0      0      0     55      0\n";

    #[test]
    fn proc_net_wireless_quality_is_scaled_to_a_percentage() {
        // A driver reporting past the wext ceiling is already a percentage.
        let driver_percent =
            "hdr\nhdr\nwlan0: 0000   88.  -40.  -256        0      0      0      0\n";
        for (case, raw, want) in [
            // 49/70 = 70%.
            (
                "link_quality_is_scaled_off_the_wext_ceiling",
                PROC_WIRELESS,
                Some(70),
            ),
            (
                "a_driver_reporting_percent_directly_is_not_rescaled",
                driver_percent,
                Some(88),
            ),
            (
                "no_wireless_interfaces_yields_no_reading",
                "Inter-| sta-|   Quality\n face | tus | link\n",
                None,
            ),
        ] {
            assert_eq!(parse_proc_net_wireless(raw), want, "case {case}");
        }
        // 70% is bucket 3 on the shared signal scale.
        assert_eq!(signal_pct_to_bucket(70), 3);
    }
}

#[cfg(test)]
mod radio_presence_tests {
    use super::*;

    fn device(kind: DeviceKind) -> Device {
        Device {
            path: String::new(),
            interface: String::new(),
            kind,
            state: 100,
            hw_address: String::new(),
            active: None,
        }
    }

    fn switches(wifi: bool, wwan: bool) -> Radios {
        Radios {
            wifi_enabled: wifi,
            wifi_hardware_enabled: true,
            wwan_enabled: wwan,
            wwan_hardware_enabled: true,
        }
    }

    /// `NetworkManager` reports its WWAN switch as enabled on a machine with no modem at all, so only a
    /// modem device it manages counts.
    #[test]
    fn an_enabled_wwan_switch_without_a_modem_device_is_no_modem() {
        let wifi_only = [device(DeviceKind::Wifi), device(DeviceKind::Other)];
        let on = radio_state_of(switches(true, true), &wifi_only, &[]);
        assert!(on.wifi_available);
        assert!(!on.wwan_available, "no modem device is listed");
        assert!(!on.airplane_mode);
        let off = radio_state_of(switches(false, true), &wifi_only, &[]);
        assert!(
            off.airplane_mode,
            "the WWAN switch of a missing modem keeps no radio on"
        );
        let modem = radio_state_of(
            switches(true, false),
            &[device(DeviceKind::Wifi), device(DeviceKind::Modem)],
            &[],
        );
        assert!(modem.wwan_available && !modem.wwan_enabled);
        let hotspot = ActiveConnection {
            path: String::new(),
            id: HOTSPOT_PROFILE.into(),
            uuid: String::new(),
            kind: "802-11-wireless".into(),
            state: tilo_nm::ActiveState::Activated,
            devices: Vec::new(),
            profile: String::new(),
        };
        assert!(radio_state_of(switches(true, false), &wifi_only, &[hotspot]).hotspot_active);
    }

    /// A Wi-Fi PHY or WLAN interface is a Wi-Fi radio and a WWAN device or interface is a modem; a
    /// wired interface and an empty class are neither.
    #[test]
    fn radios_are_read_from_the_kernels_device_classes() {
        use std::fs;
        let class = tempfile::tempdir().unwrap();
        let class = class.path();
        assert!(!has_wifi_radio_in(class) && !has_wwan_modem_in(class));

        let wired = class.join("net/enp1s0");
        fs::create_dir_all(&wired).unwrap();
        fs::write(wired.join("uevent"), "INTERFACE=enp1s0\nIFINDEX=2\n").unwrap();
        fs::create_dir_all(class.join("ieee80211")).unwrap();
        fs::create_dir_all(class.join("wwan")).unwrap();
        assert!(
            !has_wifi_radio_in(class),
            "a wired interface is no Wi-Fi radio"
        );
        assert!(!has_wwan_modem_in(class), "a wired interface is no modem");

        let wlan = class.join("net/wlp2s0");
        fs::create_dir_all(&wlan).unwrap();
        fs::write(wlan.join("uevent"), "DEVTYPE=wlan\nINTERFACE=wlp2s0\n").unwrap();
        assert!(has_wifi_radio_in(class));
        assert!(!has_wwan_modem_in(class));

        let modem = class.join("net/wwan0");
        fs::create_dir_all(&modem).unwrap();
        fs::write(modem.join("uevent"), "DEVTYPE=wwan\nINTERFACE=wwan0\n").unwrap();
        assert!(has_wwan_modem_in(class));

        let bare = tempfile::tempdir().unwrap();
        fs::create_dir_all(bare.path().join("ieee80211/phy0")).unwrap();
        fs::create_dir_all(bare.path().join("wwan/wwan0")).unwrap();
        assert!(
            has_wifi_radio_in(bare.path()),
            "a PHY with no interface yet"
        );
        assert!(
            has_wwan_modem_in(bare.path()),
            "a WWAN device with no interface yet"
        );
    }
}

#[cfg(test)]
mod wifi_radio_tests {
    use super::*;
    use crate::rfkill::RfkillState;
    use std::cell::{Cell, RefCell};

    /// `NetworkManager` and the kernel as far as Wi-Fi on and off goes: like the daemon, turning its
    /// flag off soft-blocks the radio.
    struct FakeNm {
        flag: Cell<bool>,
        soft: Cell<bool>,
        hard: Cell<bool>,
        /// Whether this session may write `/dev/rfkill`.
        may_unblock: bool,
        calls: RefCell<Vec<String>>,
    }

    impl FakeNm {
        fn on() -> Self {
            Self {
                flag: Cell::new(true),
                soft: Cell::new(false),
                hard: Cell::new(false),
                may_unblock: true,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl WifiRadioHost for FakeNm {
        fn kill_switches(&self) -> RfkillState {
            RfkillState {
                present: true,
                soft_blocked: self.soft.get(),
                hard_blocked: self.hard.get(),
            }
        }
        fn lift_soft_block(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("rfkill unblock wlan".into());
            if !self.may_unblock {
                return Err("/dev/rfkill: Permission denied".into());
            }
            self.soft.set(false);
            Ok(())
        }
        fn set_wireless_enabled(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!(
                "NetworkManager wifi {}",
                if enabled { "on" } else { "off" }
            ));
            let was = self.flag.replace(enabled);
            if !enabled {
                self.soft.set(true);
            } else if !was {
                self.soft.set(false);
            }
            Ok(())
        }
        fn radio_state(&self) -> Result<RadioState, String> {
            let nm = RadioState {
                wifi_available: true,
                wifi_enabled: self.flag.get(),
                ..RadioState::default()
            };
            Ok(with_kill_switches(nm, self.kill_switches()))
        }
    }

    #[test]
    fn off_on_off_on_each_lands_in_the_state_asked_for() {
        let nm = FakeNm::on();
        for enabled in [false, true, false, true] {
            let radios = set_wifi_radio_with(&nm, enabled).unwrap();
            assert_eq!(radios.wifi_enabled, enabled, "after asking for {enabled}");
            assert_eq!(wifi_change_outcome(enabled, &radios), Ok(()));
        }
    }

    #[test]
    fn turning_on_lifts_a_block_another_program_left_under_an_enabled_flag() {
        // `rfkill block wlan`, a platform key or a restored boot state: NetworkManager's flag is still
        // on, so setting it again changes nothing, and the radio stayed off.
        let nm = FakeNm::on();
        nm.soft.set(true);
        assert!(!nm.radio_state().unwrap().wifi_enabled, "read as off");
        let radios = set_wifi_radio_with(&nm, true).unwrap();
        assert!(radios.wifi_enabled);
        assert_eq!(
            nm.calls.borrow().as_slice(),
            ["rfkill unblock wlan", "NetworkManager wifi on"]
        );
    }

    #[test]
    fn a_radio_that_stays_off_is_reported_off_with_the_reason() {
        let nm = FakeNm {
            may_unblock: false,
            ..FakeNm::on()
        };
        nm.soft.set(true);
        let radios = set_wifi_radio_with(&nm, true).unwrap();
        assert!(!radios.wifi_enabled, "the read-back, not the request");
        assert_eq!(
            wifi_change_outcome(true, &radios),
            Err("Wi-Fi stayed off".into())
        );

        let nm = FakeNm::on();
        set_wifi_radio_with(&nm, false).unwrap();
        nm.hard.set(true);
        let radios = set_wifi_radio_with(&nm, true).unwrap();
        assert!(!radios.wifi_enabled && radios.wifi_hardware_blocked);
        assert_eq!(
            wifi_change_outcome(true, &radios),
            Err("Wi-Fi is switched off with the hardware switch".into())
        );
        nm.hard.set(false);
        let radios = set_wifi_radio_with(&nm, true).unwrap();
        assert!(radios.wifi_enabled && !radios.wifi_hardware_blocked);
    }

    #[test]
    fn a_network_with_a_saved_wifi_profile_is_known() {
        let network = |ssid: &str, connected: bool| WifiNetworkInfo {
            ssid: ssid.into(),
            signal: 3,
            secure: true,
            security: WifiSecurity::Wpa2,
            connected,
            known: false,
        };
        let mut networks = [
            network("Home", true),
            network("Cafe", false),
            network("Office", false),
        ];
        let profile = |name: &str, kind: &str| {
            let mut settings = Settings::default();
            settings.set("connection", "id", name);
            settings.set("connection", "type", kind);
            settings.set("802-11-wireless", "ssid", name.as_bytes().to_vec());
            Profile {
                path: String::new(),
                settings,
                filename: String::new(),
            }
        };
        mark_known(
            &mut networks,
            &[
                profile("Office", "802-11-wireless"),
                profile("Cafe", "802-3-ethernet"),
            ],
        );
        let known: Vec<_> = networks
            .iter()
            .map(|n| (n.ssid.as_str(), n.known))
            .collect();
        assert_eq!(known, [("Home", true), ("Cafe", false), ("Office", true)]);
    }

    #[test]
    fn without_a_kill_switch_networkmanagers_flag_is_the_state() {
        let nm = RadioState {
            wifi_available: true,
            wifi_enabled: true,
            ..RadioState::default()
        };
        assert_eq!(with_kill_switches(nm.clone(), RfkillState::default()), nm);
    }
}

#[cfg(test)]
mod interface_traffic_tests {
    use super::*;

    fn device(kind: DeviceKind, interface: &str, active: Option<&str>) -> Device {
        Device {
            path: String::new(),
            interface: interface.into(),
            kind,
            state: 100,
            hw_address: String::new(),
            active: active.map(str::to_owned),
        }
    }

    fn connection(path: &str, id: &str, state: tilo_nm::ActiveState) -> ActiveConnection {
        ActiveConnection {
            path: path.into(),
            id: id.into(),
            uuid: format!("uuid-{id}"),
            kind: "802-11-wireless".into(),
            state,
            devices: Vec::new(),
            profile: String::new(),
        }
    }

    /// Each Wi-Fi and wired interface up counts toward the profile on it; one connecting, the hotspot,
    /// a modem and an interface without counters count nothing.
    #[test]
    fn interfaces_count_toward_the_connection_up_on_them() {
        use tilo_nm::ActiveState::{Activated, Activating};
        let devices = [
            device(DeviceKind::Wifi, "wlan0", Some("/a/1")),
            device(DeviceKind::Ethernet, "eth0", Some("/a/2")),
            device(DeviceKind::Ethernet, "eth1", Some("/a/3")),
            device(DeviceKind::Wifi, "wlan1", Some("/a/4")),
            device(DeviceKind::Modem, "cdc-wdm0", Some("/a/5")),
            device(DeviceKind::Ethernet, "gone0", Some("/a/6")),
        ];
        let active = [
            connection("/a/1", "Home", Activated),
            connection("/a/2", "Office", Activated),
            connection("/a/3", "Dock", Activating),
            connection("/a/4", HOTSPOT_PROFILE, Activated),
            connection("/a/5", "Carrier", Activated),
            connection("/a/6", "Gone", Activated),
        ];
        let read = interface_traffic(&devices, &active, |interface| {
            (interface != "gone0").then_some(crate::modem::Traffic {
                received: 10,
                sent: 1,
            })
        });
        let named: Vec<(&str, &str, bool)> = read
            .iter()
            .map(|traffic| {
                (
                    traffic.interface.as_str(),
                    traffic.name.as_str(),
                    traffic.wired,
                )
            })
            .collect();
        assert_eq!(named, [("wlan0", "Home", false), ("eth0", "Office", true)]);
        assert_eq!(read[0].uuid, "uuid-Home");
    }
}
