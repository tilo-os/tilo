//! Network: Wi-Fi, wired, VPN, hotspot and the saved connection profiles
//! of this device, as `NetworkManager` reports them, and mobile data
//! ([`mobile`]), whose card sits next to Wi-Fi.
//!
//! The pane opens on the Wi-Fi list ([`order`] says which networks it shows
//! and in what order) with the controls under it, and drills into pages
//! ([`crate::pages`]): every network in range, one network's page, the known
//! networks, the VPNs and a VPN's page ([`vpn`]), the hotspot and the
//! advanced settings. Every page is the same model; the
//! page on top of the window's trail says which one it composes.
//!
//! Every read and change may block on `nmcli` or the system bus for
//! seconds, so none runs on the UI thread: the model reads a snapshot in the
//! background when it is first needed and again after each change, and every
//! change is a [`NetworkEffect`] (or a [`vpn::VpnEffect`]) that runs on a
//! worker and reports what happened. Passwords, keys and PINs live in
//! process-local buffers that are zeroed once used; they never reach a
//! document.
//!
//! A radio the device does not have is not offered: Wi-Fi and the hotspot
//! without a Wi-Fi radio, airplane mode without a Wi-Fi radio or a modem
//! ([`NetworkArea::statuses`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::network_settings::{
    EnterpriseIdentity, Ipv6Configuration, Ipv6Method, VpnPlugin, VpnProfilePatch,
    VpnProfileSettings, WifiLinkDetails,
};
use os_actions::{
    ConnectivityState, Dispatcher, EnterpriseWifiConfiguration, HotspotBand, HotspotConfiguration,
    IpConfiguration, SavedNetworkProfile, WifiNetworkInfo, WiredInfo,
};
use settings_store::device_vpn::{
    ClientState, ClientStatus, DeviceVpnStatus, ExitUser, OfferedExit,
};
use tracing::warn;
use ui::LocalState;
use ui::collaboration::{NetworkWakeStatus, WifiContinuityCommand, WifiContinuityReply};
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};
use ui::tilo_resource::{self, ActionRef, Effect};
use zeroize::{Zeroize, Zeroizing};

use crate::categories::Category;
use crate::dispatcher::dispatcher_or_real;
use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

mod asking;
pub(crate) mod mobile;
mod order;
mod page;
pub(crate) mod values;
mod view;
pub(crate) mod vpn;

#[cfg(test)]
mod tests;

crate::effects::effect_family!(NetworkEffect => answer);

crate::area::pane_area! {
    NetworkArea;
    fn poll(&self) {
        NetworkArea::poll(self);
    }
    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        NetworkArea::statuses(self)
    }
    fn query(&self, category: Category) -> Option<&dyn crate::area::PaneQuery> {
        (category == Category::Network
            && self.cx.pages().top(Category::Network).is_some_and(|page| page.key == PAGE_ALL))
        .then_some(self as &dyn crate::area::PaneQuery)
    }
}

/// The window's [`NetworkArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&NetworkArea> {
    app.areas().find()
}

/// The pages the pane drills into, by their key in the window's trail.
pub(crate) const PAGE_ALL: &str = "all";
pub(crate) const PAGE_KNOWN: &str = "known";
pub(crate) const PAGE_HOTSPOT: &str = "hotspot";
pub(crate) const PAGE_ADVANCED: &str = "advanced";
/// The modem's page: mobile data and the SIM.
pub(crate) const PAGE_MOBILE: &str = "mobile";
/// A network's page: `wifi:<ssid>`.
const PAGE_WIFI: &str = "wifi:";

fn wifi_page_key(ssid: &str) -> String {
    format!("{PAGE_WIFI}{ssid}")
}

// Ask the authenticated host: this app intentionally has an isolated network
// namespace and cannot inspect the host's nl80211 interface by spawning iw.
fn host_network_wake(profile: &str, enabled: Option<bool>) -> Result<NetworkWakeStatus, String> {
    match ui::collaboration::wifi_continuity_command(WifiContinuityCommand::NetworkWake {
        profile_uuid: profile.into(),
        enabled,
    })? {
        WifiContinuityReply::NetworkWake { status } => Ok(status),
        WifiContinuityReply::Rejected { reason } => Err(reason),
        _ => Err("the host returned an invalid network wake reply".into()),
    }
}

fn wake_description(status: &NetworkWakeStatus) -> &'static str {
    if status.problem.is_some() {
        if status.configured {
            "net-wake-pending"
        } else {
            "net-wake-unavailable"
        }
    } else if status.configured && status.applied == Some(true) {
        "net-wake-unverified"
    } else if status.interface.is_none() {
        "net-wake-connect-first"
    } else if status.supported == Some(false) {
        "net-wake-unsupported"
    } else if status.supported.is_none() {
        "net-wake-unavailable"
    } else {
        "net-wake-disabled"
    }
}

/// The bands a hotspot can use.
const HOTSPOT_BANDS: &[&str] = &["Automatic", "2.4 GHz", "5 GHz"];
/// Whether a connection's data counts against a limit.
const METERED_POLICIES: &[&str] = &["Automatic", "Metered", "Unmetered"];
/// Which hardware address a Wi-Fi connection shows.
const MAC_POLICIES: &[&str] = &[
    "Device address",
    "Random for this network",
    "Random each time",
];
/// How a connection gets its IPv6 address.
const IPV6_METHODS: &[&str] = &["Automatic", "Manual", "Link-local only", "Off"];
/// Whether a connection goes through a proxy, and how it finds it.
const PROXY_MODES: &[&str] = &["None", "Automatic", "Manual"];
/// What a new hotspot is called until the person names it.
const DEFAULT_HOTSPOT_NAME: &str = "Tilo Hotspot";

/// What Network changes on this device.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum NetworkEffect {
    /// Read the adopted network state, pending work and labels this pane shows.
    #[effect(view, returns, kind = "read", label = "Inspect network state")]
    InspectNetworkState,
    /// Scan for Wi-Fi networks and read this device's connections again.
    #[effect(job, idempotent, label = "Refresh networks")]
    RefreshNetworks,
    /// Turn Wi-Fi on or off.
    #[effect(job, idempotent, label = "Wi-Fi")]
    SetWifi { on: bool },
    /// Turn the wired connection on or off.
    #[effect(job, idempotent, label = "Wired connection")]
    SetWired { on: bool },
    /// Turn every radio off, or back on.
    #[effect(job, idempotent, label = "Airplane mode")]
    SetAirplaneMode { on: bool },
    /// Share this device's connection as a Wi-Fi hotspot, or stop.
    #[effect(job, idempotent, label = "Hotspot")]
    SetHotspot { on: bool },
    /// Connect to a Wi-Fi network. Without a password a secure network uses
    /// one the person's devices share, else asks for it with the password
    /// prompt.
    #[effect(job, row, label = "Connect")]
    ConnectNetwork {
        #[param("The network's name (SSID)")]
        ssid: String,
        #[param(secret, "The network's password")]
        password: Option<String>,
    },
    /// Close a network's password prompt.
    #[effect(view, row, label = "Cancel")]
    CancelConnect {
        #[param("The network's name (SSID)")]
        ssid: String,
    },
    /// Open a network's page: how it stands and its settings.
    #[effect(view, row, label = "Network settings")]
    OpenNetworkPage {
        #[param("The network's name (SSID)")]
        ssid: String,
    },
    /// Show every network in range.
    #[effect(view, label = "Show all networks")]
    ShowAllNetworks,
    /// Show every network this device knows, in range or not.
    #[effect(view, label = "Known networks")]
    ShowKnownNetworks,
    /// Show mobile data and the SIM: the modem's whole page.
    #[effect(view, label = "Mobile")]
    ShowMobile,
    /// Ask for the name and password of a network that does not broadcast
    /// its name, to join it.
    #[effect(view, label = "Other network")]
    ShowHiddenNetwork,
    /// Join a network that does not broadcast its name; without its name,
    /// ask for it with the password prompt.
    #[effect(job, label = "Join other network")]
    JoinHiddenNetwork {
        #[param("The network's name (SSID)")]
        ssid: Option<String>,
        #[param(secret, "The network's password; none for an open network")]
        password: Option<String>,
    },
    /// Join a saved network by itself whenever it is in range, or not.
    #[effect(job, row, idempotent, label = "Connect automatically")]
    SetAutoconnect {
        #[param("The saved network's id")]
        profile: String,
        on: bool,
    },
    /// Which of several known networks in range is joined first: the
    /// higher.
    #[effect(job, row, idempotent, label = "Priority")]
    SetPriority {
        #[param("The saved network's id")]
        profile: String,
        #[param("From -999 to 999; higher is joined first")]
        priority: i32,
    },
    /// Count a saved network's data against a limit, never, or as the
    /// network says.
    #[effect(job, row, idempotent, label = "Metered connection")]
    SetMeteredPolicy {
        #[param("The saved network's id")]
        profile: String,
        #[param("Whether data counts against a limit", choices = METERED_POLICIES.iter().copied())]
        policy: String,
    },
    /// Which hardware address this device shows a saved Wi-Fi network.
    #[effect(job, row, idempotent, label = "Hardware address")]
    SetMacAddress {
        #[param("The saved network's id")]
        profile: String,
        #[param("The address it shows", choices = MAC_POLICIES.iter().copied())]
        policy: String,
    },
    /// Join a saved network even when it does not broadcast its name.
    #[effect(job, row, idempotent, label = "Hidden network")]
    SetHiddenNetwork {
        #[param("The saved network's id")]
        profile: String,
        on: bool,
    },
    /// Set a profile's IPv4 settings; what is omitted is what its form
    /// holds.
    #[effect(job, label = "Apply IP settings")]
    SetIpv4 {
        #[param("The saved profile's id")]
        profile: String,
        #[param("Take the address from the network")]
        automatic: Option<bool>,
        #[param("Addresses with their prefix, comma-separated: 192.168.1.20/24")]
        addresses: Option<String>,
        gateway: Option<String>,
        #[param("Name servers, comma-separated")]
        dns: Option<String>,
    },
    /// Set a profile's IPv6 settings; what is omitted is what its form
    /// holds.
    #[effect(job, row, label = "Apply IPv6 settings")]
    SetIpv6 {
        #[param("The saved profile's id")]
        profile: String,
        #[param("How it gets its address", choices = IPV6_METHODS.iter().copied())]
        method: Option<String>,
        #[param("Addresses with their prefix, comma-separated: 2001:db8::10/64")]
        addresses: Option<String>,
        gateway: Option<String>,
    },
    /// Give a profile name servers of its own; none takes the network's.
    #[effect(job, row, idempotent, label = "DNS servers")]
    SetDns {
        #[param("The saved profile's id")]
        profile: String,
        #[param("Name servers, comma-separated; empty for the network's")]
        servers: String,
    },
    /// Send a profile's connections through a proxy, or not; what is
    /// omitted is what its form holds.
    #[effect(job, row, label = "Apply proxy")]
    SetProxy {
        #[param("The saved profile's id")]
        profile: String,
        #[param("None, Automatic or Manual", choices = PROXY_MODES.iter().copied())]
        mode: Option<String>,
        #[param("The configuration URL (Automatic) or host:port (Manual)")]
        address: Option<String>,
    },
    /// Change who a work or school network knows this device as; what is
    /// omitted is what its form holds.
    #[effect(job, row, label = "Apply identity")]
    SetEnterpriseIdentity {
        #[param("The saved network's id")]
        profile: String,
        identity: Option<String>,
        #[param("The CA certificate's path")]
        ca_certificate: Option<String>,
    },
    /// Show a saved network's password on its page, asking to authenticate
    /// first; or hide it.
    #[effect(job, row, label = "Show password")]
    ShowNetworkPassword {
        #[param("The saved network's id")]
        profile: String,
    },
    /// Change a saved network's password; without one, ask for it with the
    /// password prompt.
    #[effect(job, row, label = "Change password")]
    SetNetworkPassword {
        #[param("The saved network's id")]
        profile: String,
        #[param(secret, "The new password")]
        password: Option<String>,
    },
    /// Show or hide the QR code a phone scans to join a network.
    #[effect(job, row, label = "Share network")]
    ShowNetworkCode {
        #[param("The network's name (SSID)")]
        ssid: String,
    },
    /// Persist magic-packet wake for this saved connection. Enabled does not certify physical resume.
    #[effect(job, row, idempotent, label = "Wake over network")]
    SetNetworkWake {
        #[param("The saved network's id")]
        profile: String,
        on: bool,
    },
    /// Forget a saved network and its password.
    #[effect(job, row, destructive, label = "Forget network")]
    ForgetNetwork {
        #[param("The saved network's id")]
        profile: String,
    },
    /// Seal a saved Wi-Fi network's password for the person's other
    /// devices, so they join it too.
    #[effect(job, row, label = "Share with my devices")]
    ShareNetwork {
        #[param("The saved network's id")]
        profile: String,
    },
    /// Check the connection, its address, gateway and name servers.
    #[effect(job, idempotent, label = "Run network diagnostics")]
    RunDiagnostics,
    /// Open the sign-in page of a network that asks for one.
    #[effect(job, label = "Open network sign-in")]
    OpenNetworkSignIn,
    /// Start the hotspot; what is omitted is what the hotspot form holds.
    #[effect(job, label = "Start hotspot")]
    StartHotspot {
        #[param("The hotspot's network name")]
        name: Option<String>,
        #[param(secret, "The hotspot's password, at least 8 characters")]
        password: Option<String>,
        #[param("The band", choices = HOTSPOT_BANDS.iter().copied())]
        band: Option<String>,
    },
    /// Show or hide the QR code others scan to join the hotspot.
    #[effect(view, label = "Share hotspot QR code")]
    ShowHotspotCode,
    /// Show the hotspot's setup: its name, password and band.
    #[effect(view, label = "Hotspot setup")]
    ShowHotspotSetup,
    /// Ask for the hotspot's password with the password prompt.
    #[effect(view, label = "Hotspot password")]
    AskHotspotPassword,
    /// Show the advanced network settings: work or school networks,
    /// diagnostics.
    #[effect(view, label = "Advanced network settings")]
    ShowAdvancedNetworkSettings,
    /// Show or hide the enterprise (802.1X) network setup.
    #[effect(view, label = "Enterprise network setup")]
    ShowEnterpriseSetup,
    /// Choose the organization's CA certificate file.
    #[effect(label = "Choose CA certificate")]
    ChooseCaCertificate,
    /// Save an enterprise (802.1X) Wi-Fi network and connect; what is
    /// omitted is what the setup form holds, and a password omitted is asked
    /// for with the password prompt.
    #[effect(job, label = "Save and connect")]
    ConnectEnterpriseNetwork {
        #[param("The profile's name")]
        profile: Option<String>,
        #[param("The network's name (SSID)")]
        ssid: Option<String>,
        identity: Option<String>,
        anonymous_identity: Option<String>,
        #[param("The CA certificate's path")]
        ca_certificate: Option<String>,
        #[param("PEAP, TTLS or TLS")]
        method: Option<String>,
        #[param("MSCHAPv2, PAP or CHAP")]
        inner_auth: Option<String>,
        client_certificate: Option<String>,
        private_key: Option<String>,
        #[param(secret, "The password")]
        password: Option<String>,
    },
}

/// Answer `effect`: a change starts on a worker and reports when it lands.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: NetworkEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if effect == NetworkEffect::InspectNetworkState {
        return crate::panes::network::area(app)
            .ok_or_else(crate::area::missing)?
            .inspection()
            .map(Effected::value);
    }
    if effect == NetworkEffect::RefreshNetworks {
        // A dongle plugged in a moment ago is seen without waiting.
        app.host().radios().look_again();
    }
    let begun = crate::panes::network::area(app)
        .ok_or_else(crate::area::missing)?
        .model()
        .borrow_mut()
        .begin(effect)?;
    Ok(answer_begun(begun, cx))
}

/// Carry out what an effect began: nothing more, or its work on a worker,
/// landing in the model.
pub(crate) fn answer_begun(begun: Begun, cx: &mut EffectCx<Settings, SettingsApp>) -> Effected {
    match begun {
        Begun::Done => Effected::done(),
        Begun::Work(work) => {
            crate::device_read::job(cx, work, |app: &mut SettingsApp, landed: Landed, _| {
                crate::panes::network::area(app)
                    .ok_or_else(crate::area::missing)?
                    .model()
                    .borrow_mut()
                    .land(landed)
            })
        }
    }
}

fn connectivity_shown(state: ConnectivityState) -> String {
    t(match state {
        ConnectivityState::Unknown => "net-connectivity-unknown",
        ConnectivityState::Offline => "net-connectivity-offline",
        ConnectivityState::Portal => "net-connectivity-portal",
        ConnectivityState::Limited => "net-connectivity-limited",
        ConnectivityState::Full => "net-connectivity-full",
    })
}

fn band_shown(id: &str) -> String {
    t(match id {
        "2.4 GHz" => "net-band-24",
        "5 GHz" => "net-band-5",
        _ => "net-band-auto",
    })
}

fn band_from_label(label: &str) -> HotspotBand {
    if label == "2.4 GHz" || label == t("net-band-24") {
        HotspotBand::TwoPointFourGhz
    } else if label == "5 GHz" || label == t("net-band-5") {
        HotspotBand::FiveGhz
    } else {
        HotspotBand::Automatic
    }
}

fn metered_id(policy: os_actions::MeteredPolicy) -> &'static str {
    match policy {
        os_actions::MeteredPolicy::Automatic => "Automatic",
        os_actions::MeteredPolicy::Metered => "Metered",
        os_actions::MeteredPolicy::Unmetered => "Unmetered",
    }
}

fn metered_shown(id: &str) -> String {
    t(match id {
        "Metered" => "net-metered-yes",
        "Unmetered" => "net-metered-no",
        _ => "net-metered-auto",
    })
}

fn metered_from_label(label: &str) -> Option<os_actions::MeteredPolicy> {
    if label == "Metered" || label == t("net-metered-yes") {
        Some(os_actions::MeteredPolicy::Metered)
    } else if label == "Unmetered" || label == t("net-metered-no") {
        Some(os_actions::MeteredPolicy::Unmetered)
    } else if label == "Automatic" || label == t("net-metered-auto") {
        Some(os_actions::MeteredPolicy::Automatic)
    } else {
        None
    }
}

fn mac_id(policy: os_actions::MacPolicy) -> &'static str {
    match policy {
        os_actions::MacPolicy::Permanent => "Device address",
        os_actions::MacPolicy::Stable => "Random for this network",
        os_actions::MacPolicy::Random => "Random each time",
    }
}

fn mac_shown(id: &str) -> String {
    t(match id {
        "Device address" => "net-mac-permanent",
        "Random each time" => "net-mac-random",
        _ => "net-mac-stable",
    })
}

fn mac_from_label(label: &str) -> Option<os_actions::MacPolicy> {
    if label == "Random each time" || label == t("net-mac-random") {
        Some(os_actions::MacPolicy::Random)
    } else if label == "Device address" || label == t("net-mac-permanent") {
        Some(os_actions::MacPolicy::Permanent)
    } else if label == "Random for this network" || label == t("net-mac-stable") {
        Some(os_actions::MacPolicy::Stable)
    } else {
        None
    }
}

fn ipv6_id(method: Ipv6Method) -> &'static str {
    match method {
        Ipv6Method::Automatic => "Automatic",
        Ipv6Method::Manual => "Manual",
        Ipv6Method::LinkLocal => "Link-local only",
        Ipv6Method::Disabled => "Off",
    }
}

fn ipv6_shown(id: &str) -> String {
    t(match id {
        "Manual" => "net-ipv6-manual",
        "Link-local only" => "net-ipv6-link-local",
        "Off" => "net-ipv6-off",
        _ => "net-ipv6-auto",
    })
}

fn ipv6_from_label(label: &str) -> Option<Ipv6Method> {
    IPV6_METHODS
        .iter()
        .find(|id| **id == label || ipv6_shown(id) == label)
        .map(|id| match *id {
            "Manual" => Ipv6Method::Manual,
            "Link-local only" => Ipv6Method::LinkLocal,
            "Off" => Ipv6Method::Disabled,
            _ => Ipv6Method::Automatic,
        })
}

fn proxy_shown(id: &str) -> String {
    t(match id {
        "Automatic" => "net-proxy-auto",
        "Manual" => "net-proxy-manual",
        _ => "net-proxy-none",
    })
}

fn proxy_from_label(label: &str) -> Option<&'static str> {
    PROXY_MODES
        .iter()
        .copied()
        .find(|id| *id == label || proxy_shown(id) == label)
}

fn radio_shown(radio: Radio) -> String {
    t(match radio {
        Radio::Airplane => "net-radio-airplane",
        Radio::Hotspot => "net-radio-hotspot",
    })
}

fn profile_label(profile: &SavedNetworkProfile) -> String {
    let short_uuid: String = profile.uuid.chars().take(8).collect();
    format!("{} ({})", profile.name, short_uuid)
}

/// The entries of a list separated by commas or spaces, at most 17.
fn list_of(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|v| !v.is_empty())
        .take(17)
        .map(str::to_string)
        .collect()
}

/// `Some(value)` unless it is blank.
fn non_blank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

/// What a secret buffer holds, leaving it zeroed.
fn take_secret(buffer: &Mutex<String>) -> Zeroizing<String> {
    buffer.lock().map_or_else(
        |_| Zeroizing::new(String::new()),
        |mut value| Zeroizing::new(std::mem::take(&mut *value)),
    )
}

fn load_profile_settings(
    profiles: &[SavedNetworkProfile],
) -> HashMap<String, os_actions::ConnectionProfileSettings> {
    profiles
        .iter()
        .filter_map(|p| {
            os_actions::connection_profile_settings(&p.uuid)
                .ok()
                .map(|s| (p.uuid.clone(), s))
        })
        .collect()
}

/// A network in range in words: whether it is secured, and how strong.
fn network_detail(net: &WifiNetworkInfo) -> String {
    let security = t(if net.secure {
        "net-security-secured"
    } else {
        "net-security-open"
    });
    format!("{security}, {}", wifi_signal_words(net.signal))
}

/// How strong a 0..=4 bar signal is, in words.
fn wifi_signal_words(signal: u8) -> String {
    t(match signal {
        0 => "net-signal-unknown",
        1 => "net-signal-weak",
        2 => "net-signal-fair",
        3 => "net-signal-good",
        _ => "net-signal-strong",
    })
}

/// How a network checks who joins it, in words.
fn security_shown(security: os_actions::WifiSecurity) -> String {
    t(match security {
        os_actions::WifiSecurity::Open => "net-security-type-open",
        os_actions::WifiSecurity::Owe => "net-security-type-owe",
        os_actions::WifiSecurity::Wpa2 => "net-security-type-wpa2",
        os_actions::WifiSecurity::Wpa3 => "net-security-type-wpa3",
        os_actions::WifiSecurity::Enterprise => "net-security-type-enterprise",
        os_actions::WifiSecurity::Unknown => "net-security-type-other",
    })
}

/// A configured VPN or `WireGuard` connection and whether it is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnInfo {
    pub name: String,
    /// `NetworkManager`'s id for it, which never changes.
    pub uuid: String,
    /// `wireguard`, or `vpn` for a plugin's.
    pub kind: String,
    /// A plugin's service type, which says its protocol
    /// (`org.freedesktop.NetworkManager.openvpn`).
    pub service: Option<String>,
    pub active: bool,
}

fn portable_security(
    security: os_actions::WifiSecurity,
) -> Option<ui::collaboration::PortableWifiSecurity> {
    match security {
        os_actions::WifiSecurity::Wpa2 => Some(ui::collaboration::PortableWifiSecurity::Wpa2),
        os_actions::WifiSecurity::Wpa3 => Some(ui::collaboration::PortableWifiSecurity::Wpa3),
        _ => None,
    }
}

/// One part of a snapshot: `None` when it was not read (the model keeps what
/// it has), otherwise the value or why it could not be read.
type Part<T> = Option<Result<T, String>>;

/// The person's devices as VPNs in a snapshot: not read (the model keeps
/// what it has), or what the session publishes, `None` while it publishes
/// nothing.
enum DeviceVpnPart {
    NotRead,
    Read(Option<DeviceVpnStatus>),
}

/// Everything Network shows from `NetworkManager`, read in one pass off the
/// UI thread. Each part keeps its own failure, so one unreadable value is
/// shown as unreadable instead of as a device state.
pub(crate) struct NetworkSnapshot {
    networks: Result<Vec<WifiNetworkInfo>, String>,
    wifi_enabled: Part<bool>,
    wired: Part<WiredInfo>,
    radios: Part<os_actions::RadioState>,
    connectivity: Part<ConnectivityState>,
    profiles: Part<Vec<SavedNetworkProfile>>,
    profile_settings: HashMap<String, os_actions::ConnectionProfileSettings>,
    wake_settings: HashMap<String, Result<NetworkWakeStatus, String>>,
    hotspot_clients: Part<Vec<os_actions::HotspotClient>>,
    vpns: Part<Vec<VpnInfo>>,
    /// The Wi-Fi link in use; `None` when not read or not on Wi-Fi.
    link: Option<WifiLinkDetails>,
    /// The VPN plugins installed; read only for the first snapshot.
    plugins: Option<Vec<VpnPlugin>>,
    /// What the session publishes of the person's devices as VPNs; `None`
    /// when not read.
    device_vpn: DeviceVpnPart,
}

impl NetworkSnapshot {
    /// Query `NetworkManager`. Every call here may block on `nmcli` or the
    /// system bus, so this never runs on the UI thread.
    fn read(initial: bool) -> Self {
        let profiles = os_actions::list_saved_network_profiles();
        Self {
            wake_settings: profiles
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|profile| {
                    matches!(profile.kind.as_str(), "802-11-wireless" | "802-3-ethernet")
                })
                .map(|profile| (profile.uuid.clone(), host_network_wake(&profile.uuid, None)))
                .collect(),
            profile_settings: profiles
                .as_deref()
                .map(load_profile_settings)
                .unwrap_or_default(),
            profiles: Some(profiles),
            plugins: initial.then(os_actions::network_settings::installed_vpn_plugins),
            device_vpn: DeviceVpnPart::Read(DeviceVpnStatus::load()),
            ..Self::status()
        }
    }

    /// What changes as the device runs: the radios, the links, the
    /// networks in range, the hotspot's clients and the VPNs. The saved
    /// profiles, their settings and the session's device VPN (whose file
    /// is followed on its own) are not read, so adopting it leaves them.
    fn status() -> Self {
        let radios = os_actions::get_radio_state();
        let networks = os_actions::list_wifi_networks();
        let on_wifi = networks
            .as_ref()
            .is_ok_and(|networks| networks.iter().any(|network| network.connected));
        Self {
            link: on_wifi
                .then(os_actions::network_settings::wifi_link_details)
                .and_then(Result::ok),
            networks,
            wifi_enabled: Some(wifi_enabled_from(&radios)),
            wired: Some(os_actions::get_wired()),
            radios: Some(radios),
            connectivity: Some(os_actions::get_connectivity()),
            profiles: None,
            profile_settings: HashMap::new(),
            wake_settings: HashMap::new(),
            hotspot_clients: Some(os_actions::list_hotspot_clients()),
            vpns: Some(os_actions::list_vpn_connections().map(|vpns| {
                vpns.into_iter()
                    .map(|v| VpnInfo {
                        service: (v.kind == "vpn")
                            .then(|| os_actions::network_settings::vpn_service_type(&v.uuid))
                            .and_then(Result::ok)
                            .flatten(),
                        name: v.name,
                        uuid: v.uuid,
                        kind: v.kind,
                        active: v.active,
                    })
                    .collect()
            })),
            plugins: None,
            device_vpn: DeviceVpnPart::NotRead,
        }
    }

    /// `networks` alone: every other part is not read, so adopting it
    /// leaves them as they are.
    fn networks_only(networks: Result<Vec<WifiNetworkInfo>, String>) -> Self {
        Self {
            networks,
            wifi_enabled: None,
            wired: None,
            radios: None,
            connectivity: None,
            profiles: None,
            profile_settings: HashMap::new(),
            wake_settings: HashMap::new(),
            hotspot_clients: None,
            vpns: None,
            link: None,
            plugins: None,
            device_vpn: DeviceVpnPart::NotRead,
        }
    }

    /// Only `networks`, with every other part read and empty.
    fn of_networks(networks: Vec<WifiNetworkInfo>) -> Self {
        Self {
            networks: Ok(networks),
            wifi_enabled: Some(Ok(true)),
            wired: Some(Ok(WiredInfo::default())),
            radios: Some(Ok(os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..os_actions::RadioState::default()
            })),
            connectivity: Some(Ok(ConnectivityState::Full)),
            profiles: Some(Ok(Vec::new())),
            profile_settings: HashMap::new(),
            wake_settings: HashMap::new(),
            hotspot_clients: Some(Ok(Vec::new())),
            vpns: Some(Ok(Vec::new())),
            link: None,
            plugins: Some(Vec::new()),
            device_vpn: DeviceVpnPart::Read(None),
        }
    }
}

/// How often the open page scans for networks while it shows them: often
/// enough that walking into range shows up while the person looks, rarely
/// enough that the radio is not kept busy.
const SCAN_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The parts of the last snapshot that could not be read, each with the
/// reason. A part listed here is shown as unreadable, never as a state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NetworkReadErrors {
    networks: Option<String>,
    wifi_enabled: Option<String>,
    wired: Option<String>,
    radios: Option<String>,
    connectivity: Option<String>,
    profiles: Option<String>,
    hotspot_clients: Option<String>,
    vpns: Option<String>,
}

/// The Wi-Fi switch position from the radio state. A machine without a Wi-Fi
/// adapter has its Wi-Fi off — a state the Wi-Fi card names — and only a radio
/// state that could not be read is an error.
fn wifi_enabled_from(radios: &Result<os_actions::RadioState, String>) -> Result<bool, String> {
    radios
        .as_ref()
        .map(|radios| radios.wifi_available && radios.wifi_enabled)
        .map_err(Clone::clone)
}

/// Keep `value` on success and record why it failed otherwise. `None` (the
/// part was not read this time) leaves both as they were.
fn adopt<T>(part: Part<T>, error: &mut Option<String>) -> Option<T> {
    match part? {
        Ok(value) => {
            *error = None;
            Some(value)
        }
        Err(reason) => {
            warn!(target: "tilo::settings", %reason, "network read failed");
            *error = Some(reason);
            None
        }
    }
}

/// A radio Network switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Radio {
    Airplane,
    Hotspot,
}

/// A change to this device's networking, carried out off the UI thread.
/// Secrets are zeroed when it is dropped.
pub(crate) enum NetworkChange {
    Wifi(bool),
    Wired(bool),
    Radio(Radio, bool),
    Autoconnect {
        uuid: String,
        name: String,
        on: bool,
    },
    Wake {
        uuid: String,
        on: bool,
    },
    Forget {
        uuid: String,
        name: String,
    },
    Hotspot(HotspotConfiguration),
    Ipv4 {
        uuid: String,
        name: String,
        config: IpConfiguration,
    },
    Ipv6 {
        uuid: String,
        name: String,
        config: Ipv6Configuration,
    },
    Dns {
        uuid: String,
        name: String,
        servers: Vec<String>,
    },
    Policy {
        uuid: String,
        name: String,
        patch: os_actions::ConnectionPolicyPatch,
    },
    Identity {
        uuid: String,
        name: String,
        identity: EnterpriseIdentity,
    },
    Password {
        uuid: String,
        name: String,
        password: Zeroizing<String>,
    },
    Connect {
        ssid: String,
        password: Option<Zeroizing<String>>,
    },
    JoinHidden {
        ssid: String,
        password: Option<Zeroizing<String>>,
    },
    Enterprise {
        config: EnterpriseWifiConfiguration,
        password: Zeroizing<String>,
    },
    /// Bring a VPN up or down.
    Vpn {
        uuid: String,
        name: String,
        on: bool,
    },
    VpnProfile {
        uuid: String,
        name: String,
        patch: VpnProfilePatch,
    },
    VpnDelete {
        uuid: String,
        name: String,
    },
}

impl Drop for NetworkChange {
    fn drop(&mut self) {
        if let Self::Hotspot(config) = self {
            config.password.zeroize();
        }
    }
}

/// A VPN to set up.
pub(crate) enum VpnSetup {
    /// A configuration file's text, of `kind` (`wireguard` or `openvpn`).
    Text {
        kind: &'static str,
        name: String,
        text: Zeroizing<String>,
    },
    /// A configuration file the person picked.
    File { kind: &'static str, path: PathBuf },
    /// A profile of an installed plugin's type.
    Plugin {
        service: String,
        name: String,
        server: String,
    },
}

/// What carries out Network's reads and changes: `NetworkManager` on a
/// device, a fixed sample in a screenshot or test.
pub(crate) trait NetworkService: Send + Sync {
    /// Read everything Network shows; `initial` also reads the installed
    /// VPN plugins.
    /// Connectivity is the last verdict; this never probes the internet,
    /// and the networks are those last seen: it never waits for a scan.
    fn snapshot(&self, initial: bool) -> NetworkSnapshot;
    /// Scan for Wi-Fi networks and return once the scan has finished, so the
    /// snapshot that follows lists what the radio sees now. A scan takes as
    /// long as the hardware needs, so it runs on its own worker (the open
    /// page's periodic look, an explicit refresh) and no reading waits for
    /// it.
    ///
    /// # Errors
    /// Why the device could not scan.
    fn scan(&self) -> Result<(), String> {
        Ok(())
    }
    /// The networks in range as last seen, and nothing else: what the
    /// periodic look reads after its scan.
    fn networks(&self) -> NetworkSnapshot {
        NetworkSnapshot::networks_only(self.snapshot(false).networks)
    }
    /// What changes as the device runs (the radios, the links, the
    /// networks in range, the VPNs) without the saved profiles and their
    /// settings: what is read again when the device reports a change.
    fn status(&self) -> NetworkSnapshot {
        self.snapshot(false)
    }
    /// Follow the device: `changed` is called, on another thread, each time
    /// it reports a change Network shows, for as long as the answer is
    /// held. `None` when nothing can be followed.
    fn follow(&self, _changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        None
    }
    /// Ask the device to check internet connectivity again, so the next
    /// snapshot carries a fresh verdict. Only the explicit refresh does this.
    fn recheck_connectivity(&self) {}
    /// What the session publishes of the person's devices as VPNs; `None`
    /// while no session publishes it.
    fn device_vpn(&self) -> Option<DeviceVpnStatus> {
        None
    }
    /// Carry out `change`.
    ///
    /// # Errors
    /// Why the device refused or failed it.
    fn change(&self, change: &NetworkChange) -> Result<(), String>;
    /// Set up a VPN; its new profile's id.
    ///
    /// # Errors
    /// Why it could not be set up.
    fn create_vpn(&self, setup: &VpnSetup) -> Result<String, String>;
    /// A VPN profile's settings.
    ///
    /// # Errors
    /// Why they could not be read.
    fn vpn_settings(&self, uuid: &str) -> Result<VpnProfileSettings, String>;
    /// A VPN profile as the file Export writes, its secrets included.
    ///
    /// # Errors
    /// Why it could not be exported.
    fn export_vpn(&self, uuid: &str) -> Result<Zeroizing<String>, String>;
    /// A saved Wi-Fi network's password, which asks the person to
    /// authenticate where the system requires it.
    ///
    /// # Errors
    /// Why it could not be read.
    fn wifi_password(&self, uuid: &str) -> Result<Zeroizing<String>, String>;
    /// Ask the person's Wi-Fi continuity service.
    ///
    /// # Errors
    /// Why the service could not be asked.
    fn continuity(&self, command: WifiContinuityCommand) -> Result<WifiContinuityReply, String>;
    /// Diagnose the connection.
    ///
    /// # Errors
    /// Why it could not be diagnosed.
    fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String>;
    /// The captive portal's sign-in page.
    ///
    /// # Errors
    /// Why there is none to open.
    fn sign_in_page(&self) -> Result<String, String>;
}

/// This device's `NetworkManager`, its radios switched through the
/// dispatcher.
struct DeviceNetwork {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
}

impl NetworkService for DeviceNetwork {
    fn networks(&self) -> NetworkSnapshot {
        NetworkSnapshot::networks_only(os_actions::list_wifi_networks())
    }

    fn snapshot(&self, initial: bool) -> NetworkSnapshot {
        NetworkSnapshot::read(initial)
    }

    fn status(&self) -> NetworkSnapshot {
        NetworkSnapshot::status()
    }

    fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        match os_actions::network::follow_network_changes(changed) {
            Ok(follow) => Some(Box::new(follow)),
            Err(error) => {
                warn!(target: "tilo::settings", %error, "NetworkManager's changes are not followed");
                None
            }
        }
    }

    fn device_vpn(&self) -> Option<DeviceVpnStatus> {
        DeviceVpnStatus::load()
    }

    fn recheck_connectivity(&self) {
        // A refused or failed recheck still leaves the last verdict, which
        // the snapshot that follows reads and reports.
        let _ = os_actions::check_connectivity_now();
    }

    fn scan(&self) -> Result<(), String> {
        os_actions::scan_wifi()
    }

    fn change(&self, change: &NetworkChange) -> Result<(), String> {
        use os_actions::network_settings as nm;
        let dispatcher = &self.dispatcher;
        match change {
            NetworkChange::Wifi(on) => dispatcher.set_wifi(*on),
            NetworkChange::Wired(on) => dispatcher.set_wired(*on),
            NetworkChange::Radio(Radio::Airplane, on) => dispatcher.set_airplane_mode(*on),
            NetworkChange::Radio(Radio::Hotspot, on) => dispatcher.set_hotspot(*on),
            NetworkChange::Autoconnect { uuid, on, .. } => {
                os_actions::set_network_autoconnect(uuid, *on)
            }
            NetworkChange::Wake { uuid, on } => {
                let status = host_network_wake(uuid, Some(*on))?;
                status.problem.map_or(Ok(()), Err)
            }
            NetworkChange::Forget { uuid, .. } => os_actions::forget_network_profile(uuid),
            NetworkChange::Hotspot(config) => os_actions::configure_hotspot(config),
            NetworkChange::Ipv4 { uuid, config, .. } => os_actions::configure_ipv4(uuid, config),
            NetworkChange::Ipv6 { uuid, config, .. } => nm::configure_ipv6(uuid, config),
            NetworkChange::Dns { uuid, servers, .. } => nm::configure_dns(uuid, servers),
            NetworkChange::Policy { uuid, patch, .. } => {
                os_actions::configure_connection_policy_patch(uuid, patch)
            }
            NetworkChange::Identity { uuid, identity, .. } => {
                nm::configure_enterprise_identity(uuid, identity)
            }
            NetworkChange::Password { uuid, password, .. } => {
                nm::set_wifi_profile_password(uuid, password)
            }
            NetworkChange::Connect { ssid, password } => {
                dispatcher.connect_wifi(ssid, password.as_deref().map(String::as_str))
            }
            NetworkChange::JoinHidden { ssid, password } => {
                nm::connect_hidden_wifi(ssid, password.as_deref().map(String::as_str))
            }
            NetworkChange::Enterprise { config, password } => {
                os_actions::connect_enterprise_wifi(config, password)
            }
            NetworkChange::Vpn { uuid, on, .. } => os_actions::set_vpn_connection_uuid(uuid, *on),
            NetworkChange::VpnProfile { uuid, patch, .. } => nm::configure_vpn_profile(uuid, patch),
            NetworkChange::VpnDelete { uuid, .. } => os_actions::delete_vpn_connection_uuid(uuid),
        }
    }

    fn create_vpn(&self, setup: &VpnSetup) -> Result<String, String> {
        use os_actions::network_settings as nm;
        match setup {
            VpnSetup::Text { kind, name, text } => nm::import_vpn_text(kind, name, text),
            VpnSetup::File { kind, path } => nm::import_path_finding_uuid(kind, path),
            VpnSetup::Plugin {
                service,
                name,
                server,
            } => nm::create_plugin_vpn(service, name, server),
        }
    }

    fn vpn_settings(&self, uuid: &str) -> Result<VpnProfileSettings, String> {
        os_actions::network_settings::vpn_profile_settings(uuid)
    }

    fn export_vpn(&self, uuid: &str) -> Result<Zeroizing<String>, String> {
        os_actions::network_settings::export_vpn(uuid)
    }

    fn wifi_password(&self, uuid: &str) -> Result<Zeroizing<String>, String> {
        os_actions::network_settings::wifi_profile_password(uuid)
    }

    fn continuity(&self, command: WifiContinuityCommand) -> Result<WifiContinuityReply, String> {
        ui::collaboration::wifi_continuity_command(command)
    }

    fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String> {
        os_actions::connection_diagnostics()
    }

    fn sign_in_page(&self) -> Result<String, String> {
        os_actions::captive_portal_uri()
    }
}

/// A street's worth of networks, a wired port, two VPNs, for a screenshot:
/// every change is accepted and reaches nothing.
struct SampleNetwork;

const SAMPLE_HOME: &str = "5b2e8a70-1c7d-4f2e-9a61-0d3c9e4b7a10";
const SAMPLE_LIBRARY: &str = "9e41c2d8-6a35-4b9f-8c07-2f5d1e8a3b64";
const SAMPLE_HOTEL: &str = "3c8f9b21-7d44-4e0a-b5c6-1a2b3c4d5e6f";
const SAMPLE_WIRED: &str = "0f1e2d3c-4b5a-4968-8776-655443322110";
const SAMPLE_VPN_HOME: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
const SAMPLE_VPN_WORK: &str = "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e";
/// The person's other devices, by their endpoint ids.
const SAMPLE_DESK_PC: &str = "6f1d2c3b4a5968778695a4b3c2d1e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091";
const SAMPLE_STUDIO: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";
const SAMPLE_PHONE: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";

impl SampleNetwork {
    fn networks() -> Vec<WifiNetworkInfo> {
        use os_actions::WifiSecurity::{Open, Wpa2, Wpa3};
        let network = |ssid: &str, signal, security, connected, known| WifiNetworkInfo {
            ssid: ssid.into(),
            signal,
            secure: !matches!(security, os_actions::WifiSecurity::Open),
            security,
            connected,
            known,
        };
        vec![
            network("Home_Network", 4, Wpa3, true, true),
            network("Cafe_Guest", 3, Open, false, false),
            network("Office_5G", 2, Wpa2, false, false),
            network("Library", 3, Wpa2, false, true),
            network("Neighbor_2G", 1, Wpa2, false, false),
            network("Printer_Direct", 2, Wpa2, false, false),
            network("Airport_Free", 2, Open, false, false),
            network("Studio_5", 1, Wpa3, false, false),
            network("Bakery", 1, Wpa2, false, false),
        ]
    }

    fn profiles(now: u64) -> Vec<SavedNetworkProfile> {
        let profile =
            |uuid: &str, name: &str, kind: &str, autoconnect, ago: u64| SavedNetworkProfile {
                uuid: uuid.into(),
                name: name.into(),
                kind: kind.into(),
                autoconnect,
                last_used: Some(now.saturating_sub(ago)),
            };
        vec![
            profile(SAMPLE_HOME, "Home_Network", "802-11-wireless", true, 600),
            profile(
                SAMPLE_LIBRARY,
                "Library",
                "802-11-wireless",
                true,
                86_400 + 3_600,
            ),
            profile(
                SAMPLE_HOTEL,
                "Hotel_Lisbon",
                "802-11-wireless",
                false,
                40 * 86_400,
            ),
            profile(
                SAMPLE_WIRED,
                "Wired connection 1",
                "802-3-ethernet",
                true,
                7 * 86_400,
            ),
        ]
    }

    fn profile_settings() -> os_actions::ConnectionProfileSettings {
        os_actions::ConnectionProfileSettings {
            metered: Some(os_actions::MeteredPolicy::Automatic),
            mac: Some(os_actions::MacPolicy::Stable),
            proxy: Some(os_actions::ProxyConfiguration::default()),
            ipv4: Some(IpConfiguration {
                automatic: true,
                ..IpConfiguration::default()
            }),
            priority: Some(0),
            hidden: Some(false),
            ipv6: Some(Ipv6Configuration::default()),
            enterprise: None,
        }
    }

    fn vpn_settings(uuid: &str) -> Result<VpnProfileSettings, String> {
        match uuid {
            SAMPLE_VPN_HOME => Ok(VpnProfileSettings {
                name: "Home".into(),
                kind: "wireguard".into(),
                service: None,
                autoconnect: false,
                all_traffic: true,
                addresses: vec!["10.7.0.2/32".into()],
                dns: vec!["10.7.0.1".into()],
                mtu: None,
                endpoint: Some("home.example.net:51820".into()),
                allowed_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
                peer_public_key: Some("kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0E=".into()),
                keepalive: Some(25),
            }),
            SAMPLE_VPN_WORK => Ok(VpnProfileSettings {
                name: "Work".into(),
                kind: "vpn".into(),
                service: Some("org.freedesktop.NetworkManager.openvpn".into()),
                autoconnect: false,
                all_traffic: false,
                addresses: vec!["10.20.0.17/24".into()],
                dns: vec!["10.20.0.53".into(), "10.20.0.54".into()],
                mtu: Some(1400),
                endpoint: Some("vpn.example.com:1194".into()),
                ..VpnProfileSettings::default()
            }),
            _ => Err("a sample has no such VPN".into()),
        }
    }
}

impl NetworkService for SampleNetwork {
    fn snapshot(&self, _initial: bool) -> NetworkSnapshot {
        let mut snapshot = NetworkSnapshot::of_networks(Self::networks());
        // A laptop on Wi-Fi with its Ethernet port empty.
        snapshot.wired = Some(Ok(WiredInfo {
            connected: false,
            device: Some("enp3s0".into()),
            speed: None,
            ipv4: None,
        }));
        // A fixed moment, so the captures read the same every day.
        let now = 1_792_000_000;
        snapshot.profiles = Some(Ok(Self::profiles(now)));
        snapshot.profile_settings = [SAMPLE_HOME, SAMPLE_LIBRARY, SAMPLE_HOTEL]
            .into_iter()
            .map(|uuid| (uuid.to_string(), Self::profile_settings()))
            .collect();
        snapshot.link = Some(WifiLinkDetails {
            device: "wlan0".into(),
            frequency_mhz: Some(5180),
            rate: Some("866 Mbit/s".into()),
            hardware_address: Some("3A:5F:C2:91:0E:7B".into()),
            ipv4: vec!["192.168.1.24/24".into()],
            ipv6: vec!["fd00:1::24/64".into()],
            gateway: Some("192.168.1.1".into()),
            dns: vec!["192.168.1.1".into()],
        });
        snapshot.vpns = Some(Ok(vec![
            VpnInfo {
                name: "Home".into(),
                uuid: SAMPLE_VPN_HOME.into(),
                kind: "wireguard".into(),
                service: None,
                active: true,
            },
            VpnInfo {
                name: "Work".into(),
                uuid: SAMPLE_VPN_WORK.into(),
                kind: "vpn".into(),
                service: Some("org.freedesktop.NetworkManager.openvpn".into()),
                active: false,
            },
        ]));
        snapshot.plugins = Some(vec![VpnPlugin {
            service: "org.freedesktop.NetworkManager.openvpn".into(),
            name: "OpenVPN".into(),
        }]);
        snapshot.device_vpn = DeviceVpnPart::Read(self.device_vpn());
        snapshot
    }

    fn device_vpn(&self) -> Option<DeviceVpnStatus> {
        Some(DeviceVpnStatus {
            exit_available: true,
            offered: vec![
                OfferedExit {
                    device: SAMPLE_DESK_PC.into(),
                    label: "Desk PC".into(),
                },
                OfferedExit {
                    device: SAMPLE_STUDIO.into(),
                    label: "Studio Mac mini".into(),
                },
            ],
            ..DeviceVpnStatus::default()
        })
    }

    fn change(&self, _change: &NetworkChange) -> Result<(), String> {
        Ok(())
    }

    fn create_vpn(&self, _setup: &VpnSetup) -> Result<String, String> {
        Err("a sample sets nothing up".into())
    }

    fn vpn_settings(&self, uuid: &str) -> Result<VpnProfileSettings, String> {
        Self::vpn_settings(uuid)
    }

    fn export_vpn(&self, _uuid: &str) -> Result<Zeroizing<String>, String> {
        Err("a sample exports nothing".into())
    }

    fn wifi_password(&self, _uuid: &str) -> Result<Zeroizing<String>, String> {
        Err("a sample has no passwords".into())
    }

    fn continuity(&self, command: WifiContinuityCommand) -> Result<WifiContinuityReply, String> {
        Ok(match command {
            WifiContinuityCommand::Lookup { .. } => WifiContinuityReply::NotFound,
            _ => WifiContinuityReply::Rejected {
                reason: "a sample shares nothing".into(),
            },
        })
    }

    fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String> {
        Err("a sample has no connection to diagnose".into())
    }

    fn sign_in_page(&self) -> Result<String, String> {
        Err("a sample has no sign-in page".into())
    }
}

/// How an effect began: finished at once, or work for a worker.
pub(crate) enum Begun {
    Done,
    Work(Box<dyn FnOnce() -> Landed + Send>),
}

impl Begun {
    fn work(work: impl FnOnce() -> Landed + Send + 'static) -> Self {
        Self::Work(Box::new(work))
    }
}

/// How a connection attempt ended.
pub(crate) enum Joined {
    Connected { continuity_notice: Option<String> },
    PasswordRequired { continuity_warning: Option<String> },
}

/// What a saved network's password was read for.
pub(crate) enum PasswordFor {
    /// To show it on the network's page.
    Reveal,
    /// To draw the QR code that shares the network.
    Code { ssid: String, hidden: bool },
}

/// Work that finished on a worker, for the model to take in.
pub(crate) enum Landed {
    Refreshed {
        snapshot: Box<NetworkSnapshot>,
        scan: Result<(), String>,
    },
    Changed(NetworkChange, Result<(), String>),
    Connected {
        ssid: String,
        result: Result<Joined, String>,
    },
    Shared(Result<String, String>),
    Diagnosed(Result<os_actions::ConnectionDiagnostics, String>),
    SignedIn(Result<(), String>),
    Password {
        uuid: String,
        purpose: PasswordFor,
        result: Result<Zeroizing<String>, String>,
    },
    VpnRead {
        uuid: String,
        result: Result<VpnProfileSettings, String>,
    },
    /// A VPN turned on; one carrying all traffic turned the others off.
    VpnOn {
        change: NetworkChange,
        result: Result<(), String>,
        full_tunnel: bool,
    },
    /// The VPNs carrying all traffic turned off for a route through one of
    /// the person's devices.
    TunnelsLeft(Result<(), String>),
    VpnCreated(Result<String, String>),
    VpnExported(Result<(), String>),
}

/// The fields of one profile's IPv4 form.
struct Ipv4Form {
    automatic: LocalState<bool>,
    addresses: LocalState<String>,
    gateway: LocalState<String>,
    dns: LocalState<String>,
}

/// The fields of one profile's IPv6 form.
struct Ipv6Form {
    method: LocalState<String>,
    addresses: LocalState<String>,
    gateway: LocalState<String>,
}

/// The fields of one profile's proxy form.
struct ProxyForm {
    mode: LocalState<String>,
    address: LocalState<String>,
}

/// The fields of one work or school network's identity form.
struct IdentityForm {
    identity: LocalState<String>,
    ca: LocalState<String>,
}

/// What an enterprise connection was given; the setup form fills the rest.
#[derive(Clone)]
struct EnterpriseGiven {
    profile: Option<String>,
    ssid: Option<String>,
    identity: Option<String>,
    anonymous_identity: Option<String>,
    ca_certificate: Option<String>,
    method: Option<String>,
    inner_auth: Option<String>,
    client_certificate: Option<String>,
    private_key: Option<String>,
}

/// The enterprise network setup form.
struct EnterpriseForm {
    profile: LocalState<String>,
    ssid: LocalState<String>,
    identity: LocalState<String>,
    anonymous: LocalState<String>,
    ca: LocalState<String>,
    client_certificate: LocalState<String>,
    private_key: LocalState<String>,
    method: LocalState<String>,
    inner_auth: LocalState<String>,
}

impl EnterpriseForm {
    fn new() -> Self {
        Self {
            profile: LocalState::new(String::new()),
            ssid: LocalState::new(String::new()),
            identity: LocalState::new(String::new()),
            anonymous: LocalState::new(String::new()),
            ca: LocalState::new(String::new()),
            client_certificate: LocalState::new(String::new()),
            private_key: LocalState::new(String::new()),
            method: LocalState::new("PEAP".into()),
            inner_auth: LocalState::new("MSCHAPv2".into()),
        }
    }
}

/// The form that joins a network that does not broadcast its name.
/// A secret shown on a network's page, for the profile or network it
/// belongs to; zeroed when it goes.
struct Shown {
    of: String,
    secret: Zeroizing<String>,
}

/// Which radios Network can switch, each as the kernel lists it or
/// `NetworkManager` reported it: either one seeing a radio is enough, so a
/// dongle plugged in counts at once and a modem only `NetworkManager` knows
/// still counts. `None` while neither has said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NetworkRadios {
    wifi: Option<bool>,
    wwan: Option<bool>,
}

impl NetworkRadios {
    fn of(cx: &PaneCx, reported: Option<&os_actions::RadioState>) -> Self {
        let listed = cx.radios().now();
        let either = |listed: Option<bool>, reported: Option<bool>| match (listed, reported) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (None, None) => None,
            _ => Some(false),
        };
        Self {
            wifi: either(
                listed.map(|radios| radios.wifi),
                reported.map(|radios| radios.wifi_available),
            ),
            wwan: either(
                listed.map(|radios| radios.wwan),
                reported.map(|radios| radios.wwan_available),
            ),
        }
    }
}

/// What the Network area keeps for the window: the model, read when first
/// needed.
pub struct NetworkArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<NetworkModel>>,
}

impl NetworkArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    /// The model, where the networks were read; never reads them.
    pub(crate) fn read(&self) -> Option<&Shared<NetworkModel>> {
        self.model.get()
    }

    /// The model over this device's networks, or over sample networks for a
    /// screenshot.
    pub fn model(&self) -> &Shared<NetworkModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                NetworkModel::of_device(self.cx.clone())
            } else {
                NetworkModel::sample(self.cx.clone())
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Observe the already mounted model; reading never starts or polls a scan.
    fn inspection(&self) -> Result<serde_json::Value, String> {
        match self.model.get() {
            Some(model) => model.borrow().inspection(),
            None => Ok(
                serde_json::json!({"state":"unopened", "loaded":false, "networks":[],
                "source": if self.cx.reaches_devices() { "device" } else { "preview" }}),
            ),
        }
    }

    /// What this device cannot do with its networks now: Wi-Fi and a
    /// hotspot without a Wi-Fi radio, airplane mode without a Wi-Fi radio
    /// or a modem. Reads nothing itself: only what the
    /// radios and the area's model have read, so it never opens the area.
    pub(crate) fn statuses(&self) -> Vec<EffectStatus> {
        let reported = self.model.get().and_then(|model| {
            let model = model.try_borrow().ok()?;
            model.reported_radios().cloned()
        });
        let radios = NetworkRadios::of(&self.cx, reported.as_ref());
        let mut statuses = Vec::new();
        let mut unavailable = |effects: &[&'static str], code: &str, reason: &str| {
            statuses.extend(effects.iter().map(|&id| {
                EffectStatus::unavailable(
                    ActionRef::<SettingsEffect, ()>::new(id, &[]),
                    code,
                    reason,
                )
            }));
        };
        let wifi = radios.wifi == Some(false);
        let wwan = radios.wwan == Some(false);
        if wifi {
            unavailable(
                &[
                    NetworkEffect::SET_WIFI.id,
                    NetworkEffect::SET_HOTSPOT.id,
                    NetworkEffect::START_HOTSPOT.id,
                ],
                "no_adapter",
                &t("net-no-wifi-adapter"),
            );
        }
        if wifi && wwan {
            unavailable(
                &[NetworkEffect::SET_AIRPLANE_MODE.id],
                "no_radio",
                &t("net-no-radio"),
            );
        }
        statuses
    }

    /// Take in background results, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_background();
        }
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per Wi-Fi network (in range or saved), per other saved
    /// profile and per VPN, each offering what can be done with it.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let mut rows: Vec<LiveRow> = model
            .wifi_names()
            .iter()
            .map(|ssid| model.wifi_live(ssid))
            .collect();
        rows.extend(
            model
                .profiles
                .iter()
                .filter(|profile| profile.kind != "802-11-wireless")
                .map(|profile| model.profile_live(profile)),
        );
        rows.extend(model.vpn_rows());
        rows
    }
}

impl crate::area::PaneQuery for NetworkArea {
    fn set_query_text(&self, text: &str) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().set_filter(text);
        }
    }

    fn query_closed(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().set_filter("");
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent disclosure or read state of the pane"
)]
pub struct NetworkModel {
    cx: PaneCx,
    service: Arc<dyn NetworkService>,
    /// False until the first snapshot arrives; until then the pane shows a
    /// pending view.
    loaded: bool,
    snapshot: Option<crate::channel::Receiver<NetworkSnapshot>>,
    networks: Vec<WifiNetworkInfo>,
    wifi_enabled: bool,
    /// The radios the kernel listed when the model last looked.
    listed: Option<crate::radios::Present>,
    wired: WiredInfo,
    radios: os_actions::RadioState,
    connectivity: ConnectivityState,
    profiles: Vec<SavedNetworkProfile>,
    profile_settings: HashMap<String, os_actions::ConnectionProfileSettings>,
    wake_settings: HashMap<String, Result<NetworkWakeStatus, String>>,
    hotspot_clients: Vec<os_actions::HotspotClient>,
    vpns: Vec<VpnInfo>,
    link: Option<WifiLinkDetails>,
    plugins: Vec<VpnPlugin>,
    diagnostics: Option<os_actions::ConnectionDiagnostics>,
    /// Why each part of the last snapshot could not be read.
    read_errors: NetworkReadErrors,
    /// Changes running; a snapshot taken meanwhile does not overrule what
    /// they show.
    in_flight: usize,
    /// Explicit refresh operations whose worker result has not landed.
    refreshes_pending: usize,
    /// Networks being joined.
    connecting: Vec<String>,
    notice: Option<String>,
    /// What the window's password prompt asks for Network, while it is
    /// open.
    asking: Option<asking::Asking>,
    /// Whether the pointer is over the Wi-Fi list of the first page, and of
    /// the page of every network.
    held_short: Rc<Cell<bool>>,
    held_all: Rc<Cell<bool>>,
    /// Whether either list was held when the pane last looked.
    held_seen: bool,
    short_list: RefCell<order::HeldList>,
    all_list: RefCell<order::HeldList>,
    /// What the window bar's search narrows the page of every network to.
    filter: String,
    /// The window's pages when the pane last composed.
    pages_seen: u64,
    /// Whether the work or school network's setup is open under its row,
    /// and its reveal: the row's press and the setup action share it.
    enterprise_setup: ui::widget::DisclosureState,
    /// When the open page last scanned for networks: it scans again every
    /// [`SCAN_EVERY`] while it shows them.
    scanned_at: Option<ui::time::Instant>,
    /// A scan the page started has not landed yet.
    scan_in_flight: bool,
    hotspot_code_shown: bool,
    /// The saved password shown on a network's page.
    revealed: Option<Shown>,
    /// The QR code shown on a network's page, by network.
    code: Option<Shown>,
    /// The wired profile being edited, by label.
    wired_profile: LocalState<String>,
    seen_selection: String,
    hotspot_name: LocalState<String>,
    hotspot_password: Arc<Mutex<String>>,
    hotspot_band: LocalState<String>,
    ipv4_forms: HashMap<String, Ipv4Form>,
    ipv6_forms: HashMap<String, Ipv6Form>,
    proxy_forms: HashMap<String, ProxyForm>,
    identity_forms: HashMap<String, IdentityForm>,
    enterprise: EnterpriseForm,
    ca_picker: Option<ui::portal::PortalChannel>,
    vpn: vpn::VpnState,
    /// Work started by the pane itself (an import a picker answered), not
    /// by an effect: it lands here when done.
    jobs: Vec<crate::channel::Receiver<Landed>>,
    /// The modem's model, whose card the first page shows next to Wi-Fi
    /// and whose page is one of this pane's: the window's [`mobile::MobileArea`]
    /// keeps it, the pane composes and polls it.
    mobile: Option<Shared<mobile::MobileModel>>,
    /// What follows the device while a pane shows the model.
    following: std::rc::Weak<crate::watch::Counted>,
    /// The device's change count when the model last started a read.
    changes_seen: u64,
    changed: bool,
}

impl Drop for NetworkModel {
    fn drop(&mut self) {
        if let Ok(mut secret) = self.hotspot_password.lock() {
            secret.zeroize();
        }
    }
}

impl NetworkModel {
    /// Only state already adopted by the ordinary snapshot/completion pump.
    /// Bound the explicit read, and omit editable forms, profiles and secrets.
    fn inspection(&self) -> Result<serde_json::Value, String> {
        const MAX_ROWS: usize = 256;
        const MAX_BYTES: usize = 128 * 1024;
        let networks = self
            .networks
            .iter()
            .take(MAX_ROWS)
            .map(|network| {
                serde_json::json!({
                    "resource":wifi_row_id(&network.ssid), "ssid":network.ssid,
                    "signal_bucket":network.signal, "security":network.security,
                    "secure":network.secure, "connected":network.connected,
                    "visible":self.loaded && self.wifi_enabled,
                })
            })
            .collect::<Vec<_>>();
        let known_wifi = self.loaded && self.read_errors.wifi_enabled.is_none();
        let known_radios = self.loaded && self.read_errors.radios.is_none();
        let known_wired = self.loaded && self.read_errors.wired.is_none();
        let known_connectivity = self.loaded && self.read_errors.connectivity.is_none();
        let wifi_status = self.loaded.then(|| {
            if self.wifi_absent() {
                t("net-no-wifi-adapter")
            } else {
                self.wifi_status_error()
                    .unwrap_or_else(|| self.connection_status_text())
            }
        });
        let value = serde_json::json!({
            "source":if self.cx.reaches_devices() { "device" } else { "preview" },
            "state":if self.loaded { "ready" } else { "loading" }, "loaded":self.loaded,
            "page": self.page_key(),
            "wifi": { "available":known_radios.then_some(self.radios.wifi_available),
                "enabled":known_wifi.then_some(self.wifi_enabled), "status_label":wifi_status },
            "wired": { "device":known_wired.then_some(self.wired.device.as_deref()).flatten(),
                "connected":known_wired.then_some(self.wired.connected) },
            "connectivity": { "state":known_connectivity.then_some(match self.connectivity {
                ConnectivityState::Unknown => "unknown", ConnectivityState::Offline => "offline",
                ConnectivityState::Portal => "portal", ConnectivityState::Limited => "limited", ConnectivityState::Full => "full",
            }), "label":known_connectivity.then(|| connectivity_shown(self.connectivity)) },
            "scan": { "snapshot_pending":self.snapshot.is_some(), "jobs_pending":self.refreshes_pending },
            "changes_pending":self.in_flight, "networks":networks,
            "network_count":self.networks.len(), "networks_truncated":self.networks.len() > MAX_ROWS,
            "errors": { "networks":self.read_errors.networks, "wifi_enabled":self.read_errors.wifi_enabled,
                "radios":self.read_errors.radios, "wired":self.read_errors.wired, "connectivity":self.read_errors.connectivity },
            "labels": { "wifi":t("net-wifi"), "wifi_caption":t("net-wifi-caption"),
                "show_all":t_args("net-show-all", &[("count", self.networks.len().into())]),
                "refresh":t("net-refresh") },
        });
        let size = serde_json::to_vec(&value)
            .map_err(|_| "Network inspection could not be encoded.".to_string())?
            .len();
        if size > MAX_BYTES {
            return Err("Network inspection exceeds its bounded response size.".into());
        }
        Ok(value)
    }

    /// This device's networks: the pane is pending until the first
    /// snapshot, read in the background, arrives.
    fn of_device(cx: PaneCx) -> Self {
        let mut model = Self::with_service(
            cx,
            Arc::new(DeviceNetwork {
                dispatcher: dispatcher_or_real(None),
            }),
        );
        model.start_snapshot();
        model
    }

    /// Sample networks, on the page a network capture asks for.
    fn sample(cx: PaneCx) -> Self {
        use crate::scenarios::NetworkSample;
        let wanted = cx.scene().and_then(|scene| scene.network);
        let mut model = Self::loaded_from(cx, Arc::new(SampleNetwork));
        match wanted {
            Some(NetworkSample::WifiOff) => {
                model.wifi_enabled = false;
                model.radios.wifi_enabled = false;
                model.networks.clear();
            }
            Some(NetworkSample::Scanning) => {
                // The first search under way: none is due again yet, so the
                // capture holds that moment.
                model.networks.clear();
                model.scan_in_flight = true;
                model.scanned_at = Some(ui::time::now());
            }
            Some(NetworkSample::Known) => model.open_page(PAGE_KNOWN, t("net-known")),
            Some(NetworkSample::AllNetworks) => {
                model.open_page(PAGE_ALL, t("net-all-networks"));
            }
            Some(NetworkSample::NetworkPage) => {
                model.open_page(&wifi_page_key("Home_Network"), "Home_Network".into());
            }
            Some(NetworkSample::VpnList) => model.open_page(vpn::PAGE_VPN, t("net-vpn")),
            Some(NetworkSample::VpnProfile) => {
                model.open_page(vpn::PAGE_VPN, t("net-vpn"));
                model.open_page(&vpn::profile_page_key(SAMPLE_VPN_HOME), "Home".into());
                if let Ok(settings) = SampleNetwork::vpn_settings(SAMPLE_VPN_HOME) {
                    model.adopt_vpn_settings(SAMPLE_VPN_HOME, settings);
                }
            }
            Some(NetworkSample::VpnKillSwitch | NetworkSample::VpnBlocking) => {
                model.open_page(vpn::PAGE_VPN, t("net-vpn"));
                model.open_page(&vpn::profile_page_key(SAMPLE_VPN_HOME), "Home".into());
                if let Ok(settings) = SampleNetwork::vpn_settings(SAMPLE_VPN_HOME) {
                    model.adopt_vpn_settings(SAMPLE_VPN_HOME, settings);
                }
                model.sample_kill_switch(
                    SAMPLE_VPN_HOME,
                    wanted == Some(NetworkSample::VpnBlocking),
                );
            }
            Some(NetworkSample::PluginVpnProfile) => {
                model.open_page(vpn::PAGE_VPN, t("net-vpn"));
                model.open_page(&vpn::profile_page_key(SAMPLE_VPN_WORK), "Work".into());
                if let Ok(settings) = SampleNetwork::vpn_settings(SAMPLE_VPN_WORK) {
                    model.adopt_vpn_settings(SAMPLE_VPN_WORK, settings);
                }
            }
            Some(NetworkSample::VpnDevices) => {
                // Out through the desk PC, and the phone out through this
                // laptop. Everything goes through the desk PC, so the Home
                // tunnel, which carries everything too, is off.
                for vpn in &mut model.vpns {
                    vpn.active = false;
                }
                if let Some(status) = &mut model.vpn.devices.status {
                    status.client = Some(ClientStatus {
                        device: SAMPLE_DESK_PC.into(),
                        label: "Desk PC".into(),
                        state: ClientState::Connected {
                            since_unix_ms: 1_792_000_000_000,
                        },
                        ..ClientStatus::default()
                    });
                    status.exit_on = true;
                    status.exit_users = vec![ExitUser {
                        device: SAMPLE_PHONE.into(),
                        label: "Pixel".into(),
                        since_unix_ms: 1_792_000_000_000,
                    }];
                }
                model.open_page(vpn::PAGE_VPN, t("net-vpn"));
            }
            Some(NetworkSample::AddVpn) => {
                model.open_page(vpn::PAGE_VPN, t("net-vpn"));
                model.open_page(vpn::PAGE_ADD, t("net-vpn-add"));
            }
            Some(NetworkSample::List) | None => {}
        }
        model
    }

    /// A model over `service`, its first snapshot read at once.
    fn loaded_from(cx: PaneCx, service: Arc<dyn NetworkService>) -> Self {
        let mut model = Self::with_service(cx, service);
        let snapshot = model.service.snapshot(true);
        let _ = model.adopt_snapshot(snapshot);
        model
    }

    fn with_service(cx: PaneCx, service: Arc<dyn NetworkService>) -> Self {
        Self {
            listed: cx.radios().now(),
            pages_seen: cx.pages().revision(),
            cx,
            service,
            loaded: false,
            snapshot: None,
            networks: Vec::new(),
            wifi_enabled: false,
            wired: WiredInfo::default(),
            radios: os_actions::RadioState::default(),
            connectivity: ConnectivityState::Unknown,
            profiles: Vec::new(),
            profile_settings: HashMap::new(),
            wake_settings: HashMap::new(),
            hotspot_clients: Vec::new(),
            vpns: Vec::new(),
            link: None,
            plugins: Vec::new(),
            diagnostics: None,
            read_errors: NetworkReadErrors::default(),
            in_flight: 0,
            refreshes_pending: 0,
            connecting: Vec::new(),
            notice: None,
            asking: None,
            held_short: Rc::new(Cell::new(false)),
            held_all: Rc::new(Cell::new(false)),
            held_seen: false,
            short_list: RefCell::new(order::HeldList::default()),
            all_list: RefCell::new(order::HeldList::default()),
            filter: String::new(),
            enterprise_setup: ui::widget::DisclosureState::new(),
            scanned_at: None,
            scan_in_flight: false,
            hotspot_code_shown: false,
            revealed: None,
            code: None,
            wired_profile: LocalState::new(String::new()),
            seen_selection: String::new(),
            hotspot_name: LocalState::new(DEFAULT_HOTSPOT_NAME.into()),
            hotspot_password: Arc::new(Mutex::new(String::new())),
            hotspot_band: LocalState::new(band_shown("Automatic")),
            ipv4_forms: HashMap::new(),
            ipv6_forms: HashMap::new(),
            proxy_forms: HashMap::new(),
            identity_forms: HashMap::new(),
            enterprise: EnterpriseForm::new(),
            ca_picker: None,
            vpn: vpn::VpnState::default(),
            jobs: Vec::new(),
            mobile: None,
            following: std::rc::Weak::new(),
            changes_seen: 0,
            changed: false,
        }
    }

    /// Show the modem `mobile` describes on the first page and its own page.
    pub(crate) fn host_mobile(&mut self, mobile: &Shared<mobile::MobileModel>) {
        if !self
            .mobile
            .as_ref()
            .is_some_and(|hosted| Rc::ptr_eq(hosted, mobile))
        {
            self.mobile = Some(Rc::clone(mobile));
            self.changed = true;
        }
    }

    /// The radio state `NetworkManager` last reported, once it has.
    fn reported_radios(&self) -> Option<&os_actions::RadioState> {
        (self.loaded && self.read_errors.radios.is_none()).then_some(&self.radios)
    }

    /// Which radios Network can switch now.
    fn radios_present(&self) -> NetworkRadios {
        NetworkRadios::of(&self.cx, self.reported_radios())
    }

    /// Whether this device has no Wi-Fi radio.
    fn wifi_absent(&self) -> bool {
        self.radios_present().wifi == Some(false)
    }

    /// The networks in range, as last scanned.
    pub fn networks(&self) -> &[WifiNetworkInfo] {
        &self.networks
    }

    /// The page the pane shows: its key in the window's trail, `None` on the
    /// first page.
    pub(crate) fn page_key(&self) -> Option<String> {
        self.cx.pages().top(Category::Network).map(|page| page.key)
    }

    /// Drill into the page `key`, titled `title`.
    fn open_page(&mut self, key: &str, title: String) {
        self.cx.pages().open(Category::Network, key, title);
        self.changed = true;
    }

    /// Narrow the page of every network to `text`.
    fn set_filter(&mut self, text: &str) {
        if self.filter != text {
            text.clone_into(&mut self.filter);
            self.changed = true;
        }
    }

    /// Start reading a snapshot in the background; a newer read supersedes
    /// an older one still running.
    fn start_snapshot(&mut self) {
        let initial = !self.loaded;
        // A whole read covers every change the device reported before it.
        if let Some(changes) = self.device_changes() {
            self.changes_seen = changes;
        }
        self.start_read(move |service| service.snapshot(initial));
    }

    /// The device's change count, while it is followed.
    fn device_changes(&self) -> Option<u64> {
        self.following.upgrade()?.changes()
    }

    /// Read the device's status again once it reported a change: a VPN
    /// brought up from quick settings, a cable pulled out, airplane mode
    /// turned on elsewhere shows while the page is open. One read at a
    /// time; changes during it are read after it, and none while a change
    /// of the pane's own runs (its landing reads the device anyway).
    fn read_reported_changes(&mut self) {
        let Some(changes) = self.device_changes() else {
            return;
        };
        if changes == self.changes_seen
            || !self.loaded
            || self.snapshot.is_some()
            || self.in_flight > 0
        {
            return;
        }
        self.changes_seen = changes;
        self.start_read(|service| service.status());
    }

    /// Follow the device while a pane shows the model.
    fn follow_device(&mut self) -> Rc<crate::watch::Counted> {
        if let Some(held) = self.following.upgrade() {
            return held;
        }
        let service = Arc::clone(&self.service);
        let started = crate::watch::shared(&mut self.following, move || {
            crate::watch::Counted::start(|changed| service.follow(changed))
        });
        // Nothing was followed while no pane showed the model: a model
        // already read reads the status again once; a first read on its way
        // covers it.
        self.changes_seen = if self.loaded { u64::MAX } else { 0 };
        started
    }

    /// Read with `read` in the background; a newer read supersedes an older
    /// one still running.
    fn start_read(
        &mut self,
        read: impl FnOnce(&dyn NetworkService) -> NetworkSnapshot + Send + 'static,
    ) {
        let (sender, receiver) = crate::channel::channel();
        let service = Arc::clone(&self.service);
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-network-read",
            Box::new(move || {
                if sender.send(read(service.as_ref())).is_ok() {
                    wake.wake();
                }
            }),
        );
        match started {
            Ok(()) => self.snapshot = Some(receiver),
            Err(error) => self.notice = Some(format!("{}: {error}", t("net-notice-unavailable"))),
        }
        self.changed = true;
    }

    /// Run `work` on a worker of the pane's own; what it lands is taken in
    /// when it arrives.
    fn start_job(&mut self, name: &str, work: Box<dyn FnOnce() -> Landed + Send>) {
        let (sender, receiver) = crate::channel::channel();
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            name,
            Box::new(move || {
                if sender.send(work()).is_ok() {
                    wake.wake();
                }
            }),
        );
        match started {
            Ok(()) => self.jobs.push(receiver),
            Err(error) => self.notice = Some(format!("{}: {error}", t("net-notice-unavailable"))),
        }
        self.changed = true;
    }

    /// Adopt a snapshot; what refreshing reports.
    fn adopt_snapshot(&mut self, snapshot: NetworkSnapshot) -> Result<String, String> {
        self.changed = true;
        // A whole snapshot says which Wi-Fi link is in use, also none.
        let whole = snapshot.wifi_enabled.is_some();
        let outcome = match snapshot.networks {
            Ok(networks) => {
                self.networks = networks;
                self.read_errors.networks = None;
                Ok(t("net-notice-refreshed"))
            }
            Err(error) => {
                warn!(target: "tilo::settings", op = "list_wifi_networks", %error, "network scan failed");
                // The last list is no longer what the radio sees.
                self.networks.clear();
                self.read_errors.networks = Some(error.clone());
                Err(t_args("net-notice-refresh-err", &[("error", error.into())]))
            }
        };
        if self.loaded && self.in_flight > 0 {
            // Read before a running change landed; the snapshot taken once
            // the changes land reports the device.
            return outcome;
        }
        if let Some(enabled) = adopt(snapshot.wifi_enabled, &mut self.read_errors.wifi_enabled) {
            self.wifi_enabled = enabled;
        }
        if let Some(wired) = adopt(snapshot.wired, &mut self.read_errors.wired) {
            self.wired = wired;
        }
        if let Some(radios) = adopt(snapshot.radios, &mut self.read_errors.radios) {
            self.radios = radios;
        }
        if let Some(connectivity) = adopt(snapshot.connectivity, &mut self.read_errors.connectivity)
        {
            self.connectivity = connectivity;
        }
        if let Some(clients) = adopt(
            snapshot.hotspot_clients,
            &mut self.read_errors.hotspot_clients,
        ) {
            self.hotspot_clients = clients;
        }
        if let Some(profiles) = adopt(snapshot.profiles, &mut self.read_errors.profiles) {
            self.profiles = profiles;
            self.profile_settings = snapshot.profile_settings;
            self.wake_settings = snapshot.wake_settings;
            self.fill_forms();
        }
        if let Some(vpns) = adopt(snapshot.vpns, &mut self.read_errors.vpns) {
            self.vpns = vpns;
        }
        if whole || snapshot.link.is_some() {
            self.link = snapshot.link;
        }
        if let Some(plugins) = snapshot.plugins {
            self.plugins = plugins;
        }
        if let DeviceVpnPart::Read(status) = snapshot.device_vpn {
            self.vpn.devices.status = status;
        }
        self.loaded = true;
        outcome
    }

    /// Give each saved profile its forms, filled from what `NetworkManager`
    /// reports, and keep the wired selector on a profile that still exists.
    fn fill_forms(&mut self) {
        for profile in &self.profiles {
            let Some(settings) = self.profile_settings.get(&profile.uuid) else {
                continue;
            };
            if let Some(ipv4) = &settings.ipv4 {
                self.ipv4_forms
                    .entry(profile.uuid.clone())
                    .or_insert_with(|| Ipv4Form {
                        automatic: LocalState::new(ipv4.automatic),
                        addresses: LocalState::new(ipv4.addresses.join(", ")),
                        gateway: LocalState::new(ipv4.gateway.clone().unwrap_or_default()),
                        dns: LocalState::new(ipv4.dns.join(", ")),
                    });
            }
            if let Some(ipv6) = &settings.ipv6 {
                self.ipv6_forms
                    .entry(profile.uuid.clone())
                    .or_insert_with(|| Ipv6Form {
                        method: LocalState::new(ipv6_shown(ipv6_id(ipv6.method))),
                        addresses: LocalState::new(ipv6.addresses.join(", ")),
                        gateway: LocalState::new(ipv6.gateway.clone().unwrap_or_default()),
                    });
            }
            if let Some(proxy) = &settings.proxy {
                let (mode, address) = match (&proxy.automatic_url, &proxy.manual) {
                    (_, Some(server)) => ("Manual", server.clone()),
                    (Some(url), None) => ("Automatic", url.clone()),
                    (None, None) => ("None", String::new()),
                };
                self.proxy_forms
                    .entry(profile.uuid.clone())
                    .or_insert_with(|| ProxyForm {
                        mode: LocalState::new(proxy_shown(mode)),
                        address: LocalState::new(address),
                    });
            }
            if let Some(identity) = &settings.enterprise {
                self.identity_forms
                    .entry(profile.uuid.clone())
                    .or_insert_with(|| IdentityForm {
                        identity: LocalState::new(identity.identity.clone()),
                        ca: LocalState::new(identity.ca_certificate.clone()),
                    });
            }
        }
        let current = self.wired_profile.get();
        if !self
            .profiles
            .iter()
            .any(|p| p.kind == "802-3-ethernet" && profile_label(p) == current)
        {
            self.wired_profile.set(
                self.profiles
                    .iter()
                    .find(|p| p.kind == "802-3-ethernet")
                    .map(profile_label)
                    .unwrap_or_default(),
            );
        }
    }

    /// The saved profile `id` (its id or its name).
    fn profile(&self, id: &str) -> Result<SavedNetworkProfile, String> {
        self.profiles
            .iter()
            .find(|p| p.uuid == id || p.name == id)
            .cloned()
            .ok_or_else(|| format!("No saved network is called {id}."))
    }

    /// The saved Wi-Fi profile `id` names, and its settings as read.
    fn wifi_profile(
        &self,
        id: &str,
    ) -> Result<(SavedNetworkProfile, os_actions::ConnectionProfileSettings), String> {
        let profile = self.profile(id)?;
        let settings = self
            .profile_settings
            .get(&profile.uuid)
            .cloned()
            .ok_or_else(|| t("net-notice-policy-unread"))?;
        Ok((profile, settings))
    }

    /// The saved Wi-Fi networks, as this device knows them.
    fn saved_wifi(&self) -> impl Iterator<Item = &SavedNetworkProfile> {
        self.profiles
            .iter()
            .filter(|profile| profile.kind == "802-11-wireless")
    }

    /// The saved profile a network joins with: the one named for it, as
    /// `NetworkManager` names a profile it makes when joining.
    fn saved_profile_of(&self, ssid: &str) -> Option<&SavedNetworkProfile> {
        self.saved_wifi().find(|profile| profile.name == ssid)
    }

    /// Whether this device knows `network`: a saved profile, or
    /// `NetworkManager` says so.
    fn knows(&self, network: &WifiNetworkInfo) -> bool {
        network.known || self.saved_profile_of(&network.ssid).is_some()
    }

    /// Every Wi-Fi network the pane has a row for: in range or saved.
    fn wifi_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .networks
            .iter()
            .map(|network| network.ssid.clone())
            .collect();
        for profile in self.saved_wifi() {
            if !names.contains(&profile.name) {
                names.push(profile.name.clone());
            }
        }
        names
    }

    /// The network in range called `ssid`.
    fn in_range(&self, ssid: &str) -> Option<&WifiNetworkInfo> {
        self.networks.iter().find(|network| network.ssid == ssid)
    }

    /// The published row of the Wi-Fi network `ssid`: what can be done with
    /// it, in range or saved.
    fn wifi_live(&self, ssid: &str) -> LiveRow {
        let offer = |effect: NetworkEffect| SettingsEffect::from(effect).bound();
        let mut row = LiveRow::new(wifi_row_id(ssid), "network.wifi", ssid.to_owned()).operation(
            offer(NetworkEffect::OpenNetworkPage {
                ssid: ssid.to_owned(),
            }),
        );
        if let Some(network) = self.in_range(ssid) {
            if !network.connected {
                row = row.operation(offer(NetworkEffect::ConnectNetwork {
                    ssid: ssid.to_owned(),
                    password: None,
                }));
            }
            row = row.operation(offer(NetworkEffect::ShowNetworkCode {
                ssid: ssid.to_owned(),
            }));
        }
        let Some(profile) = self.saved_profile_of(ssid) else {
            return row;
        };
        let uuid = profile.uuid.clone();
        let settings = self.profile_settings.get(&uuid);
        // A setting is offered with its value to give; the profile is the
        // row's own.
        let asking = |effect: NetworkEffect, asking: &[&str]| {
            SettingsEffect::from(effect).bound_asking(asking)
        };
        row = row
            .operation(asking(
                NetworkEffect::SetAutoconnect {
                    profile: uuid.clone(),
                    on: !profile.autoconnect,
                },
                &["on"],
            ))
            .operation(offer(NetworkEffect::ForgetNetwork {
                profile: uuid.clone(),
            }))
            .operation(offer(NetworkEffect::ShareNetwork {
                profile: uuid.clone(),
            }))
            .operation(offer(NetworkEffect::ShowNetworkPassword {
                profile: uuid.clone(),
            }))
            .operation(offer(NetworkEffect::SetNetworkPassword {
                profile: uuid.clone(),
                password: None,
            }));
        if let Some(settings) = settings {
            row = with_profile_settings(row, &uuid, settings);
        }
        self.with_wake(row, profile)
    }

    /// The live row a saved profile's settings act on: a Wi-Fi network's
    /// row, or the profile's own (a wired connection's).
    pub(crate) fn profile_live(&self, profile: &SavedNetworkProfile) -> LiveRow {
        if profile.kind == "802-11-wireless" {
            return self.wifi_live(&profile.name);
        }
        let row = LiveRow::new(
            format!("settings.network-profile:{}", profile.uuid),
            "settings.network-profile",
            profile.name.clone(),
        )
        .operation(
            SettingsEffect::from(NetworkEffect::SetAutoconnect {
                profile: profile.uuid.clone(),
                on: !profile.autoconnect,
            })
            .bound_asking(&["on"]),
        )
        .operation(
            SettingsEffect::from(NetworkEffect::ForgetNetwork {
                profile: profile.uuid.clone(),
            })
            .bound(),
        );
        self.with_wake(row, profile)
    }

    /// `row`, saying how `profile` wakes this machine and offering to
    /// change it where it can.
    fn with_wake(&self, mut row: LiveRow, profile: &SavedNetworkProfile) -> LiveRow {
        if let Some(Ok(wake)) = self.wake_settings.get(&profile.uuid) {
            row = row.presentation(ui::tilo_actions::ResourcePresentation {
                detail: None,
                badge: None,
                preview: Some(t(wake_description(wake))),
            });
            if wake.configured || wake.supported == Some(true) {
                row = row.operation(
                    SettingsEffect::from(NetworkEffect::SetNetworkWake {
                        profile: profile.uuid.clone(),
                        on: !wake.configured,
                    })
                    .bound_asking(&["on"]),
                );
            }
        }
        row
    }

    /// Start `change` on a worker, showing `notice` meanwhile.
    fn change(&mut self, change: NetworkChange, notice: Option<String>) -> Begun {
        if notice.is_some() {
            self.notice = notice;
        }
        self.in_flight += 1;
        let service = Arc::clone(&self.service);
        Begun::work(move || {
            let result = service.change(&change);
            Landed::Changed(change, result)
        })
    }

    /// A change to the policy of the saved Wi-Fi network `profile`.
    fn policy_change(
        &mut self,
        profile: &SavedNetworkProfile,
        patch: os_actions::ConnectionPolicyPatch,
    ) -> Begun {
        self.change(
            NetworkChange::Policy {
                uuid: profile.uuid.clone(),
                name: profile.name.clone(),
                patch,
            },
            None,
        )
    }

    /// Begin `effect`.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per effect; the longer ones call out"
    )]
    fn begin(&mut self, effect: NetworkEffect) -> Result<Begun, String> {
        self.changed = true;
        let service = Arc::clone(&self.service);
        Ok(match effect {
            NetworkEffect::InspectNetworkState => {
                return Err("Network inspection is a read-only resource operation.".into());
            }
            NetworkEffect::RefreshNetworks => {
                self.refreshes_pending += 1;
                Begun::work(move || {
                    let scan = service.scan();
                    service.recheck_connectivity();
                    Landed::Refreshed {
                        snapshot: Box::new(service.snapshot(false)),
                        scan,
                    }
                })
            }
            NetworkEffect::SetWifi { on } => {
                if let Some(error) = &self.read_errors.wifi_enabled {
                    return Err(t_args(
                        "net-status-error",
                        &[("error", error.as_str().into())],
                    ));
                }
                self.wifi_enabled = on;
                self.change(NetworkChange::Wifi(on), None)
            }
            NetworkEffect::SetWired { on } => {
                if self.wired.device.is_none() {
                    return Err("Wired networking is unavailable on this device.".into());
                }
                self.wired.connected = on;
                self.change(NetworkChange::Wired(on), None)
            }
            NetworkEffect::SetAirplaneMode { on } => self.switch(Radio::Airplane, on)?,
            NetworkEffect::SetHotspot { on } => self.switch(Radio::Hotspot, on)?,
            NetworkEffect::ConnectNetwork { ssid, password } => {
                self.connect(ssid, password.map(Zeroizing::new))?
            }
            NetworkEffect::CancelConnect { ssid } => {
                if self.asking_to_join(&ssid) {
                    self.close_prompt();
                }
                Begun::Done
            }
            NetworkEffect::OpenNetworkPage { ssid } => {
                if self.in_range(&ssid).is_none() && self.saved_profile_of(&ssid).is_none() {
                    return Err(format!("{ssid} is neither in range nor saved."));
                }
                self.open_page(&wifi_page_key(&ssid), ssid);
                Begun::Done
            }
            NetworkEffect::ShowAllNetworks => {
                self.open_page(PAGE_ALL, t("net-all-networks"));
                Begun::Done
            }
            NetworkEffect::ShowKnownNetworks => {
                self.open_page(PAGE_KNOWN, t("net-known"));
                Begun::Done
            }
            NetworkEffect::ShowMobile => {
                self.open_page(PAGE_MOBILE, t("net-mobile"));
                Begun::Done
            }
            NetworkEffect::ShowHiddenNetwork => {
                self.ask_hidden();
                Begun::Done
            }
            NetworkEffect::JoinHiddenNetwork { ssid, password } => {
                if let Some(ssid) = ssid.and_then(non_blank) {
                    let password = password
                        .map(Zeroizing::new)
                        .filter(|password| !password.is_empty());
                    self.join_hidden(ssid, password)
                } else {
                    self.ask_hidden();
                    Begun::Done
                }
            }
            NetworkEffect::SetAutoconnect { profile, on } => {
                let profile = self.profile(&profile)?;
                if let Some(saved) = self.profiles.iter_mut().find(|p| p.uuid == profile.uuid) {
                    saved.autoconnect = on;
                }
                self.change(
                    NetworkChange::Autoconnect {
                        uuid: profile.uuid,
                        name: profile.name,
                        on,
                    },
                    None,
                )
            }
            NetworkEffect::SetPriority { profile, priority } => {
                let (profile, _) = self.wifi_profile(&profile)?;
                let priority = priority.clamp(-999, 999);
                if let Some(settings) = self.profile_settings.get_mut(&profile.uuid) {
                    settings.priority = Some(priority);
                }
                self.policy_change(
                    &profile,
                    os_actions::ConnectionPolicyPatch {
                        priority: Some(priority),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                )
            }
            NetworkEffect::SetMeteredPolicy { profile, policy } => {
                let (profile, _) = self.wifi_profile(&profile)?;
                let metered = metered_from_label(&policy)
                    .ok_or_else(|| format!("{policy} is not a metered setting."))?;
                if let Some(settings) = self.profile_settings.get_mut(&profile.uuid) {
                    settings.metered = Some(metered);
                }
                self.policy_change(
                    &profile,
                    os_actions::ConnectionPolicyPatch {
                        metered: Some(metered),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                )
            }
            NetworkEffect::SetMacAddress { profile, policy } => {
                let (profile, _) = self.wifi_profile(&profile)?;
                let mac = mac_from_label(&policy)
                    .ok_or_else(|| format!("{policy} is not a hardware address setting."))?;
                if let Some(settings) = self.profile_settings.get_mut(&profile.uuid) {
                    settings.mac = Some(mac);
                }
                self.policy_change(
                    &profile,
                    os_actions::ConnectionPolicyPatch {
                        mac: Some(mac),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                )
            }
            NetworkEffect::SetHiddenNetwork { profile, on } => {
                let (profile, _) = self.wifi_profile(&profile)?;
                if let Some(settings) = self.profile_settings.get_mut(&profile.uuid) {
                    settings.hidden = Some(on);
                }
                self.policy_change(
                    &profile,
                    os_actions::ConnectionPolicyPatch {
                        hidden: Some(on),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                )
            }
            NetworkEffect::SetIpv4 {
                profile,
                automatic,
                addresses,
                gateway,
                dns,
            } => {
                let profile = self.profile(&profile)?;
                if self
                    .profile_settings
                    .get(&profile.uuid)
                    .is_none_or(|s| s.ipv4.is_none())
                {
                    return Err(
                        "IP settings were not applied because the profile could not \
                                be read safely."
                            .into(),
                    );
                }
                let form = self.ipv4_forms.get(&profile.uuid);
                let field = |given: Option<String>, held: fn(&Ipv4Form) -> &LocalState<String>| {
                    given.or_else(|| form.map(|form| held(form).get()))
                };
                let config = IpConfiguration {
                    automatic: automatic
                        .or_else(|| form.map(|form| form.automatic.get()))
                        .unwrap_or(false),
                    addresses: list_of(
                        &field(addresses, |form| &form.addresses).unwrap_or_default(),
                    ),
                    gateway: field(gateway, |form| &form.gateway).and_then(non_blank),
                    dns: list_of(&field(dns, |form| &form.dns).unwrap_or_default()),
                };
                self.change(
                    NetworkChange::Ipv4 {
                        uuid: profile.uuid,
                        name: profile.name.clone(),
                        config,
                    },
                    Some(t_args(
                        "net-notice-applying",
                        &[("name", profile.name.as_str().into())],
                    )),
                )
            }
            NetworkEffect::SetIpv6 {
                profile,
                method,
                addresses,
                gateway,
            } => {
                let (profile, settings) = self.wifi_profile(&profile)?;
                let current = settings.ipv6.ok_or_else(|| t("net-notice-policy-unread"))?;
                let form = self.ipv6_forms.get(&profile.uuid);
                let method = method
                    .or_else(|| form.map(|form| form.method.get()))
                    .and_then(|label| ipv6_from_label(&label))
                    .unwrap_or(current.method);
                let addresses = addresses
                    .or_else(|| form.map(|form| form.addresses.get()))
                    .map_or(current.addresses, |list| list_of(&list));
                let gateway = gateway
                    .or_else(|| form.map(|form| form.gateway.get()))
                    .map_or(current.gateway, non_blank);
                self.change(
                    NetworkChange::Ipv6 {
                        uuid: profile.uuid.clone(),
                        name: profile.name.clone(),
                        config: Ipv6Configuration {
                            method,
                            addresses,
                            gateway,
                            dns: current.dns,
                        },
                    },
                    Some(t_args(
                        "net-notice-applying",
                        &[("name", profile.name.as_str().into())],
                    )),
                )
            }
            NetworkEffect::SetDns { profile, servers } => {
                let profile = self.profile(&profile)?;
                let servers = list_of(&servers);
                if let Some(ipv4) = self
                    .profile_settings
                    .get_mut(&profile.uuid)
                    .and_then(|settings| settings.ipv4.as_mut())
                {
                    ipv4.dns.clone_from(&servers);
                }
                self.change(
                    NetworkChange::Dns {
                        uuid: profile.uuid,
                        name: profile.name,
                        servers,
                    },
                    None,
                )
            }
            NetworkEffect::SetProxy {
                profile,
                mode,
                address,
            } => {
                let (profile, settings) = self.wifi_profile(&profile)?;
                let current = settings
                    .proxy
                    .ok_or_else(|| t("net-notice-policy-unread"))?;
                let form = self.proxy_forms.get(&profile.uuid);
                let mode = mode
                    .or_else(|| form.map(|form| form.mode.get()))
                    .and_then(|label| proxy_from_label(&label))
                    .unwrap_or("None");
                let address = address
                    .or_else(|| form.map(|form| form.address.get()))
                    .and_then(non_blank)
                    .map(|address| address.trim().to_string());
                let proxy = os_actions::ProxyConfiguration {
                    automatic_url: (mode == "Automatic").then(|| address.clone()).flatten(),
                    manual: (mode == "Manual").then_some(address).flatten(),
                    browser_only: current.browser_only,
                };
                if mode != "None" && proxy.automatic_url.is_none() && proxy.manual.is_none() {
                    return Err(t("net-proxy-needs-address"));
                }
                self.policy_change(
                    &profile,
                    os_actions::ConnectionPolicyPatch {
                        proxy: Some(proxy),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                )
            }
            NetworkEffect::SetEnterpriseIdentity {
                profile,
                identity,
                ca_certificate,
            } => {
                let (profile, settings) = self.wifi_profile(&profile)?;
                let current = settings
                    .enterprise
                    .ok_or_else(|| format!("{} is not a work or school network.", profile.name))?;
                let form = self.identity_forms.get(&profile.uuid);
                let identity = EnterpriseIdentity {
                    identity: identity
                        .or_else(|| form.map(|form| form.identity.get()))
                        .unwrap_or(current.identity),
                    ca_certificate: ca_certificate
                        .or_else(|| form.map(|form| form.ca.get()))
                        .unwrap_or(current.ca_certificate),
                };
                self.change(
                    NetworkChange::Identity {
                        uuid: profile.uuid,
                        name: profile.name,
                        identity,
                    },
                    None,
                )
            }
            NetworkEffect::ShowNetworkPassword { profile } => {
                let profile = self.profile(&profile)?;
                if self
                    .revealed
                    .as_ref()
                    .is_some_and(|shown| shown.of == profile.uuid)
                {
                    self.revealed = None;
                    return Ok(Begun::Done);
                }
                let uuid = profile.uuid;
                Begun::work(move || Landed::Password {
                    result: service.wifi_password(&uuid),
                    uuid,
                    purpose: PasswordFor::Reveal,
                })
            }
            NetworkEffect::SetNetworkPassword { profile, password } => {
                let profile = self.profile(&profile)?;
                let Some(password) = password.map(Zeroizing::new) else {
                    self.ask_new_password(profile.uuid, profile.name);
                    return Ok(Begun::Done);
                };
                if password.is_empty() {
                    return Err(t("net-password-needed"));
                }
                self.revealed = None;
                self.change(
                    NetworkChange::Password {
                        uuid: profile.uuid,
                        name: profile.name,
                        password,
                    },
                    None,
                )
            }
            NetworkEffect::ShowNetworkCode { ssid } => {
                if self.code.as_ref().is_some_and(|shown| shown.of == ssid) {
                    self.code = None;
                    return Ok(Begun::Done);
                }
                let network = self
                    .in_range(&ssid)
                    .cloned()
                    .ok_or_else(|| format!("{ssid} is not in range."))?;
                let hidden = self
                    .saved_profile_of(&ssid)
                    .and_then(|profile| self.profile_settings.get(&profile.uuid))
                    .and_then(|settings| settings.hidden)
                    .unwrap_or(false);
                if !network.secure {
                    self.code = Some(Shown {
                        of: ssid.clone(),
                        secret: os_actions::network_settings::wifi_qr_payload(&ssid, None, hidden),
                    });
                    return Ok(Begun::Done);
                }
                let profile = self
                    .saved_profile_of(&ssid)
                    .cloned()
                    .ok_or_else(|| t("net-share-needs-saved"))?;
                let uuid = profile.uuid;
                Begun::work(move || Landed::Password {
                    result: service.wifi_password(&uuid),
                    uuid,
                    purpose: PasswordFor::Code { ssid, hidden },
                })
            }
            NetworkEffect::SetNetworkWake { profile, on } => {
                let profile = self.profile(&profile)?;
                self.change(
                    NetworkChange::Wake {
                        uuid: profile.uuid,
                        on,
                    },
                    None,
                )
            }
            NetworkEffect::ForgetNetwork { profile } => {
                let profile = self.profile(&profile)?;
                self.change(
                    NetworkChange::Forget {
                        uuid: profile.uuid,
                        name: profile.name,
                    },
                    None,
                )
            }
            NetworkEffect::ShareNetwork { profile } => {
                let profile = self.profile(&profile)?;
                if profile.kind != "802-11-wireless" {
                    return Err(format!("{} is not a Wi-Fi network.", profile.name));
                }
                self.notice = Some(t_args(
                    "net-notice-sealing",
                    &[("name", profile.name.as_str().into())],
                ));
                Begun::work(move || {
                    Landed::Shared(
                        service
                            .continuity(WifiContinuityCommand::ShareSavedProfile {
                                profile_uuid: profile.uuid.clone(),
                            })
                            .and_then(|reply| match reply {
                                WifiContinuityReply::Shared { ssid } => Ok(ssid),
                                WifiContinuityReply::Rejected { reason } => Err(reason),
                                _ => Err("the Wi-Fi continuity service returned an invalid \
                                          profile reply"
                                    .into()),
                            }),
                    )
                })
            }
            NetworkEffect::RunDiagnostics => {
                Begun::work(move || Landed::Diagnosed(service.diagnostics()))
            }
            NetworkEffect::OpenNetworkSignIn => {
                // The opener is resolved here: a scoped host is only visible
                // on this thread.
                let opener = ui::host::current().opener;
                Begun::work(move || {
                    Landed::SignedIn(service.sign_in_page().and_then(|uri| {
                        ui::portal::open_uri_with(&*opener, &uri).map_err(|error| {
                            format!("could not open browser through the desktop portal: {error}")
                        })
                    }))
                })
            }
            NetworkEffect::StartHotspot {
                name,
                password,
                band,
            } => {
                let config = self.hotspot(name, password, band)?;
                self.change(
                    NetworkChange::Hotspot(config),
                    Some(t("net-notice-hotspot-starting")),
                )
            }
            NetworkEffect::ShowHotspotCode => {
                if !self.hotspot_code_shown {
                    self.hotspot(None, None, None)?;
                }
                self.hotspot_code_shown = !self.hotspot_code_shown;
                Begun::Done
            }
            NetworkEffect::ShowHotspotSetup => {
                self.open_page(PAGE_HOTSPOT, t("net-hotspot"));
                Begun::Done
            }
            NetworkEffect::AskHotspotPassword => {
                self.ask_hotspot_password();
                Begun::Done
            }
            NetworkEffect::ShowAdvancedNetworkSettings => {
                self.open_page(PAGE_ADVANCED, t("net-advanced"));
                Begun::Done
            }
            NetworkEffect::ShowEnterpriseSetup => {
                self.enterprise_setup
                    .set_open(!self.enterprise_setup.is_open());
                Begun::Done
            }
            NetworkEffect::ChooseCaCertificate => {
                if self.ca_picker.is_none() {
                    self.ca_picker = Some(
                        ui::portal::PickRequest::open(&t("net-choose-ca-title"), &t("net-choose"))
                            .thread_name("settings-ca-picker")
                            .send(),
                    );
                }
                Begun::Done
            }
            NetworkEffect::ConnectEnterpriseNetwork {
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
                let given = EnterpriseGiven {
                    profile,
                    ssid,
                    identity,
                    anonymous_identity,
                    ca_certificate,
                    method,
                    inner_auth,
                    client_certificate,
                    private_key,
                };
                if let Some(password) = password.map(Zeroizing::new) {
                    self.connect_enterprise(given, password)
                } else {
                    let config = self.enterprise_config(given.clone());
                    self.ask_enterprise(given, &config.ssid, &config.identity);
                    Begun::Done
                }
            }
        })
    }

    /// Switch `radio`, showing the requested state at once.
    fn switch(&mut self, radio: Radio, on: bool) -> Result<Begun, String> {
        if let Some(error) = &self.read_errors.radios {
            return Err(t_args(
                "net-radios-error",
                &[("error", error.as_str().into())],
            ));
        }
        match radio {
            Radio::Airplane => self.radios.airplane_mode = on,
            Radio::Hotspot => self.radios.hotspot_active = on,
        }
        Ok(self.change(NetworkChange::Radio(radio, on), None))
    }

    /// Join `ssid`: with `password`, else with what its open prompt holds,
    /// else (a secure network) with a password the person's devices share.
    fn connect(
        &mut self,
        ssid: String,
        password: Option<Zeroizing<String>>,
    ) -> Result<Begun, String> {
        let network = self
            .in_range(&ssid)
            .cloned()
            .ok_or_else(|| format!("{ssid} is not in range."))?;
        if self.connecting.contains(&ssid) {
            return Err(format!("Already connecting to {ssid}."));
        }
        let password = password.filter(|password| !password.is_empty());
        // A known network joins with what this device saved for it.
        let saved = self.knows(&network) && password.is_none();
        let lookup_shared = network.secure && password.is_none() && !saved;
        self.connecting.push(ssid.clone());
        self.notice = Some(t_args(
            "net-notice-connecting",
            &[("ssid", ssid.as_str().into())],
        ));
        let service = Arc::clone(&self.service);
        let portable = portable_security(network.security);
        Ok(Begun::work(move || {
            let result = if lookup_shared {
                join_with_shared_password(service.as_ref(), &ssid, portable)
            } else {
                join(service.as_ref(), &ssid, portable, password)
            };
            Landed::Connected { ssid, result }
        }))
    }

    /// The hotspot from what is given, else what its form holds.
    fn hotspot(
        &self,
        name: Option<String>,
        password: Option<String>,
        band: Option<String>,
    ) -> Result<HotspotConfiguration, String> {
        let ssid = name
            .or_else(|| non_blank(self.hotspot_name.get()))
            .unwrap_or_else(|| DEFAULT_HOTSPOT_NAME.into());
        let password = match password {
            Some(password) => password,
            None => self
                .hotspot_password
                .lock()
                .map_err(|_| "hotspot password buffer is unavailable")?
                .clone(),
        };
        let band = band_from_label(&band.unwrap_or_else(|| self.hotspot_band.get()));
        let config = HotspotConfiguration {
            ssid,
            password,
            band,
        };
        config.validate()?;
        Ok(config)
    }

    /// Save the enterprise network `given` asks for and connect with
    /// `password`.
    fn connect_enterprise(&mut self, given: EnterpriseGiven, password: Zeroizing<String>) -> Begun {
        let config = self.enterprise_config(given);
        self.change(
            NetworkChange::Enterprise { config, password },
            Some(t("net-notice-enterprise-connecting")),
        )
    }

    /// Join `ssid`, a network that does not broadcast its name.
    fn join_hidden(&mut self, ssid: String, password: Option<Zeroizing<String>>) -> Begun {
        let notice = t_args("net-notice-connecting", &[("ssid", ssid.as_str().into())]);
        self.connecting.push(ssid.clone());
        self.change(NetworkChange::JoinHidden { ssid, password }, Some(notice))
    }

    /// The enterprise network `given` asks for, completed from the setup
    /// form.
    fn enterprise_config(&self, given: EnterpriseGiven) -> EnterpriseWifiConfiguration {
        let EnterpriseGiven {
            profile,
            ssid,
            identity,
            anonymous_identity,
            ca_certificate,
            method,
            inner_auth,
            client_certificate,
            private_key,
        } = given;
        let form = &self.enterprise;
        EnterpriseWifiConfiguration {
            profile_name: profile.unwrap_or_else(|| form.profile.get()),
            ssid: ssid.unwrap_or_else(|| form.ssid.get()),
            identity: identity.unwrap_or_else(|| form.identity.get()),
            anonymous_identity: anonymous_identity
                .or_else(|| Some(form.anonymous.get()))
                .and_then(non_blank),
            ca_certificate: ca_certificate.unwrap_or_else(|| form.ca.get()),
            method: match method.unwrap_or_else(|| form.method.get()).as_str() {
                "TTLS" => os_actions::EnterpriseEapMethod::Ttls,
                "TLS" => os_actions::EnterpriseEapMethod::Tls,
                _ => os_actions::EnterpriseEapMethod::Peap,
            },
            phase2_auth: Some(
                inner_auth
                    .unwrap_or_else(|| form.inner_auth.get())
                    .to_ascii_lowercase(),
            ),
            client_certificate: client_certificate
                .or_else(|| Some(form.client_certificate.get()))
                .and_then(non_blank),
            private_key: private_key
                .or_else(|| Some(form.private_key.get()))
                .and_then(non_blank),
        }
    }

    /// A VPN turned on: one carrying all traffic leaves the route through
    /// a device.
    fn vpn_on(
        &mut self,
        change: NetworkChange,
        result: Result<(), String>,
        full_tunnel: bool,
    ) -> EffectAnswer {
        if full_tunnel && result.is_ok() {
            self.leave_device_route();
        }
        self.land(Landed::Changed(change, result))
    }

    /// The VPNs carrying all traffic turned off for a route through a
    /// device: read back what stands.
    fn tunnels_left(&mut self, result: Result<(), String>) -> Result<String, String> {
        self.in_flight = self.in_flight.saturating_sub(1);
        if self.in_flight == 0 {
            self.start_snapshot();
        }
        result.map(|()| String::new())
    }

    /// A saved network's password, read to show it or its code.
    fn password_read(
        &mut self,
        uuid: String,
        purpose: PasswordFor,
        result: Result<Zeroizing<String>, String>,
    ) -> Result<String, String> {
        match (purpose, result) {
            (PasswordFor::Reveal, Ok(secret)) => {
                self.revealed = Some(Shown { of: uuid, secret });
                Ok(String::new())
            }
            (PasswordFor::Code { ssid, hidden }, Ok(secret)) => {
                self.code = Some(Shown {
                    secret: os_actions::network_settings::wifi_qr_payload(
                        &ssid,
                        Some(&secret),
                        hidden,
                    ),
                    of: ssid,
                });
                Ok(String::new())
            }
            (_, Err(error)) => Err(t_args(
                "net-notice-password-err",
                &[("error", error.into())],
            )),
        }
    }

    /// Take in work that landed; what the effect answers.
    fn land(&mut self, landed: Landed) -> EffectAnswer {
        self.changed = true;
        let reported = match landed {
            Landed::Refreshed { snapshot, scan } => {
                self.refreshes_pending = self.refreshes_pending.saturating_sub(1);
                let adopted = self.adopt_snapshot(*snapshot);
                match scan {
                    // The list and status still read; only the fresh look
                    // failed. A radio that is off cannot scan, and says so.
                    Err(error) if adopted.is_ok() && self.wifi_enabled => {
                        warn!(target: "tilo::settings", op = "scan_wifi", %error, "Wi-Fi scan failed");
                        Err(t_args("net-notice-refresh-err", &[("error", error.into())]))
                    }
                    _ => adopted,
                }
            }
            Landed::Changed(change, result) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                let reported = self.report(&change, result);
                if self.in_flight == 0 {
                    self.start_snapshot();
                }
                self.settle_prompt(&reported);
                reported
            }
            Landed::VpnOn {
                change,
                result,
                full_tunnel,
            } => return self.vpn_on(change, result, full_tunnel),
            Landed::TunnelsLeft(result) => self.tunnels_left(result),
            Landed::Connected { ssid, result } => {
                let reported = self.joined(&ssid, result);
                self.settle_prompt(&reported);
                reported
            }
            Landed::Shared(result) => match result {
                Ok(ssid) => Ok(t_args(
                    "net-notice-sealed-profile",
                    &[("ssid", ssid.into())],
                )),
                Err(error) => Err(t_args(
                    "net-notice-configure-err",
                    &[
                        ("subject", t("net-subject-saved-wifi").into()),
                        ("error", error.into()),
                    ],
                )),
            },
            Landed::Diagnosed(result) => match result {
                Ok(diagnostics) => {
                    self.diagnostics = Some(diagnostics);
                    Ok(t("net-notice-diagnosed"))
                }
                Err(error) => Err(t_args(
                    "net-notice-diagnose-err",
                    &[("error", error.into())],
                )),
            },
            Landed::SignedIn(result) => result
                .map(|()| String::new())
                .map_err(|error| t_args("net-notice-signin-err", &[("error", error.into())])),
            Landed::Password {
                uuid,
                purpose,
                result,
            } => self.password_read(uuid, purpose, result),
            Landed::VpnRead { uuid, result } => match result {
                Ok(settings) => {
                    self.adopt_vpn_settings(&uuid, settings);
                    Ok(String::new())
                }
                Err(error) => {
                    self.vpn.errors.insert(uuid, error.clone());
                    Err(t_args("net-vpn-read-err", &[("error", error.into())]))
                }
            },
            Landed::VpnCreated(result) => self.vpn_created(result),
            Landed::VpnExported(result) => result
                .map(|()| t("net-vpn-exported"))
                .map_err(|error| t_args("net-vpn-export-err", &[("error", error.into())])),
        };
        self.notice = Some(match &reported {
            Ok(said) if said.is_empty() => return Ok(Effected::done()),
            Ok(said) | Err(said) => said.clone(),
        });
        reported.map(|_| Effected::done())
    }

    /// Report a landed change, putting back what the pane showed if it
    /// failed.
    #[expect(clippy::too_many_lines, reason = "one arm per change and outcome")]
    fn report(
        &mut self,
        change: &NetworkChange,
        result: Result<(), String>,
    ) -> Result<String, String> {
        if let Err(error) = &result {
            warn!(target: "tilo::settings", %error, "network change failed");
        }
        match (change, result) {
            (NetworkChange::Wifi(on), Ok(())) => Ok(t(if *on {
                "net-notice-wifi-on"
            } else {
                "net-notice-wifi-off"
            })),
            (NetworkChange::Wifi(on), Err(error)) => {
                self.wifi_enabled = !on;
                Err(t_args("net-notice-wifi-err", &[("error", error.into())]))
            }
            (NetworkChange::Wired(on), Ok(())) => Ok(t(if *on {
                "net-notice-wired-on"
            } else {
                "net-notice-wired-off"
            })),
            (NetworkChange::Wired(on), Err(error)) => {
                self.wired.connected = !on;
                Err(t_args("net-notice-wired-err", &[("error", error.into())]))
            }
            (NetworkChange::Radio(radio, on), Ok(())) => Ok(t_args(
                if *on {
                    "net-notice-radio-on"
                } else {
                    "net-notice-radio-off"
                },
                &[("radio", radio_shown(*radio).into())],
            )),
            (NetworkChange::Radio(radio, on), Err(error)) => {
                match radio {
                    Radio::Airplane => self.radios.airplane_mode = !on,
                    Radio::Hotspot => self.radios.hotspot_active = !on,
                }
                Err(t_args(
                    "net-notice-radio-err",
                    &[
                        ("radio", radio_shown(*radio).into()),
                        ("error", error.into()),
                    ],
                ))
            }
            (NetworkChange::Autoconnect { uuid, name, on }, result) => match result {
                Ok(()) => Ok(String::new()),
                Err(error) => {
                    if let Some(saved) = self.profiles.iter_mut().find(|p| &p.uuid == uuid) {
                        saved.autoconnect = !on;
                    }
                    Err(t_args(
                        "net-notice-update-err",
                        &[("name", name.as_str().into()), ("error", error.into())],
                    ))
                }
            },
            (NetworkChange::Wake { .. }, result) => result.map(|()| t("net-wake-saved")),
            (NetworkChange::Forget { uuid, name }, Ok(())) => {
                self.profiles.retain(|p| &p.uuid != uuid);
                if self.in_range(name).is_none() {
                    self.cx
                        .pages()
                        .close(Category::Network, &wifi_page_key(name));
                }
                if self
                    .revealed
                    .as_ref()
                    .is_some_and(|shown| &shown.of == uuid)
                {
                    self.revealed = None;
                }
                Ok(t_args(
                    "net-notice-forgot",
                    &[("name", name.as_str().into())],
                ))
            }
            (NetworkChange::Forget { name, .. }, Err(error)) => Err(t_args(
                "net-notice-forget-err",
                &[("name", name.as_str().into()), ("error", error.into())],
            )),
            (NetworkChange::Hotspot(_), Ok(())) => {
                self.radios.hotspot_active = true;
                Ok(t("net-notice-hotspot-on"))
            }
            (NetworkChange::Hotspot(_), Err(error)) => {
                Err(t_args("net-notice-hotspot-err", &[("error", error.into())]))
            }
            (
                NetworkChange::Ipv4 { name, .. }
                | NetworkChange::Ipv6 { name, .. }
                | NetworkChange::Dns { name, .. },
                Ok(()),
            ) => Ok(t_args(
                "net-notice-ip-ok",
                &[("name", name.as_str().into())],
            )),
            (
                NetworkChange::Ipv4 { .. } | NetworkChange::Ipv6 { .. } | NetworkChange::Dns { .. },
                Err(error),
            ) => Err(t_args("net-notice-ip-err", &[("error", error.into())])),
            (
                NetworkChange::Policy { name, .. }
                | NetworkChange::Identity { name, .. }
                | NetworkChange::Password { name, .. },
                Ok(()),
            ) => Ok(t_args(
                "net-notice-policy-ok",
                &[("name", name.as_str().into())],
            )),
            (
                NetworkChange::Policy { .. }
                | NetworkChange::Identity { .. }
                | NetworkChange::Password { .. },
                Err(error),
            ) => Err(t_args("net-notice-policy-err", &[("error", error.into())])),
            (NetworkChange::Enterprise { .. }, result) => Self::operation(
                result,
                &t("net-notice-enterprise-connected"),
                "net-subject-enterprise",
            ),
            (NetworkChange::JoinHidden { ssid, .. }, result) => {
                self.connecting.retain(|pending| pending != ssid);
                match result {
                    Ok(()) => Ok(t_args(
                        "net-notice-connected",
                        &[("subject", ssid.as_str().into())],
                    )),
                    Err(error) => Err(t_args(
                        "net-notice-configure-err",
                        &[("subject", ssid.as_str().into()), ("error", error.into())],
                    )),
                }
            }
            (NetworkChange::Connect { .. }, result) => result.map(|()| String::new()),
            (
                NetworkChange::Vpn { .. }
                | NetworkChange::VpnProfile { .. }
                | NetworkChange::VpnDelete { .. },
                result,
            ) => self.vpn_changed(change, result),
        }
    }

    fn operation(result: Result<(), String>, done: &str, subject: &str) -> Result<String, String> {
        result.map(|()| done.to_owned()).map_err(|error| {
            t_args(
                "net-notice-configure-err",
                &[("subject", t(subject).into()), ("error", error.into())],
            )
        })
    }

    /// Take in how joining `ssid` ended.
    fn joined(&mut self, ssid: &str, result: Result<Joined, String>) -> Result<String, String> {
        self.connecting.retain(|pending| pending != ssid);
        match result {
            Ok(Joined::Connected { continuity_notice }) => {
                self.start_snapshot();
                Ok(continuity_notice.map_or_else(
                    || t_args("net-notice-connected", &[("subject", ssid.into())]),
                    |notice| {
                        t_args(
                            "net-notice-connected-extra",
                            &[("subject", ssid.into()), ("notice", notice.into())],
                        )
                    },
                ))
            }
            Ok(Joined::PasswordRequired { continuity_warning }) => {
                let security = self
                    .in_range(ssid)
                    .map(|network| security_shown(network.security))
                    .unwrap_or_default();
                self.ask_join(ssid, &security);
                Ok(continuity_warning.map_or_else(
                    || t_args("net-notice-enter-password", &[("ssid", ssid.into())]),
                    |warning| {
                        t_args(
                            "net-notice-enter-password-warn",
                            &[("ssid", ssid.into()), ("warning", warning.into())],
                        )
                    },
                ))
            }
            Err(error) => {
                warn!(target: "tilo::settings", op = "connect_wifi", %ssid, %error, "joining a network failed");
                Err(t_args(
                    "net-notice-configure-err",
                    &[("subject", ssid.into()), ("error", error.into())],
                ))
            }
        }
    }

    /// Take in the CA certificate the picker chose.
    fn poll_picker(&mut self) {
        let Some(result) = self
            .ca_picker
            .as_ref()
            .and_then(|picker| picker.try_recv().ok())
        else {
            return;
        };
        match result {
            ui::portal::PortalOutcome::Opened(path) => {
                self.enterprise.ca.set(path.display().to_string());
                self.notice = Some(t("net-notice-cert-chosen"));
            }
            ui::portal::PortalOutcome::Cancelled => {}
            ui::portal::PortalOutcome::Failed(error) => {
                self.notice = Some(t_args("net-notice-cert-err", &[("error", error.into())]));
            }
        }
        self.ca_picker = None;
        self.changed = true;
    }

    /// Why the Wi-Fi status cannot be told, when it cannot: the radio or the
    /// network list could not be read.
    fn wifi_status_error(&self) -> Option<String> {
        let error = self
            .read_errors
            .wifi_enabled
            .as_ref()
            .or(self.read_errors.networks.as_ref())?;
        Some(t_args(
            "net-status-error",
            &[("error", error.as_str().into())],
        ))
    }

    fn connection_status_text(&self) -> String {
        if !self.wifi_enabled {
            return t("net-wifi-off");
        }
        match self.networks.iter().find(|n| n.connected) {
            Some(n) => t_args("net-connected-to", &[("ssid", n.ssid.as_str().into())]),
            None => t("net-not-connected"),
        }
    }
}

fn wifi_row_id(ssid: &str) -> String {
    format!("network.wifi:{ssid}")
}

/// Connect to `ssid`, then seal a typed password for the person's other
/// devices.
fn join(
    service: &dyn NetworkService,
    ssid: &str,
    portable: Option<ui::collaboration::PortableWifiSecurity>,
    password: Option<Zeroizing<String>>,
) -> Result<Joined, String> {
    let shared = password.as_ref().map(|password| password.to_string());
    service.change(&NetworkChange::Connect {
        ssid: ssid.to_owned(),
        password,
    })?;
    let continuity_notice = shared.map(Zeroizing::new).and_then(|password| {
        let mode = portable?;
        Some(
            match service.continuity(WifiContinuityCommand::Share {
                ssid: ssid.to_owned(),
                security: mode,
                password: password.to_string(),
            }) {
                Ok(WifiContinuityReply::Shared { .. }) => t("net-notice-password-sealed"),
                Ok(WifiContinuityReply::Rejected { reason }) => {
                    t_args("net-notice-share-err", &[("error", reason.into())])
                }
                Ok(_) => t("net-notice-share-invalid"),
                Err(error) => t_args("net-notice-share-err", &[("error", error.into())]),
            },
        )
    });
    Ok(Joined::Connected { continuity_notice })
}

/// Connect to secure `ssid` with a password the person's devices share; a
/// password is asked for when they share none.
fn join_with_shared_password(
    service: &dyn NetworkService,
    ssid: &str,
    portable: Option<ui::collaboration::PortableWifiSecurity>,
) -> Result<Joined, String> {
    let ask = |continuity_warning| Ok(Joined::PasswordRequired { continuity_warning });
    let Some(mode) = portable else {
        return ask(None);
    };
    match service.continuity(WifiContinuityCommand::Lookup {
        ssid: ssid.to_owned(),
        security: mode,
    }) {
        Ok(WifiContinuityReply::Password { password }) => {
            service.change(&NetworkChange::Connect {
                ssid: ssid.to_owned(),
                password: Some(Zeroizing::new(password)),
            })?;
            Ok(Joined::Connected {
                continuity_notice: Some(t("net-notice-used-sealed")),
            })
        }
        Ok(WifiContinuityReply::NotFound) => ask(None),
        Ok(WifiContinuityReply::Rejected { reason }) => ask(Some(reason)),
        Ok(_) => ask(Some(
            "the Wi-Fi continuity service returned an invalid lookup reply".into(),
        )),
        Err(error) => ask(Some(error)),
    }
}

impl PaneModel for NetworkModel {
    fn compose(&self) -> Box<dyn ui::widget::Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_background();
        self.read_reported_changes();
        self.poll_prompt();
        self.poll_device_vpn();
        if let Some(mobile) = &self.mobile
            && let Ok(mut mobile) = mobile.try_borrow_mut()
        {
            self.changed |= mobile.poll();
        }
        self.scan_when_due(ui::time::now());
        let selection = self.wired_profile.get();
        if selection != self.seen_selection {
            self.seen_selection = selection;
            self.changed = true;
        }
        let pages = self.cx.pages().revision();
        if pages != self.pages_seen {
            self.pages_seen = pages;
            self.left_pages();
            self.changed = true;
        }
        // A list let go takes the order it held back.
        let held = self.held_short.get() || self.held_all.get();
        if held != self.held_seen {
            self.held_seen = held;
            if !held {
                self.changed = true;
            }
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "NetworkPane"
    }

    fn next_wake(&self) -> Option<ui::time::Instant> {
        let scan = self
            .scans_networks()
            .then(|| self.scanned_at?.checked_add(SCAN_EVERY))
            .flatten();
        let mobile = self
            .mobile
            .as_ref()
            .and_then(|mobile| mobile.try_borrow().ok()?.next_wake());
        scan.into_iter().chain(mobile).min()
    }

    fn follow(&mut self) -> Option<Rc<dyn std::any::Any>> {
        let device = self.follow_device();
        Some(Rc::new((device, self.follow_device_vpn())))
    }
}

impl NetworkModel {
    /// Whether the page looks for networks by itself: Wi-Fi is on and the
    /// device has answered.
    fn scans_networks(&self) -> bool {
        self.loaded && self.wifi_enabled && !self.wifi_absent()
    }

    /// Drop what only a page that is no longer shown held: a password shown
    /// or a code drawn on a network's page, a filter of the full list.
    fn left_pages(&mut self) {
        let trail: Vec<String> = self
            .cx
            .pages()
            .trail(Category::Network)
            .into_iter()
            .map(|page| page.key)
            .collect();
        let showing = |key: String| trail.contains(&key);
        if let Some(shown) = &self.revealed {
            let still = self
                .profiles
                .iter()
                .find(|profile| profile.uuid == shown.of)
                .is_some_and(|profile| showing(wifi_page_key(&profile.name)));
            if !still {
                self.revealed = None;
            }
        }
        if self
            .code
            .as_ref()
            .is_some_and(|shown| !showing(wifi_page_key(&shown.of)))
        {
            self.code = None;
        }
        if !showing(PAGE_ALL.to_owned()) {
            self.filter.clear();
        }
    }

    /// Scan, then read the networks in range, when the last look is
    /// [`SCAN_EVERY`] old: the list follows the room while the page is
    /// open, with no Refresh to press. The status never waits for it.
    fn scan_when_due(&mut self, now: ui::time::Instant) {
        let due = self.scans_networks()
            && self.snapshot.is_none()
            && self.in_flight == 0
            && self
                .scanned_at
                .is_none_or(|at| now.saturating_duration_since(at) >= SCAN_EVERY);
        if !due {
            return;
        }
        self.scanned_at = Some(now);
        let (sender, receiver) = crate::channel::channel();
        let service = Arc::clone(&self.service);
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-network-scan",
            Box::new(move || {
                // A failed scan still reads what `NetworkManager` knows.
                if let Err(error) = service.scan() {
                    warn!(target: "tilo::settings", op = "scan_wifi", %error, "Wi-Fi scan failed");
                }
                if sender.send(service.networks()).is_ok() {
                    wake.wake();
                }
            }),
        );
        if started.is_ok() {
            self.snapshot = Some(receiver);
            self.scan_in_flight = true;
            self.changed = true;
        }
    }

    /// Take in a snapshot, a chosen file or work of the pane's own that
    /// arrived.
    fn poll_background(&mut self) {
        let listed = self.cx.radios().now();
        if listed != self.listed {
            let plugged = self.listed.is_some();
            self.listed = listed;
            self.changed = true;
            // A radio plugged in or pulled out: what `NetworkManager` makes
            // of it is read again.
            if plugged && self.loaded && self.snapshot.is_none() {
                self.start_snapshot();
            }
        }
        if let Some(receiver) = &self.snapshot {
            match receiver.try_recv() {
                Ok(snapshot) => {
                    self.snapshot = None;
                    self.scan_in_flight = false;
                    let _ = self.adopt_snapshot(snapshot);
                }
                Err(crate::channel::TryRecvError::Disconnected) => {
                    self.snapshot = None;
                    self.scan_in_flight = false;
                    self.changed = true;
                }
                Err(crate::channel::TryRecvError::Empty) => {}
            }
        }
        let mut landed = Vec::new();
        self.jobs.retain(|job| match job.try_recv() {
            Ok(done) => {
                landed.push(done);
                false
            }
            Err(crate::channel::TryRecvError::Disconnected) => false,
            Err(crate::channel::TryRecvError::Empty) => true,
        });
        for done in landed {
            // The notice says how it went; nobody waits on the answer.
            let _ = self.land(done);
        }
        self.poll_picker();
        self.poll_vpn_pickers();
    }
}

/// `row` with the settings of the saved profile `uuid` offered on it, each
/// asking the value to give.
fn with_profile_settings(
    mut row: LiveRow,
    uuid: &str,
    settings: &os_actions::ConnectionProfileSettings,
) -> LiveRow {
    let asking =
        |effect: NetworkEffect, asking: &[&str]| SettingsEffect::from(effect).bound_asking(asking);
    let uuid = uuid.to_owned();
    row = row
        .operation(asking(
            NetworkEffect::SetPriority {
                profile: uuid.clone(),
                priority: settings.priority.unwrap_or(0),
            },
            &["priority"],
        ))
        .operation(asking(
            NetworkEffect::SetMeteredPolicy {
                profile: uuid.clone(),
                policy: settings.metered.map_or("Automatic", metered_id).into(),
            },
            &["policy"],
        ))
        .operation(asking(
            NetworkEffect::SetMacAddress {
                profile: uuid.clone(),
                policy: settings
                    .mac
                    .map_or("Random for this network", mac_id)
                    .into(),
            },
            &["policy"],
        ))
        .operation(asking(
            NetworkEffect::SetHiddenNetwork {
                profile: uuid.clone(),
                on: !settings.hidden.unwrap_or(false),
            },
            &["on"],
        ))
        .operation(asking(
            NetworkEffect::SetDns {
                profile: uuid.clone(),
                servers: settings
                    .ipv4
                    .as_ref()
                    .map(|ipv4| ipv4.dns.join(", "))
                    .unwrap_or_default(),
            },
            &["servers"],
        ))
        .operation(asking(
            NetworkEffect::SetIpv6 {
                profile: uuid.clone(),
                method: None,
                addresses: None,
                gateway: None,
            },
            &["method", "addresses", "gateway"],
        ))
        .operation(asking(
            NetworkEffect::SetProxy {
                profile: uuid.clone(),
                mode: None,
                address: None,
            },
            &["mode", "address"],
        ));
    if settings.enterprise.is_some() {
        row = row.operation(asking(
            NetworkEffect::SetEnterpriseIdentity {
                profile: uuid,
                identity: None,
                ca_certificate: None,
            },
            &["identity", "ca_certificate"],
        ));
    }
    row
}
