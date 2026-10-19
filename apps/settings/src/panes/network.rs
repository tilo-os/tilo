//! Network — Wi-Fi, wired, VPN, hotspot, mobile broadband and the saved
//! connection profiles of this device, as `NetworkManager` reports them.
//!
//! Every read and change may block on `nmcli` or the system bus for
//! seconds, so none runs on the UI thread: the model reads a snapshot in the
//! background when it is first needed and again after each change, and every
//! change is a [`NetworkEffect`] that runs on a worker and reports what
//! happened. Passwords and PINs live in process-local buffers that are
//! zeroed once used; they never reach a document.
//!
//! A radio the device does not have is not offered: Wi-Fi and the hotspot
//! without a Wi-Fi radio, mobile broadband without a modem, airplane mode
//! with neither ([`NetworkArea::statuses`]).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::{
    ConnectivityState, Dispatcher, EnterpriseWifiConfiguration, HotspotBand, HotspotConfiguration,
    IpConfiguration, SavedNetworkProfile, ToolIntent, VpnRoute, WifiNetworkInfo, WiredInfo,
};
use tracing::warn;
use ui::LocalState;
use ui::collaboration::{NetworkWakeStatus, WifiContinuityCommand, WifiContinuityReply};
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow};
use ui::tilo_resource::{self, ActionRef, Effect};
use ui::tokens::Rhythm;
use ui::widget::{
    Button, DisclosureGroup, DisclosureRow, Dropdown, ListRow, QrWidget, Row, Spacer, TextInput,
    Toggle, Widget,
};
use zeroize::{Zeroize, Zeroizing};

use crate::dispatcher::dispatcher_or_real;
use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, Item, SettingsSection, block, card, card_with_action, error_caption, error_row, field_row,
    note, row, rows, setting_row, setting_row_described, value_row,
};

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
    "Stable per network",
    "Random each connection",
    "Permanent hardware address",
];
/// What a new hotspot is called until the person names it.
const DEFAULT_HOTSPOT_NAME: &str = "Tilo Hotspot";

/// What Network changes on this device.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum NetworkEffect {
    /// Read the adopted network state, pending work and labels this pane shows.
    #[effect(
        view,
        returns,
        device_only,
        kind = "read",
        label = "Inspect network state"
    )]
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
    /// Use mobile broadband, or stop.
    #[effect(job, idempotent, label = "Mobile broadband")]
    SetMobileBroadband { on: bool },
    /// Connect to a Wi-Fi network. Without a password a secure network uses
    /// one the person's devices share, else asks for it; while its password
    /// prompt is open, the password typed there.
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
    /// Join a saved network by itself whenever it is in range, or not.
    #[effect(job, row, idempotent, label = "Connect automatically")]
    SetAutoconnect {
        #[param("The saved network's id")]
        profile: String,
        on: bool,
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
    /// Connect or disconnect a VPN.
    #[effect(job, row, idempotent, label = "VPN")]
    SetVpn {
        #[param("The VPN's name")]
        vpn: String,
        on: bool,
    },
    /// Open VPN Studio, at one VPN or at its overview.
    #[effect(label = "Open VPN Studio")]
    OpenVpnStudio {
        #[param("The VPN's name; the overview when omitted")]
        vpn: Option<String>,
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
    /// Set a wired profile's IPv4 settings; what is omitted is what its
    /// form holds.
    #[effect(job, label = "Apply IP settings")]
    SetIpv4 {
        #[param("The wired profile's id")]
        profile: String,
        #[param("Take the address from the network")]
        automatic: Option<bool>,
        #[param("Addresses with their prefix, comma-separated: 192.168.1.20/24")]
        addresses: Option<String>,
        gateway: Option<String>,
        #[param("Name servers, comma-separated")]
        dns: Option<String>,
    },
    /// Open or close a known network's details in place.
    #[effect(view, row, label = "Network details")]
    ShowKnownNetwork {
        #[param("The saved network's id")]
        profile: String,
    },
    /// Count a known network's data against a limit, or leave it to the
    /// network to say.
    #[effect(job, row, idempotent, label = "Metered connection")]
    SetMetered {
        #[param("The saved network's id")]
        profile: String,
        on: bool,
    },
    /// Open or close the hotspot's setup.
    #[effect(view, label = "Hotspot setup")]
    ShowHotspotSetup,
    /// Set a saved Wi-Fi network's policy; what is omitted is what its
    /// form holds.
    #[effect(job, label = "Apply connection policy")]
    SetConnectionPolicy {
        #[param("The saved network's id")]
        profile: String,
        #[param("Whether data counts against a limit", choices = METERED_POLICIES.iter().copied())]
        metered: Option<String>,
        #[param("The hardware address it shows", choices = MAC_POLICIES.iter().copied())]
        mac: Option<String>,
        #[param("A proxy auto-configuration URL")]
        proxy: Option<String>,
    },
    /// Create a mobile broadband profile; what is omitted is what the form
    /// holds.
    #[effect(job, label = "Create mobile profile")]
    CreateMobileProfile {
        name: Option<String>,
        #[param("The access point name")]
        apn: Option<String>,
    },
    /// Unlock a SIM with its PIN; what is omitted is what the form holds.
    #[effect(job, label = "Unlock SIM")]
    UnlockSim {
        #[param("The SIM, as listed")]
        sim: Option<String>,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Show or hide the enterprise (802.1X) network setup.
    #[effect(view, label = "Enterprise network setup")]
    ShowEnterpriseSetup,
    /// Choose the organization's CA certificate file.
    #[effect(label = "Choose CA certificate")]
    ChooseCaCertificate,
    /// Save an enterprise (802.1X) Wi-Fi network and connect; what is
    /// omitted is what the setup form holds.
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
        return app.network().inspection().map(Effected::value);
    }
    if effect == NetworkEffect::RefreshNetworks {
        // A dongle plugged in a moment ago is seen without waiting.
        app.host().radios().look_again();
    }
    let begun = app.network().model().borrow_mut().begin(effect)?;
    Ok(match begun {
        Begun::Done => Effected::done(),
        Begun::Work(work) => {
            crate::device_read::job(cx, work, |app: &mut SettingsApp, landed: Landed, _| {
                app.network().model().borrow_mut().land(landed)
            })
        }
    })
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
        os_actions::MacPolicy::Stable => "Stable per network",
        os_actions::MacPolicy::Random => "Random each connection",
        os_actions::MacPolicy::Permanent => "Permanent hardware address",
    }
}

fn mac_shown(id: &str) -> String {
    t(match id {
        "Random each connection" => "net-mac-random",
        "Permanent hardware address" => "net-mac-permanent",
        _ => "net-mac-stable",
    })
}

fn mac_from_label(label: &str) -> Option<os_actions::MacPolicy> {
    if label == "Random each connection" || label == t("net-mac-random") {
        Some(os_actions::MacPolicy::Random)
    } else if label == "Permanent hardware address" || label == t("net-mac-permanent") {
        Some(os_actions::MacPolicy::Permanent)
    } else if label == "Stable per network" || label == t("net-mac-stable") {
        Some(os_actions::MacPolicy::Stable)
    } else {
        None
    }
}

fn radio_shown(radio: Radio) -> String {
    t(match radio {
        Radio::Airplane => "net-radio-airplane",
        Radio::Hotspot => "net-radio-hotspot",
        Radio::Mobile => "net-radio-mobile",
    })
}

fn profile_label(profile: &SavedNetworkProfile) -> String {
    let short_uuid: String = profile.uuid.chars().take(8).collect();
    format!("{} ({})", profile.name, short_uuid)
}

/// A comma-separated list, trimmed, at most 17 entries.
fn list_of(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
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

/// A configured VPN / `WireGuard` connection and whether it is currently up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnInfo {
    pub name: String,
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

/// Everything Network shows from `NetworkManager`, read in one pass off the
/// UI thread. Each part keeps its own failure, so one unreadable value is
/// shown as unreadable instead of as a device state.
pub(crate) struct NetworkSnapshot {
    networks: Result<Vec<WifiNetworkInfo>, String>,
    wifi_enabled: Part<bool>,
    /// Read only for the first snapshot.
    sims: Part<Vec<os_actions::SimInfo>>,
    wired: Part<WiredInfo>,
    radios: Part<os_actions::RadioState>,
    connectivity: Part<ConnectivityState>,
    profiles: Part<Vec<SavedNetworkProfile>>,
    profile_settings: HashMap<String, os_actions::ConnectionProfileSettings>,
    wake_settings: HashMap<String, Result<NetworkWakeStatus, String>>,
    hotspot_clients: Part<Vec<os_actions::HotspotClient>>,
    vpns: Part<Vec<VpnInfo>>,
}

impl NetworkSnapshot {
    /// Query `NetworkManager`. Every call here may block on `nmcli` or the
    /// system bus, so this never runs on the UI thread.
    fn read(initial: bool) -> Self {
        let profiles = os_actions::list_saved_network_profiles();
        let radios = os_actions::get_radio_state();
        Self {
            networks: os_actions::list_wifi_networks(),
            wifi_enabled: Some(wifi_enabled_from(&radios)),
            sims: initial.then(os_actions::list_mobile_sims),
            wired: Some(os_actions::get_wired()),
            radios: Some(radios),
            connectivity: Some(os_actions::get_connectivity()),
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
            hotspot_clients: Some(os_actions::list_hotspot_clients()),
            vpns: Some(os_actions::list_vpn_connections().map(|vpns| {
                vpns.into_iter()
                    .map(|v| VpnInfo {
                        name: v.name,
                        active: v.active,
                    })
                    .collect()
            })),
        }
    }

    /// `networks` alone: every other part is not read, so adopting it
    /// leaves them as they are.
    fn networks_only(networks: Result<Vec<WifiNetworkInfo>, String>) -> Self {
        Self {
            networks,
            wifi_enabled: None,
            sims: None,
            wired: None,
            radios: None,
            connectivity: None,
            profiles: None,
            profile_settings: HashMap::new(),
            wake_settings: HashMap::new(),
            hotspot_clients: None,
            vpns: None,
        }
    }

    /// Only `networks`, with every other part read and empty.
    fn of_networks(networks: Vec<WifiNetworkInfo>) -> Self {
        Self {
            networks: Ok(networks),
            wifi_enabled: Some(Ok(true)),
            sims: Some(Ok(Vec::new())),
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
        }
    }
}

/// The status line: its glyph, sentence and detail, and the action that
/// fixes what it reports (an id, its label and the effect).
type StatusLine = (
    ui::icons::Icon,
    String,
    String,
    Option<(&'static str, String, NetworkEffect)>,
);

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
    sims: Option<String>,
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
    Mobile,
}

/// A change to this device's networking, carried out off the UI thread.
/// Secrets are zeroed when it is dropped.
pub(crate) enum NetworkChange {
    Wifi(bool),
    Wired(bool),
    Radio(Radio, bool),
    Vpn {
        name: String,
        on: bool,
    },
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
    Policy {
        uuid: String,
        name: String,
        patch: os_actions::ConnectionPolicyPatch,
    },
    Connect {
        ssid: String,
        password: Option<Zeroizing<String>>,
    },
    MobileProfile {
        name: String,
        apn: String,
    },
    UnlockSim {
        sim: String,
        pin: Zeroizing<String>,
    },
    Enterprise {
        config: EnterpriseWifiConfiguration,
        password: Zeroizing<String>,
    },
}

impl Drop for NetworkChange {
    fn drop(&mut self) {
        if let Self::Hotspot(config) = self {
            config.password.zeroize();
        }
    }
}

/// What carries out Network's reads and changes: `NetworkManager` on a
/// device, a fixed sample in a screenshot or test.
pub(crate) trait NetworkService: Send + Sync {
    /// Read everything Network shows; `initial` also reads the SIMs.
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
    /// Ask the device to check internet connectivity again, so the next
    /// snapshot carries a fresh verdict. Only the explicit refresh does this.
    fn recheck_connectivity(&self) {}
    /// Carry out `change`.
    ///
    /// # Errors
    /// Why the device refused or failed it.
    fn change(&self, change: &NetworkChange) -> Result<(), String>;
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
    /// Open a system tool.
    ///
    /// # Errors
    /// Why it could not be opened.
    fn open_tool(&self, intent: ToolIntent) -> Result<(), String>;
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

    fn recheck_connectivity(&self) {
        // A refused or failed recheck still leaves the last verdict, which
        // the snapshot that follows reads and reports.
        let _ = os_actions::check_connectivity_now();
    }

    fn scan(&self) -> Result<(), String> {
        os_actions::scan_wifi()
    }

    fn change(&self, change: &NetworkChange) -> Result<(), String> {
        let dispatcher = &self.dispatcher;
        match change {
            NetworkChange::Wifi(on) => dispatcher.set_wifi(*on),
            NetworkChange::Wired(on) => dispatcher.set_wired(*on),
            NetworkChange::Radio(Radio::Airplane, on) => dispatcher.set_airplane_mode(*on),
            NetworkChange::Radio(Radio::Hotspot, on) => dispatcher.set_hotspot(*on),
            NetworkChange::Radio(Radio::Mobile, on) => dispatcher.set_mobile_broadband(*on),
            NetworkChange::Vpn { name, on } => dispatcher.set_vpn_connection(name, *on),
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
            NetworkChange::Policy { uuid, patch, .. } => {
                os_actions::configure_connection_policy_patch(uuid, patch)
            }
            NetworkChange::Connect { ssid, password } => {
                dispatcher.connect_wifi(ssid, password.as_deref().map(String::as_str))
            }
            NetworkChange::MobileProfile { name, apn } => {
                os_actions::create_mobile_profile(name, apn)
            }
            NetworkChange::UnlockSim { sim, pin } => os_actions::unlock_mobile_sim_on(sim, pin),
            NetworkChange::Enterprise { config, password } => {
                os_actions::connect_enterprise_wifi(config, password)
            }
        }
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

    fn open_tool(&self, intent: ToolIntent) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::open_tool(intent)
    }
}

/// A few nearby networks and a wired link, for a screenshot: every change
/// is accepted and reaches nothing.
struct SampleNetwork;

impl SampleNetwork {
    fn networks() -> Vec<WifiNetworkInfo> {
        vec![
            WifiNetworkInfo {
                ssid: "Home_Network".into(),
                signal: 4,
                secure: true,
                security: os_actions::WifiSecurity::Wpa3,
                connected: true,
                known: true,
            },
            WifiNetworkInfo {
                ssid: "Cafe_Guest".into(),
                signal: 3,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: false,
                known: false,
            },
            WifiNetworkInfo {
                ssid: "Office_5G".into(),
                signal: 2,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: false,
                known: false,
            },
        ]
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
        let profile = |uuid: &str, name: &str, autoconnect| SavedNetworkProfile {
            uuid: uuid.into(),
            name: name.into(),
            kind: "802-11-wireless".into(),
            autoconnect,
        };
        snapshot.profiles = Some(Ok(vec![
            profile("5b2e8a70-1c7d-4f2e-9a61-0d3c9e4b7a10", "Home_Network", true),
            profile("9e41c2d8-6a35-4b9f-8c07-2f5d1e8a3b64", "Library", false),
        ]));
        snapshot.profile_settings = [(
            "5b2e8a70-1c7d-4f2e-9a61-0d3c9e4b7a10".to_string(),
            os_actions::ConnectionProfileSettings {
                metered: Some(os_actions::MeteredPolicy::Automatic),
                mac: None,
                proxy: None,
                ipv4: None,
            },
        )]
        .into_iter()
        .collect();
        snapshot
    }

    fn change(&self, _change: &NetworkChange) -> Result<(), String> {
        Ok(())
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

    fn open_tool(&self, _intent: ToolIntent) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
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

/// Work that finished on a worker, for the model to take in.
pub(crate) enum Landed {
    Refreshed {
        snapshot: NetworkSnapshot,
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
    /// A system tool the shell was asked to open.
    Opened(Result<(), String>),
}

/// The fields of one saved Wi-Fi network's policy form.
struct PolicyForm {
    metered: LocalState<String>,
    mac: LocalState<String>,
    proxy: LocalState<String>,
}

/// The fields of one wired profile's IPv4 form.
struct Ipv4Form {
    automatic: LocalState<bool>,
    addresses: LocalState<String>,
    gateway: LocalState<String>,
    dns: LocalState<String>,
}

/// What an enterprise connection was given; the setup form fills the rest.
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
    password: Option<String>,
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
    password: Arc<Mutex<String>>,
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
            password: Arc::new(Mutex::new(String::new())),
        }
    }
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
    /// hotspot without a Wi-Fi radio, mobile broadband without a modem,
    /// airplane mode with neither. Reads nothing itself: only what the
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
        if wwan {
            unavailable(
                &[NetworkEffect::SET_MOBILE_BROADBAND.id],
                "no_modem",
                &t("net-no-modem"),
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
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per nearby network, saved network and VPN, each offering
    /// what can be done with it.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let offer = |effect: NetworkEffect| SettingsEffect::from(effect).bound();
        let mut rows: Vec<LiveRow> = model
            .networks
            .iter()
            .map(|network| {
                let row = LiveRow::new(
                    network_row_id(network),
                    "settings.network",
                    network.ssid.clone(),
                );
                if network.connected {
                    row
                } else {
                    row.operation(offer(NetworkEffect::ConnectNetwork {
                        ssid: network.ssid.clone(),
                        password: None,
                    }))
                }
            })
            .collect();
        rows.extend(model.profiles.iter().map(|profile| {
            let mut row = LiveRow::new(
                format!("settings.network-profile:{}", profile.uuid),
                "settings.network-profile",
                profile.name.clone(),
            )
            .operation(offer(NetworkEffect::SetAutoconnect {
                profile: profile.uuid.clone(),
                on: !profile.autoconnect,
            }))
            .operation(offer(NetworkEffect::ForgetNetwork {
                profile: profile.uuid.clone(),
            }));
            if profile.kind == "802-11-wireless" {
                row = row.operation(offer(NetworkEffect::ShareNetwork {
                    profile: profile.uuid.clone(),
                }));
            }
            if let Some(Ok(wake)) = model.wake_settings.get(&profile.uuid) {
                row = row.presentation(ui::tilo_actions::ResourcePresentation {
                    detail: None,
                    badge: None,
                    preview: Some(t(wake_description(wake))),
                });
                if wake.configured || wake.supported == Some(true) {
                    row = row.operation(offer(NetworkEffect::SetNetworkWake {
                        profile: profile.uuid.clone(),
                        on: !wake.configured,
                    }));
                }
            }
            row
        }));
        rows.extend(model.vpns.iter().map(|vpn| {
            LiveRow::new(
                format!("settings.vpn:{}", vpn.name),
                "settings.vpn",
                vpn.name.clone(),
            )
            .operation(offer(NetworkEffect::SetVpn {
                vpn: vpn.name.clone(),
                on: !vpn.active,
            }))
        }));
        rows
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
    sims: Vec<os_actions::SimInfo>,
    hotspot_clients: Vec<os_actions::HotspotClient>,
    vpns: Vec<VpnInfo>,
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
    /// The secure network whose password prompt is open.
    expanded: Option<String>,
    /// The password typed into each open prompt.
    join_passwords: HashMap<String, Arc<Mutex<String>>>,
    /// The known network whose details are open in place.
    known_open: Option<String>,
    /// The rows of the networks in range, keyed by name: a password prompt
    /// opens under its network and stays open while the page is rebuilt.
    prompts: DisclosureGroup,
    /// The known networks' rows, keyed by profile, which open their details.
    known_rows: DisclosureGroup,
    /// The prompt and the details the rows were last opened for, so a row
    /// the person closed is not opened again by the next rebuild.
    prompt_shown: std::cell::RefCell<Option<String>>,
    known_shown: std::cell::RefCell<Option<String>>,
    enterprise_open: bool,
    /// The hotspot's setup is open while the hotspot is off.
    hotspot_open: bool,
    /// When the open page last scanned for networks: it scans again every
    /// [`SCAN_EVERY`] while it shows them.
    scanned_at: Option<ui::time::Instant>,
    /// A scan the page started has not landed yet.
    scan_in_flight: bool,
    hotspot_code_shown: bool,
    /// The saved Wi-Fi and wired profile being edited, by label.
    wifi_profile: LocalState<String>,
    wired_profile: LocalState<String>,
    seen_selection: (String, String),
    hotspot_name: LocalState<String>,
    hotspot_password: Arc<Mutex<String>>,
    hotspot_band: LocalState<String>,
    policy_forms: HashMap<String, PolicyForm>,
    ipv4_forms: HashMap<String, Ipv4Form>,
    mobile_name: LocalState<String>,
    mobile_apn: LocalState<String>,
    mobile_sim: LocalState<String>,
    sim_pin: Arc<Mutex<String>>,
    enterprise: EnterpriseForm,
    ca_picker: Option<ui::portal::PortalChannel>,
    changed: bool,
}

impl Drop for NetworkModel {
    fn drop(&mut self) {
        let secrets = [
            &self.hotspot_password,
            &self.sim_pin,
            &self.enterprise.password,
        ];
        for buffer in secrets.into_iter().chain(self.join_passwords.values()) {
            if let Ok(mut secret) = buffer.lock() {
                secret.zeroize();
            }
        }
    }
}

/// The AP identity shared by publication and inspection.
/// A network in range as a row of the published surface, offering to join
/// it; the pane's controls for it bind to it.
fn network_live(network: &WifiNetworkInfo) -> ui::tilo_actions::Resource {
    LiveRow::new(
        network_row_id(network),
        "settings.network",
        network.ssid.clone(),
    )
    .operation(
        SettingsEffect::from(NetworkEffect::ConnectNetwork {
            ssid: network.ssid.clone(),
            password: None,
        })
        .bound(),
    )
    .into_resource()
}

fn network_row_id(network: &WifiNetworkInfo) -> String {
    format!("settings.network:{}", network.ssid)
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
                    "resource":network_row_id(network), "ssid":network.ssid,
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
                "enterprise_entry":t("net-enterprise-entry"), "refresh":t("net-refresh") },
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

    /// Sample networks: by default a secure one's password prompt open,
    /// as joining it looks; a network capture asks for another moment.
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
            Some(NetworkSample::Known) => {
                model.known_open = model
                    .profiles
                    .iter()
                    .find(|profile| profile.kind == "802-11-wireless")
                    .map(|profile| profile.uuid.clone());
            }
            None => model.set_expanded(Some("Office_5G".into())),
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
            sims: Vec::new(),
            hotspot_clients: Vec::new(),
            vpns: Vec::new(),
            diagnostics: None,
            read_errors: NetworkReadErrors::default(),
            in_flight: 0,
            refreshes_pending: 0,
            connecting: Vec::new(),
            notice: None,
            expanded: None,
            join_passwords: HashMap::new(),
            known_open: None,
            prompts: DisclosureGroup::new(),
            known_rows: DisclosureGroup::new(),
            prompt_shown: std::cell::RefCell::new(None),
            known_shown: std::cell::RefCell::new(None),
            enterprise_open: false,
            hotspot_open: false,
            scanned_at: None,
            scan_in_flight: false,
            hotspot_code_shown: false,
            wifi_profile: LocalState::new(String::new()),
            wired_profile: LocalState::new(String::new()),
            seen_selection: (String::new(), String::new()),
            hotspot_name: LocalState::new(DEFAULT_HOTSPOT_NAME.into()),
            hotspot_password: Arc::new(Mutex::new(String::new())),
            hotspot_band: LocalState::new(band_shown("Automatic")),
            policy_forms: HashMap::new(),
            ipv4_forms: HashMap::new(),
            mobile_name: LocalState::new(String::new()),
            mobile_apn: LocalState::new(String::new()),
            mobile_sim: LocalState::new(String::new()),
            sim_pin: Arc::new(Mutex::new(String::new())),
            enterprise: EnterpriseForm::new(),
            ca_picker: None,
            changed: false,
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

    /// Start reading a snapshot in the background; a newer read supersedes
    /// an older one still running.
    fn start_snapshot(&mut self) {
        let (sender, receiver) = crate::channel::channel();
        let service = Arc::clone(&self.service);
        let initial = !self.loaded;
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-network-read",
            Box::new(move || {
                if sender.send(service.snapshot(initial)).is_ok() {
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

    /// Adopt a snapshot; what refreshing reports.
    fn adopt_snapshot(&mut self, snapshot: NetworkSnapshot) -> Result<String, String> {
        self.changed = true;
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
        if let Some(sims) = adopt(snapshot.sims, &mut self.read_errors.sims) {
            self.sims = sims;
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
        if self.mobile_sim.get().is_empty()
            && let Some(sim) = self.sims.first()
        {
            self.mobile_sim.set(sim.label.clone());
        }
        self.loaded = true;
        outcome
    }

    /// Give each saved profile its forms, filled from what `NetworkManager`
    /// reports, and keep each selector on a profile that still exists.
    fn fill_forms(&mut self) {
        for profile in &self.profiles {
            let Some(settings) = self.profile_settings.get(&profile.uuid) else {
                continue;
            };
            if profile.kind == "802-11-wireless" {
                self.policy_forms
                    .entry(profile.uuid.clone())
                    .or_insert_with(|| PolicyForm {
                        metered: LocalState::new(metered_shown(metered_id(
                            settings.metered.unwrap_or_default(),
                        ))),
                        mac: LocalState::new(mac_shown(
                            settings.mac.map_or("Stable per network", mac_id),
                        )),
                        proxy: LocalState::new(
                            settings
                                .proxy
                                .as_ref()
                                .and_then(|proxy| proxy.automatic_url.clone())
                                .unwrap_or_default(),
                        ),
                    });
            }
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
        }
        for (selection, kind) in [
            (&self.wifi_profile, "802-11-wireless"),
            (&self.wired_profile, "802-3-ethernet"),
        ] {
            let current = selection.get();
            if !self
                .profiles
                .iter()
                .any(|p| p.kind == kind && profile_label(p) == current)
            {
                selection.set(
                    self.profiles
                        .iter()
                        .find(|p| p.kind == kind)
                        .map(profile_label)
                        .unwrap_or_default(),
                );
            }
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

    /// The saved profile `selection` names, else the first.
    fn selected<'a>(
        profiles: &[&'a SavedNetworkProfile],
        selection: &LocalState<String>,
    ) -> Option<&'a SavedNetworkProfile> {
        let label = selection.get();
        profiles
            .iter()
            .find(|p| profile_label(p) == label)
            .or_else(|| profiles.first())
            .copied()
    }

    /// Open the password prompt of `ssid`, or close it; the previous
    /// prompt's password is zeroed.
    fn set_expanded(&mut self, ssid: Option<String>) {
        if self.expanded == ssid {
            return;
        }
        if let Some(old) = self.expanded.take()
            && let Some(buffer) = self.join_passwords.remove(&old)
            && let Ok(mut secret) = buffer.lock()
        {
            secret.zeroize();
        }
        if let Some(ssid) = &ssid {
            self.join_passwords
                .entry(ssid.clone())
                .or_insert_with(|| Arc::new(Mutex::new(String::new())));
        }
        self.expanded = ssid;
        self.changed = true;
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
                        snapshot: service.snapshot(false),
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
            NetworkEffect::SetMobileBroadband { on } => self.switch(Radio::Mobile, on)?,
            NetworkEffect::ConnectNetwork { ssid, password } => self.connect(ssid, password)?,
            NetworkEffect::CancelConnect { ssid } => {
                if self.expanded.as_deref() == Some(ssid.as_str()) {
                    self.set_expanded(None);
                }
                Begun::Done
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
                self.notice = Some(format!("Sealing {} for your devices…", profile.name));
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
            NetworkEffect::SetVpn { vpn, on } => {
                let known = self
                    .vpns
                    .iter_mut()
                    .find(|known| known.name == vpn)
                    .ok_or_else(|| format!("No VPN is called {vpn}."))?;
                known.active = on;
                self.change(NetworkChange::Vpn { name: vpn, on }, None)
            }
            NetworkEffect::OpenVpnStudio { vpn } => {
                let intent = match vpn {
                    Some(profile) => ToolIntent::Vpn {
                        route: VpnRoute::Profile,
                        profile: Some(profile),
                    },
                    None => ToolIntent::Vpn {
                        route: VpnRoute::Overview,
                        profile: None,
                    },
                };
                Begun::work(move || Landed::Opened(service.open_tool(intent)))
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
                    Some(format!("Applying IP settings to {}…", profile.name)),
                )
            }
            NetworkEffect::ShowKnownNetwork { profile } => {
                let profile = self.profile(&profile)?;
                self.known_open = (self.known_open.as_deref() != Some(profile.uuid.as_str()))
                    .then_some(profile.uuid);
                Begun::Done
            }
            NetworkEffect::SetMetered { profile, on } => {
                let profile = self.profile(&profile)?;
                let metered = if on {
                    os_actions::MeteredPolicy::Metered
                } else {
                    os_actions::MeteredPolicy::Automatic
                };
                if let Some(settings) = self.profile_settings.get_mut(&profile.uuid) {
                    settings.metered = Some(metered);
                }
                if let Some(form) = self.policy_forms.get(&profile.uuid) {
                    form.metered.set(metered_shown(metered_id(metered)));
                }
                self.change(
                    NetworkChange::Policy {
                        uuid: profile.uuid,
                        name: profile.name.clone(),
                        patch: os_actions::ConnectionPolicyPatch {
                            metered: Some(metered),
                            mac: None,
                            proxy: None,
                        },
                    },
                    None,
                )
            }
            NetworkEffect::ShowHotspotSetup => {
                self.hotspot_open = !self.hotspot_open;
                Begun::Done
            }
            NetworkEffect::SetConnectionPolicy {
                profile,
                metered,
                mac,
                proxy,
            } => {
                let profile = self.profile(&profile)?;
                let patch = self.policy_patch(&profile, metered, mac, proxy)?;
                self.change(
                    NetworkChange::Policy {
                        uuid: profile.uuid,
                        name: profile.name.clone(),
                        patch,
                    },
                    Some(format!("Applying advanced policy to {}…", profile.name)),
                )
            }
            NetworkEffect::CreateMobileProfile { name, apn } => {
                let name = name.unwrap_or_else(|| self.mobile_name.get());
                let apn = apn.unwrap_or_else(|| self.mobile_apn.get());
                self.change(
                    NetworkChange::MobileProfile { name, apn },
                    Some("Creating mobile profile…".into()),
                )
            }
            NetworkEffect::UnlockSim { sim, pin } => {
                let typed = take_secret(&self.sim_pin);
                let pin = pin.map_or(typed, Zeroizing::new);
                let wanted = sim.unwrap_or_else(|| self.mobile_sim.get());
                let sim = self
                    .sims
                    .iter()
                    .find(|s| s.label == wanted || s.path == wanted)
                    .or_else(|| self.sims.first())
                    .map(|s| s.path.clone())
                    .ok_or_else(|| "No SIM is available".to_string())?;
                self.change(
                    NetworkChange::UnlockSim { sim, pin },
                    Some("Unlocking SIM…".into()),
                )
            }
            NetworkEffect::ShowEnterpriseSetup => {
                self.enterprise_open = !self.enterprise_open;
                Begun::Done
            }
            NetworkEffect::ChooseCaCertificate => {
                if self.ca_picker.is_none() {
                    self.ca_picker = Some(
                        ui::portal::PickRequest::open(
                            "Choose organization CA certificate",
                            "Choose",
                        )
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
                let (config, password) = self.enterprise_config(EnterpriseGiven {
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
                });
                self.change(
                    NetworkChange::Enterprise { config, password },
                    Some("Connecting to enterprise Wi-Fi…".into()),
                )
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
            Radio::Mobile => self.radios.wwan_enabled = on,
        }
        Ok(self.change(NetworkChange::Radio(radio, on), None))
    }

    /// Join `ssid`: with `password`, else with what its open prompt holds,
    /// else (a secure network) with a password the person's devices share.
    fn connect(&mut self, ssid: String, password: Option<String>) -> Result<Begun, String> {
        let network = self
            .networks
            .iter()
            .find(|network| network.ssid == ssid)
            .cloned()
            .ok_or_else(|| format!("{ssid} is not in range."))?;
        if self.connecting.contains(&ssid) {
            return Err(format!("Already connecting to {ssid}."));
        }
        let prompt_open = self.expanded.as_deref() == Some(ssid.as_str());
        let typed = prompt_open
            .then(|| {
                self.join_passwords
                    .get(&ssid)
                    .map(|buffer| take_secret(buffer))
            })
            .flatten();
        let password = password
            .map(Zeroizing::new)
            .or(typed)
            .filter(|password| !password.is_empty());
        let lookup_shared = network.secure && password.is_none() && !prompt_open;
        if !lookup_shared {
            self.set_expanded(None);
        }
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

    /// The change between `profile`'s policy and what is asked of it.
    fn policy_patch(
        &self,
        profile: &SavedNetworkProfile,
        metered: Option<String>,
        mac: Option<String>,
        proxy: Option<String>,
    ) -> Result<os_actions::ConnectionPolicyPatch, String> {
        let original = self
            .profile_settings
            .get(&profile.uuid)
            .cloned()
            .ok_or_else(|| t("net-notice-policy-unread"))?;
        let form = self.policy_forms.get(&profile.uuid);
        let metered = metered.or_else(|| form.map(|form| form.metered.get()));
        let mac = mac.or_else(|| form.map(|form| form.mac.get()));
        let proxy = proxy.or_else(|| form.map(|form| form.proxy.get()));
        let requested_metered = metered
            .as_deref()
            .and_then(metered_from_label)
            .unwrap_or_else(|| original.metered.unwrap_or_default());
        let requested_mac = mac.as_deref().and_then(mac_from_label).or(original.mac);
        let requested_proxy = original
            .proxy
            .as_ref()
            .map(|old| os_actions::ProxyConfiguration {
                automatic_url: proxy.and_then(non_blank),
                browser_only: old.browser_only,
            });
        Ok(os_actions::ConnectionPolicyPatch {
            metered: original
                .metered
                .filter(|old| *old != requested_metered)
                .map(|_| requested_metered),
            mac: match (original.mac, requested_mac) {
                (Some(old), Some(new)) if old != new => Some(new),
                _ => None,
            },
            proxy: match (&original.proxy, requested_proxy) {
                (Some(old), Some(new)) if old != &new => Some(new),
                _ => None,
            },
        })
    }

    /// The enterprise network `given` asks for, completed from the setup
    /// form, and its password; the form's password is zeroed.
    fn enterprise_config(
        &self,
        given: EnterpriseGiven,
    ) -> (EnterpriseWifiConfiguration, Zeroizing<String>) {
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
            password,
        } = given;
        let form = &self.enterprise;
        let typed = take_secret(&form.password);
        let config = EnterpriseWifiConfiguration {
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
        };
        (config, password.map_or(typed, Zeroizing::new))
    }

    /// Take in work that landed; what the effect answers.
    fn land(&mut self, landed: Landed) -> EffectAnswer {
        self.changed = true;
        let reported = match landed {
            Landed::Refreshed { snapshot, scan } => {
                self.refreshes_pending = self.refreshes_pending.saturating_sub(1);
                let adopted = self.adopt_snapshot(snapshot);
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
                reported
            }
            Landed::Connected { ssid, result } => self.joined(&ssid, result),
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
                    Ok("Network diagnostics completed.".into())
                }
                Err(error) => Err(format!("Diagnostics unavailable: {error}")),
            },
            Landed::SignedIn(result) => result
                .map(|()| String::new())
                .map_err(|error| format!("Could not open network sign-in: {error}")),
            // The tool answers for itself: neither outcome is a notice.
            Landed::Opened(result) => return result.map(|()| Effected::done()),
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
                    Radio::Mobile => self.radios.wwan_enabled = !on,
                }
                Err(t_args(
                    "net-notice-radio-err",
                    &[
                        ("radio", radio_shown(*radio).into()),
                        ("error", error.into()),
                    ],
                ))
            }
            (NetworkChange::Vpn { name, on }, result) => {
                if result.is_err()
                    && let Some(vpn) = self.vpns.iter_mut().find(|vpn| &vpn.name == name)
                {
                    vpn.active = !on;
                }
                result.map(|()| String::new())
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
            (NetworkChange::Ipv4 { name, .. }, Ok(())) => Ok(t_args(
                "net-notice-ip-ok",
                &[("name", name.as_str().into())],
            )),
            (NetworkChange::Ipv4 { .. }, Err(error)) => {
                Err(t_args("net-notice-ip-err", &[("error", error.into())]))
            }
            (NetworkChange::Policy { name, .. }, Ok(())) => Ok(t_args(
                "net-notice-policy-ok",
                &[("name", name.as_str().into())],
            )),
            (NetworkChange::Policy { .. }, Err(error)) => {
                Err(t_args("net-notice-policy-err", &[("error", error.into())]))
            }
            (NetworkChange::MobileProfile { .. }, result) => {
                Self::operation(result, "Mobile profile created.", "net-subject-mobile")
            }
            (NetworkChange::UnlockSim { .. }, result) => {
                Self::operation(result, "SIM unlocked.", "net-subject-sim")
            }
            (NetworkChange::Enterprise { .. }, result) => Self::operation(
                result,
                "Enterprise Wi-Fi connected.",
                "net-subject-enterprise",
            ),
            (NetworkChange::Connect { .. }, result) => result.map(|()| String::new()),
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
                self.set_expanded(None);
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
                self.set_expanded(Some(ssid.to_owned()));
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
                self.notice = Some("CA certificate selected.".into());
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

    /// A button invoking `effect`.
    fn button(&self, id: &str, label: &str, effect: NetworkEffect) -> Button {
        self.cx.bind_invocation(Button::new(id, label), effect)
    }

    /// `button`, sized to its label and pushed to the row's start.
    fn button_row(&self, id: &str, label: &str, effect: NetworkEffect) -> Item {
        row(Box::new(
            Row::new()
                .push(Box::new(self.button(id, label, effect)))
                .push(Box::new(Spacer::new())),
        ))
    }

    /// A switch showing `on`, invoking `set`.
    fn switch_control(
        &self,
        id: &str,
        on: bool,
        set: ui::tilo_resource::ActionRef<NetworkEffect, bool>,
    ) -> Toggle {
        self.cx.bind(
            Toggle::from_state(&LocalState::new(on)).with_id(id),
            effect(set),
        )
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if !self.loaded {
            let mut pending = vec![note(&t("net-pending"))];
            if let Some(notice) = &self.notice {
                pending.push(note(notice));
            }
            return crate::widgets::cards(vec![card(&t("net-pending-title"), rows(pending))]);
        }
        // What is happening now comes first; then Wi-Fi with what is in
        // range and what this device knows; the rest are their own groups,
        // short while unused.
        let mut cards = vec![self.status_section(), self.wifi_card()];
        if !self.wifi_absent() {
            cards.push(self.known_networks_card());
        }
        if self.wired.device.is_some() || self.read_errors.wired.is_some() {
            cards.push(self.wired_card());
        }
        cards.push(card(&t("net-vpn"), rows(self.vpn_rows())));
        if !self.wifi_absent() {
            cards.push(self.hotspot_card());
        }
        cards.push(self.airplane_card());
        if self.radios.wwan_available {
            cards.push(card_with_action(
                &t("net-mobile"),
                Box::new(self.switch_control(
                    "network.mobile",
                    self.radios.wwan_enabled,
                    NetworkEffect::SET_MOBILE_BROADBAND,
                )),
                rows(self.mobile_rows()),
            ));
        }
        cards.push(card(&t("net-diag-title"), rows(self.diagnostic_rows())));
        crate::widgets::cards(cards)
    }

    /// The one line on top: how this device is connected now, or that it is
    /// offline and why, with the action that fixes it.
    fn status_section(&self) -> SettingsSection {
        let (icon, title, detail, fix) = self.status_line();
        let control: El = match fix {
            Some((id, label, effect)) => Box::new(self.button(id, &label, effect)),
            None => Box::new(Spacer::new()),
        };
        crate::widgets::untitled(rows(vec![crate::widgets::icon_row(
            icon, &title, &detail, control,
        )]))
    }

    /// The status line's glyph, sentence, detail and fixing action.
    fn status_line(&self) -> StatusLine {
        use ui::icons::Icon;
        let connected = self
            .wifi_enabled
            .then(|| self.networks.iter().find(|network| network.connected))
            .flatten();
        let through = if self.wired.connected && self.wired.device.is_some() {
            Some((Icon::EthernetPort, t("net-now-wired")))
        } else {
            connected.map(|network| {
                (
                    Icon::Wifi,
                    t_args(
                        "net-connected-to",
                        &[("ssid", network.ssid.as_str().into())],
                    ),
                )
            })
        };
        if let Some((icon, title)) = through {
            return match self.connectivity {
                ConnectivityState::Portal => (
                    icon,
                    title,
                    t("net-now-portal"),
                    Some((
                        "settings.network.captive-portal",
                        t("net-open-signin"),
                        NetworkEffect::OpenNetworkSignIn,
                    )),
                ),
                ConnectivityState::Limited | ConnectivityState::Offline => {
                    (icon, title, t("net-now-no-internet"), None)
                }
                ConnectivityState::Unknown | ConnectivityState::Full => (
                    icon,
                    title,
                    t(if icon == Icon::EthernetPort {
                        "net-now-by-cable"
                    } else {
                        "net-now-by-wifi"
                    }),
                    None,
                ),
            };
        }
        let offline = t("net-now-offline");
        if self.radios.airplane_mode && self.read_errors.radios.is_none() {
            return (
                Icon::WifiOff,
                offline,
                t("net-now-airplane"),
                Some((
                    "settings.network.status.airplane",
                    t("net-turn-off-airplane"),
                    NetworkEffect::SetAirplaneMode { on: false },
                )),
            );
        }
        if self.wifi_absent() {
            let reason = if self.wired.device.is_some() {
                "net-now-plug-in"
            } else {
                "net-no-wifi-adapter"
            };
            return (Icon::WifiOff, offline, t(reason), None);
        }
        if self.radios.wifi_hardware_blocked {
            return (Icon::WifiOff, offline, t("net-wifi-hardware-switch"), None);
        }
        if !self.wifi_enabled {
            return (
                Icon::WifiOff,
                offline,
                t("net-wifi-off"),
                (self.read_errors.wifi_enabled.is_none()).then(|| {
                    (
                        "settings.network.status.wifi",
                        t("net-turn-on-wifi"),
                        NetworkEffect::SetWifi { on: true },
                    )
                }),
            );
        }
        (Icon::Wifi, offline, t("net-now-choose"), None)
    }

    /// Wi-Fi: the switch says whether the radio is on (the machine's state,
    /// read back after every change); under it the networks in range, the
    /// one in use first, each joined with a press (a password asked in
    /// place), and the way into a work or school network.
    fn wifi_card(&self) -> SettingsSection {
        let wifi_toggle =
            self.switch_control("network.wifi", self.wifi_enabled, NetworkEffect::SET_WIFI);
        // An unread radio state, or a radio a switch holds off, is not
        // offered as a switch position.
        let usable = self.read_errors.wifi_enabled.is_none()
            && !self.wifi_absent()
            && !self.radios.wifi_hardware_blocked;
        let wifi_toggle: El = Box::new(wifi_toggle.with_enabled(usable));

        let mut wifi_rows: Vec<Item> = Vec::new();
        if self.wifi_absent() {
            wifi_rows.push(note(&t("net-no-wifi-adapter")));
        } else if let Some(error) = self.wifi_status_error() {
            wifi_rows.push(error_row(&t("net-status"), &error));
        } else if !self.wifi_enabled {
            wifi_rows.push(note(&t(if self.radios.wifi_hardware_blocked {
                "net-wifi-hardware-switch"
            } else {
                "net-wifi-off"
            })));
        }
        if let Some(notice) = &self.notice {
            wifi_rows.push(note(notice));
        }
        if self.wifi_enabled && !self.wifi_absent() {
            self.open_rows();
            for net in self.networks_in_order() {
                wifi_rows.push(self.network_row(net));
            }
            if self.networks.is_empty() && self.read_errors.networks.is_none() {
                wifi_rows.push(note(&t(if self.scan_in_flight {
                    "net-scanning"
                } else {
                    "net-none-in-range"
                })));
            }
            wifi_rows.extend(self.enterprise_entry());
        }
        if self.in_flight > 0 {
            wifi_rows.push(note(&t("net-applying")));
        }
        card_with_action(&t("net-wifi"), wifi_toggle, rows(wifi_rows))
    }

    /// The networks in range as the list shows them: the one in use, then
    /// the ones this device knows, then by signal, then by name.
    fn networks_in_order(&self) -> Vec<&WifiNetworkInfo> {
        let mut order: Vec<&WifiNetworkInfo> = self.networks.iter().collect();
        order.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then(b.known.cmp(&a.known))
                .then(b.signal.cmp(&a.signal))
                .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
        });
        order
    }

    /// "Connect to a work or school network…" under the networks in range,
    /// opening the enterprise setup in place.
    fn enterprise_entry(&self) -> Vec<Item> {
        let mut out = vec![crate::widgets::icon_row(
            ui::icons::Icon::Landmark,
            &t("net-enterprise-entry"),
            &t("net-enterprise-entry-detail"),
            Box::new(self.disclosure_button(
                "settings.network.enterprise.editor",
                &t("net-enterprise-entry"),
                self.enterprise_open,
                NetworkEffect::ShowEnterpriseSetup,
            )),
        )];
        if self.enterprise_open {
            out.extend(self.enterprise_rows());
        }
        out
    }

    /// A quiet chevron button that opens or closes something in place.
    fn disclosure_button(&self, id: &str, name: &str, open: bool, effect: NetworkEffect) -> Button {
        self.cx.bind_invocation(
            Button::icon_only(
                id,
                if open {
                    ui::icons::Icon::ChevronDown
                } else {
                    ui::icons::Icon::ChevronRight
                },
                name,
            )
            .role(ui::widget::ButtonRole::Plain),
            effect,
        )
    }

    /// Wired card. `NetworkManager` applies the switch to this interface
    /// only; Wi-Fi and VPN stay untouched.
    fn wired_card(&self) -> SettingsSection {
        if let Some(error) = &self.read_errors.wired {
            return card(
                &t("net-wired"),
                rows(vec![row(error_caption(&t_args(
                    "net-wired-error",
                    &[("error", error.as_str().into())],
                )))]),
            );
        }
        if self.wired.device.is_some() {
            card_with_action(
                &t("net-wired"),
                Box::new(self.switch_control(
                    "network.wired",
                    self.wired.connected,
                    NetworkEffect::SET_WIRED,
                )),
                rows(self.wired_rows()),
            )
        } else {
            card(&t("net-wired"), rows(self.wired_rows()))
        }
    }

    fn airplane_card(&self) -> SettingsSection {
        let mut content = Vec::new();
        if let Some(error) = &self.read_errors.radios {
            content.push(row(error_caption(&t_args(
                "net-radios-error",
                &[("error", error.as_str().into())],
            ))));
        }
        content.push(note(&t("net-airplane-caption")));
        card_with_action(
            &t("net-airplane"),
            Box::new(
                self.switch_control(
                    "network.airplane",
                    self.radios.airplane_mode,
                    NetworkEffect::SET_AIRPLANE_MODE,
                )
                .with_enabled(self.read_errors.radios.is_none()),
            ),
            rows(content),
        )
    }

    /// The hotspot's on/off state, or why the radios could not be read.
    fn hotspot_status_row(&self) -> Item {
        let status = t("net-status");
        if let Some(error) = &self.read_errors.radios {
            return error_row(
                &status,
                &t_args("net-radios-error", &[("error", error.as_str().into())]),
            );
        }
        let value = if self.radios.hotspot_active {
            t("net-hotspot-active")
        } else {
            t("net-hotspot-off")
        };
        value_row(&status, &value)
    }

    /// Rows for the hotspot card: name/password/band, the start and QR
    /// actions, and (when active) the QR code and connected devices.
    fn hotspot_rows(&self) -> Vec<Item> {
        let hotspot_secret = self.hotspot_password.lock().map_or_else(
            |_| Zeroizing::new(String::new()),
            |value| Zeroizing::new(value.clone()),
        );
        let mut hotspot_rows: Vec<Item> = vec![
            self.hotspot_status_row(),
            field_row(
                &t("net-hotspot-name"),
                Box::new(
                    TextInput::from_state(&self.hotspot_name)
                        .with_id("network.hotspot.ssid")
                        .with_placeholder(t("net-hotspot-name")),
                ),
            ),
            field_row(
                &t("net-hotspot-password"),
                Box::new(
                    TextInput::local()
                        .with_id("network.hotspot.password")
                        .with_external_buffer(Arc::clone(&self.hotspot_password))
                        .with_initial_value(&hotspot_secret)
                        .with_secret(true)
                        .with_placeholder(t("net-hotspot-password")),
                ),
            ),
            setting_row(
                &t("net-band"),
                Box::new(
                    Dropdown::from_state(
                        HOTSPOT_BANDS.iter().map(|band| band_shown(band)).collect(),
                        &self.hotspot_band,
                    )
                    .with_id("network.hotspot.band")
                    .with_placeholder(&self.hotspot_band.get()),
                ),
            ),
            row(Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "settings.network.hotspot.apply",
                        &t("net-apply-start"),
                        NetworkEffect::StartHotspot {
                            name: None,
                            password: None,
                            band: None,
                        },
                    )))
                    .push(Box::new(self.button(
                        "settings.network.hotspot.qr",
                        &if self.hotspot_code_shown {
                            t("net-hide-qr")
                        } else {
                            t("net-share-qr")
                        },
                        NetworkEffect::ShowHotspotCode,
                    )))
                    .push(Box::new(Spacer::new())),
            )),
        ];
        if self.hotspot_code_shown
            && let Ok(payload) = self
                .hotspot(None, None, None)
                .and_then(|config| config.qr_payload())
        {
            hotspot_rows.push(block(Box::new(QrWidget::new(payload.into_bytes()))));
            hotspot_rows.push(note(&t("net-qr-caption")));
        }
        if let Some(error) = &self.read_errors.hotspot_clients
            && self.radios.hotspot_active
        {
            hotspot_rows.push(row(error_caption(&t_args(
                "net-hotspot-clients-error",
                &[("error", error.as_str().into())],
            ))));
        } else if self.radios.hotspot_active {
            let clients = t("net-hotspot-clients");
            hotspot_rows.push(value_row(&clients, &self.hotspot_clients.len().to_string()));
            for client in &self.hotspot_clients {
                let signal = client
                    .signal_dbm
                    .map_or_else(|| t("net-signal-unknown"), |dbm| format!("{dbm} dBm"));
                hotspot_rows.push(value_row(&client.mac, &signal));
            }
            hotspot_rows.push(note(&t("net-hotspot-clients-caption")));
        }
        hotspot_rows
    }

    /// The hotspot: its switch, and its setup only while it runs or is
    /// opened.
    fn hotspot_card(&self) -> SettingsSection {
        let mut content = vec![setting_row(
            &t("net-hotspot-setup"),
            Box::new(self.disclosure_button(
                "settings.network.hotspot.setup",
                &t("net-hotspot-setup"),
                self.hotspot_open || self.radios.hotspot_active,
                NetworkEffect::ShowHotspotSetup,
            )),
        )];
        if self.hotspot_open || self.radios.hotspot_active {
            content.extend(self.hotspot_rows());
        }
        card_with_action(
            &t("net-hotspot"),
            Box::new(self.switch_control(
                "network.hotspot",
                self.radios.hotspot_active,
                NetworkEffect::SET_HOTSPOT,
            )),
            rows(content),
        )
    }

    /// Known networks: one row each, opening its details in place.
    fn known_networks_card(&self) -> SettingsSection {
        card(
            &t("net-known"),
            rows(match &self.read_errors.profiles {
                Some(error) => vec![row(error_caption(&t_args(
                    "net-profiles-error",
                    &[("error", error.as_str().into())],
                )))],
                None => self.known_network_rows(),
            }),
        )
    }

    fn known_network_rows(&self) -> Vec<Item> {
        let wifi_profiles: Vec<&SavedNetworkProfile> = self
            .profiles
            .iter()
            .filter(|profile| profile.kind == "802-11-wireless")
            .collect();
        if wifi_profiles.is_empty() {
            return vec![note(&t("net-no-saved-wifi"))];
        }
        // Each known network opens its details in place; the row stays open
        // while the page is rebuilt.
        self.open_rows();
        wifi_profiles
            .into_iter()
            .map(|profile| {
                let details = crate::widgets::page(vec![crate::widgets::untitled(rows(
                    self.known_network_details(profile),
                ))]);
                row(Box::new(
                    DisclosureRow::keyed(
                        profile.name.clone(),
                        details,
                        &self.known_rows,
                        &profile.uuid,
                    )
                    .icon(ui::icons::Icon::Wifi)
                    .secondary(t(if profile.autoconnect {
                        "net-known-joins-itself"
                    } else {
                        "net-known-joins-when-chosen"
                    })),
                ))
            })
            .collect()
    }

    /// A known network's details: joining it by itself, metered data, its
    /// wake, sharing it with the person's other devices, and forgetting it.
    fn known_network_details(&self, profile: &SavedNetworkProfile) -> Vec<Item> {
        let uuid = profile.uuid.clone();
        let mut out = vec![setting_row(
            &t("net-autoconnect"),
            Box::new(
                self.cx.bind(
                    Toggle::from_state(&LocalState::new(profile.autoconnect))
                        .with_id(&format!("network.profile.autoconnect.{}", profile.uuid)),
                    effect(NetworkEffect::SET_AUTOCONNECT).map(move |on: bool| (uuid.clone(), on)),
                ),
            ),
        )];
        if let Some(settings) = self.profile_settings.get(&profile.uuid) {
            let uuid = profile.uuid.clone();
            out.push(setting_row_described(
                &t("net-metered-connection"),
                &t("net-metered-caption"),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(
                            settings.metered == Some(os_actions::MeteredPolicy::Metered),
                        ))
                        .with_id(&format!("network.profile.metered.{}", profile.uuid)),
                        effect(NetworkEffect::SET_METERED).map(move |on: bool| (uuid.clone(), on)),
                    ),
                ),
            ));
        }
        out.extend(self.wake_rows(profile));
        out.push(setting_row_described(
            &t("net-share-devices"),
            &t("net-share-caption"),
            Box::new(self.button(
                &format!("settings.network.profile.share.{}", profile.uuid),
                &t("net-share"),
                NetworkEffect::ShareNetwork {
                    profile: profile.uuid.clone(),
                },
            )),
        ));
        out.push(setting_row(
            &t("net-forget-network"),
            Box::new(
                self.button(
                    &format!("settings.network.profile.forget.{}", profile.uuid),
                    &t("net-forget"),
                    NetworkEffect::ForgetNetwork {
                        profile: profile.uuid.clone(),
                    },
                )
                .role(ui::widget::ButtonRole::Destructive),
            ),
        ));
        out
    }

    fn wake_rows(&self, profile: &SavedNetworkProfile) -> Vec<Item> {
        let label = t_args(
            "net-wake-network",
            &[("name", profile.name.as_str().into())],
        );
        match self.wake_settings.get(&profile.uuid) {
            Some(Ok(status)) => {
                let uuid = profile.uuid.clone();
                let toggle = self.cx.bind(
                    Toggle::from_state(&LocalState::new(status.configured))
                        .with_id(&format!("network.profile.wake.{}", profile.uuid))
                        .with_accessible_name(&label)
                        .with_enabled(status.configured || status.supported == Some(true)),
                    effect(NetworkEffect::SET_NETWORK_WAKE).map(move |on: bool| (uuid.clone(), on)),
                );
                let description = wake_description(status);
                let mut rows = vec![setting_row_described(
                    &label,
                    &t(description),
                    Box::new(toggle),
                )];
                if let Some(problem) = &status.problem {
                    rows.push(row(error_caption(problem)));
                }
                rows
            }
            Some(Err(error)) => vec![
                value_row(&label, &t("net-wake-unavailable")),
                row(error_caption(error)),
            ],
            None => vec![value_row(&label, &t("net-wake-unavailable"))],
        }
    }

    fn diagnostic_rows(&self) -> Vec<Item> {
        let mut out: Vec<Item> = vec![setting_row_described(
            &t("net-diag-check"),
            &t("net-diag-check-caption"),
            Box::new(self.button(
                "settings.network.diagnostics",
                &t("net-run-diagnostics"),
                NetworkEffect::RunDiagnostics,
            )),
        )];
        if let Some(d) = &self.diagnostics {
            out.push(value_row("NetworkManager", &d.network_manager_state));
            out.push(value_row(
                &t("net-diag-internet"),
                &connectivity_shown(d.connectivity),
            ));
            if let Some(v) = &d.device {
                out.push(value_row(&t("net-diag-device"), v));
            }
            if let Some(v) = &d.address {
                out.push(value_row(&t("net-diag-address"), v));
            }
            if let Some(v) = &d.gateway {
                out.push(value_row(&t("net-diag-gateway"), v));
            }
            let dns = if d.dns.is_empty() {
                t("net-diag-dns-none")
            } else {
                d.dns.join(", ")
            };
            out.push(value_row(&t("net-diag-dns"), &dns));
            for recommendation in &d.recommendations {
                out.push(note(recommendation));
            }
        } else {
            out.push(note(&t("net-diag-idle")));
        }
        out
    }

    /// A masked field over `buffer`, which never leaves this process.
    fn secret_field(buffer: &Arc<Mutex<String>>, id: &str, placeholder: &str) -> Item {
        let initial = buffer.lock().map_or_else(
            |_| Zeroizing::new(String::new()),
            |value| Zeroizing::new(value.clone()),
        );
        field_row(
            placeholder,
            Box::new(
                TextInput::local()
                    .with_id(id)
                    .with_external_buffer(Arc::clone(buffer))
                    .with_initial_value(&initial)
                    .with_secret(true)
                    .with_placeholder(placeholder),
            ),
        )
    }

    /// A text field over `state`.
    fn text_field(state: &LocalState<String>, id: &str, placeholder: &str) -> Item {
        field_row(
            placeholder,
            Box::new(
                TextInput::from_state(state)
                    .with_id(id)
                    .with_placeholder(placeholder),
            ),
        )
    }

    fn mobile_rows(&self) -> Vec<Item> {
        let mut rows = Vec::new();
        if let Some(error) = &self.read_errors.sims {
            rows.push(row(error_caption(&t_args(
                "net-sims-error",
                &[("error", error.as_str().into())],
            ))));
        }
        rows.extend([
            note(&t("net-mobile-caption")),
            Self::text_field(
                &self.mobile_name,
                "network.mobile.profile",
                &t("net-profile-name"),
            ),
            Self::text_field(&self.mobile_apn, "network.mobile.apn", &t("net-apn")),
            self.button_row(
                "settings.network.mobile.create",
                &t("net-create-profile"),
                NetworkEffect::CreateMobileProfile {
                    name: None,
                    apn: None,
                },
            ),
            note(&t("net-sim-pin-caption")),
        ]);
        if !self.sims.is_empty() {
            rows.push(setting_row(
                &t("net-sim"),
                Box::new(
                    Dropdown::from_state(
                        self.sims.iter().map(|s| s.label.clone()).collect(),
                        &self.mobile_sim,
                    )
                    .with_placeholder(&self.mobile_sim.get()),
                ),
            ));
        }
        rows.extend([
            Self::secret_field(&self.sim_pin, "network.mobile.pin", &t("net-sim-pin")),
            self.button_row(
                "settings.network.mobile.unlock",
                &t("net-unlock-sim"),
                NetworkEffect::UnlockSim {
                    sim: None,
                    pin: None,
                },
            ),
        ]);
        rows
    }

    fn enterprise_rows(&self) -> Vec<Item> {
        let form = &self.enterprise;
        vec![
            setting_row(
                &t("net-method"),
                Box::new(
                    Dropdown::from_state(
                        vec!["PEAP".into(), "TTLS".into(), "TLS".into()],
                        &form.method,
                    )
                    .with_placeholder(&form.method.get()),
                ),
            ),
            setting_row(
                &t("net-inner-auth"),
                Box::new(
                    Dropdown::from_state(
                        vec!["MSCHAPv2".into(), "PAP".into(), "CHAP".into()],
                        &form.inner_auth,
                    )
                    .with_placeholder(&form.inner_auth.get()),
                ),
            ),
            Self::text_field(
                &form.profile,
                "network.enterprise.profile",
                &t("net-profile-name"),
            ),
            Self::text_field(
                &form.client_certificate,
                "network.enterprise.client_cert",
                &t("net-client-cert"),
            ),
            Self::text_field(
                &form.private_key,
                "network.enterprise.private_key",
                &t("net-private-key"),
            ),
            Self::text_field(&form.ssid, "network.enterprise.ssid", &t("net-ssid")),
            Self::text_field(
                &form.identity,
                "network.enterprise.identity",
                &t("net-identity"),
            ),
            Self::text_field(
                &form.anonymous,
                "network.enterprise.anonymous",
                &t("net-anonymous"),
            ),
            Self::text_field(&form.ca, "network.enterprise.ca", &t("net-ca-path")),
            self.button_row(
                "settings.network.enterprise.ca.choose",
                &t("net-choose-ca"),
                NetworkEffect::ChooseCaCertificate,
            ),
            Self::secret_field(
                &form.password,
                "network.enterprise.password",
                &t("net-password"),
            ),
            self.button_row(
                "settings.network.enterprise.connect",
                &t("net-save-connect"),
                NetworkEffect::ConnectEnterpriseNetwork {
                    profile: None,
                    ssid: None,
                    identity: None,
                    anonymous_identity: None,
                    ca_certificate: None,
                    method: None,
                    inner_auth: None,
                    client_certificate: None,
                    private_key: None,
                    password: None,
                },
            ),
        ]
    }

    /// Keep specialist 802.1X credentials out of the ordinary connection
    /// view. The affordance remains visible, while the form appears only when
    /// someone has chosen to add that kind of network.
    /// Open the rows the model asks for (a password a join needs, the
    /// details an agent asked for) once each time it asks.
    fn open_rows(&self) {
        let mut shown = self.prompt_shown.borrow_mut();
        if *shown != self.expanded {
            match &self.expanded {
                Some(ssid) => self.prompts.open(ssid),
                None => {
                    if let Some(old) = shown.as_deref() {
                        self.prompts.close(old);
                    }
                }
            }
            shown.clone_from(&self.expanded);
        }
        let mut shown = self.known_shown.borrow_mut();
        if *shown != self.known_open {
            match &self.known_open {
                Some(uuid) => self.known_rows.open(uuid),
                None => {
                    if let Some(old) = shown.as_deref() {
                        self.known_rows.close(old);
                    }
                }
            }
            shown.clone_from(&self.known_open);
        }
    }

    /// Follow what the person opened and closed in place: closing a
    /// network's password prompt cancels joining it, and the details open
    /// are the ones the model says are open.
    fn follow_rows(&mut self) {
        if let Some(ssid) = self.expanded.clone()
            && self.prompt_shown.borrow().as_deref() == Some(ssid.as_str())
            && self.prompts.open_key().as_deref() != Some(ssid.as_str())
        {
            *self.prompt_shown.borrow_mut() = None;
            self.set_expanded(None);
        }
        let open = self.known_rows.open_key();
        if open != self.known_open && *self.known_shown.borrow() == self.known_open {
            self.known_open.clone_from(&open);
            *self.known_shown.borrow_mut() = open;
        }
    }

    /// One network in range: its name, whether it is secured and how
    /// strong it is, and its state on the trailing edge. Choosing it joins
    /// it; a password it needs is asked for in place, under it.
    fn network_row(&self, net: &WifiNetworkInfo) -> Item {
        let security = t(if net.secure {
            "net-security-secured"
        } else {
            "net-security-open"
        });
        let detail = format!("{security}, {}", wifi_signal_words(net.signal));
        let id = format!("settings.network.connect.{}", net.ssid);
        if self.prompt_shown.borrow().as_deref() == Some(net.ssid.as_str())
            || self.prompts.open_key().as_deref() == Some(net.ssid.as_str())
        {
            return row(Box::new(
                DisclosureRow::keyed(
                    net.ssid.clone(),
                    self.password_prompt(&net.ssid),
                    &self.prompts,
                    &net.ssid,
                )
                .icon(ui::icons::Icon::Wifi)
                .secondary(detail),
            ));
        }
        let invocation_live = network_live(net);
        let mut line = ListRow::new(id, net.ssid.clone())
            .icon(ui::icons::Icon::Wifi)
            .secondary(detail);
        if net.connected {
            line = line.value(t("net-connected"));
        } else if self.connecting.contains(&net.ssid) {
            line = line.value(t("net-connecting"));
        } else {
            line = match self.cx.row_invocation(
                &invocation_live,
                NetworkEffect::ConnectNetwork {
                    ssid: net.ssid.clone(),
                    password: None,
                },
            ) {
                Some(binding) => line.on_action(binding),
                None => line.with_enabled(false),
            };
        }
        row(Box::new(line))
    }

    /// The password prompt under a secure network: a masked, process-local
    /// field and Join beside it. Closing the row cancels the join.
    fn password_prompt(&self, ssid: &str) -> Box<dyn Widget> {
        let buffer = self
            .join_passwords
            .get(ssid)
            .cloned()
            .unwrap_or_else(|| Arc::new(Mutex::new(String::new())));
        let field = TextInput::local()
            .with_id(&format!("settings.network.password.{ssid}"))
            .with_external_buffer(buffer)
            .with_secret(true)
            .with_placeholder(t("net-password"));
        Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_padding(0.0, crate::widgets::spacing().s2)
                .push(Box::new(
                    ui::widget::Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(field)),
                ))
                .push(Box::new({
                    let join =
                        Button::new(&format!("settings.network.join.{ssid}"), &t("net-join"))
                            .role(ui::widget::ButtonRole::Primary);
                    match self.networks.iter().find(|net| net.ssid == ssid) {
                        // Joining is the network row's own action.
                        Some(net) => self.cx.bind_row(
                            join,
                            &network_live(net),
                            NetworkEffect::ConnectNetwork {
                                ssid: ssid.to_owned(),
                                password: None,
                            },
                        ),
                        None => join.with_enabled(false),
                    }
                })),
        )
    }

    /// Rows for the Wired card: the link, and the selected wired profile's
    /// IPv4 settings.
    fn wired_rows(&self) -> Vec<Item> {
        if !self.wired.connected {
            return vec![
                value_row(&t("net-ethernet"), &t("net-not-connected")),
                note(&t("net-wired-empty")),
            ];
        }
        let mut out = Vec::new();
        let status = match &self.wired.speed {
            Some(s) => t_args("net-connected-speed", &[("speed", s.as_str().into())]),
            None => t("net-connected"),
        };
        out.push(value_row(&t("net-ethernet"), &status));
        if let Some(dev) = &self.wired.device {
            out.push(value_row(&t("net-interface"), dev));
        }
        out.push(value_row(
            &t("net-ipv4"),
            self.wired.ipv4.as_deref().unwrap_or("—"),
        ));
        let wired_profiles: Vec<&SavedNetworkProfile> = self
            .profiles
            .iter()
            .filter(|p| p.kind == "802-3-ethernet")
            .collect();
        let Some(profile) = Self::selected(&wired_profiles, &self.wired_profile) else {
            return out;
        };
        if wired_profiles.len() > 1 {
            out.push(setting_row(
                &t("net-edit-profile"),
                Box::new(
                    Dropdown::from_state(
                        wired_profiles.iter().map(|p| profile_label(p)).collect(),
                        &self.wired_profile,
                    )
                    .with_placeholder(&profile_label(profile)),
                ),
            ));
        }
        let Some(form) = self.ipv4_forms.get(&profile.uuid) else {
            out.push(note(&t("net-ipv4-unsupported")));
            return out;
        };
        out.extend([
            setting_row(
                &t("net-auto-ipv4"),
                Box::new(
                    Toggle::from_state(&form.automatic).with_id("settings.network.ipv4.automatic"),
                ),
            ),
            Self::text_field(
                &form.addresses,
                "network.ipv4.address",
                &t("net-address-prefix"),
            ),
            Self::text_field(&form.gateway, "network.ipv4.gateway", &t("net-gateway")),
            Self::text_field(&form.dns, "network.ipv4.dns", &t("net-dns-servers")),
            self.button_row(
                "settings.network.ipv4.apply",
                &t("net-apply-ip"),
                NetworkEffect::SetIpv4 {
                    profile: profile.uuid.clone(),
                    automatic: None,
                    addresses: None,
                    gateway: None,
                    dns: None,
                },
            ),
            note(&t("net-ipv4-caption")),
        ]);
        out
    }

    fn vpn_rows(&self) -> Vec<Item> {
        if let Some(error) = &self.read_errors.vpns {
            return vec![row(error_caption(&t_args(
                "net-vpn-error",
                &[("error", error.as_str().into())],
            )))];
        }
        if self.vpns.is_empty() {
            // Unused, the group is one line: where a VPN is added.
            return vec![setting_row_described(
                &t("net-vpn-add"),
                &t("net-vpn-empty"),
                Box::new(self.button(
                    "settings.network.vpn.studio.open",
                    &t("net-open-vpn-studio"),
                    NetworkEffect::OpenVpnStudio { vpn: None },
                )),
            )];
        }
        let mut out: Vec<Item> = Vec::new();
        for vpn in &self.vpns {
            let name = vpn.name.clone();
            out.push(setting_row(
                &vpn.name,
                Box::new(
                    Row::new()
                        .no_wrap()
                        .with_rhythm(Rhythm::Related)
                        .push(Box::new(self.button(
                            &format!("settings.network.vpn.studio.{}", vpn.name),
                            &t("net-open-vpn-studio"),
                            NetworkEffect::OpenVpnStudio {
                                vpn: Some(vpn.name.clone()),
                            },
                        )))
                        .push(Box::new(
                            self.cx.bind(
                                Toggle::from_state(&LocalState::new(vpn.active))
                                    .with_id(&format!("settings.network.vpn.{}", vpn.name)),
                                effect(NetworkEffect::SET_VPN)
                                    .map(move |on: bool| (name.clone(), on)),
                            ),
                        )),
                ),
            ));
        }
        out.push(note(&t("net-vpn-caption")));
        out
    }
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
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_background();
        self.scan_when_due(ui::time::now());
        self.follow_rows();
        let selection = (self.wifi_profile.get(), self.wired_profile.get());
        if selection != self.seen_selection {
            self.seen_selection = selection;
            self.changed = true;
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "NetworkPane"
    }

    fn next_wake(&self) -> Option<ui::time::Instant> {
        self.scans_networks()
            .then(|| self.scanned_at?.checked_add(SCAN_EVERY))
            .flatten()
    }
}

impl NetworkModel {
    /// Whether the page looks for networks by itself: Wi-Fi is on and the
    /// device has answered.
    fn scans_networks(&self) -> bool {
        self.loaded && self.wifi_enabled && !self.wifi_absent()
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

    /// Take in a snapshot or a chosen certificate that arrived.
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
        self.poll_picker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_pane_relabels_retained_controls_when_locale_changes() -> Result<(), String> {
        use crate::panes::model::ModelPane;
        use std::cell::RefCell;
        use ui::headless::{Framed, viewport};
        use ui::i18n::I18nManager;

        let fake = Arc::new(FakeNetwork::default());
        let network = model(&fake);
        let _ = t("net-run-diagnostics");
        I18nManager::set_locale("en-US");
        let root = ModelPane::new(Rc::new(RefCell::new(network)));
        let mut app = Framed::new(
            ui::State::new(),
            root,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        let host = Framed::harness_mut(&mut app);
        for (locale, expected) in [
            ("en-US", "Run diagnostics"),
            ("de-DE", "Diagnose ausführen"),
            ("en-US", "Run diagnostics"),
        ] {
            I18nManager::set_locale(locale);
            host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
                .map_err(|error| error.to_string())?;
            let name = ui::widget::find_test_id(host.root_widget(), "settings.network.diagnostics")
                .and_then(Widget::accessibility)
                .and_then(|node| node.name);
            if name.as_deref() != Some(expected) {
                return Err(format!(
                    "the actual network diagnostics control must follow {locale}: {name:?}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn network_editor_retains_caret_through_window_resize() -> Result<(), String> {
        use crate::panes::model::ModelPane;
        use std::cell::RefCell;
        use ui::headless::{Framed, viewport};

        let fake = Arc::new(FakeNetwork::default());
        let mut network = model(&fake);
        network.enterprise_open = true;
        let profile = network.enterprise.profile.clone();
        let root = ModelPane::new(Rc::new(RefCell::new(network)));
        let mut app = Framed::new(
            ui::State::new(),
            root,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        let host = Framed::harness_mut(&mut app);
        host.click_named("network.enterprise.profile")?;
        host.type_text("abcd");
        if profile.get() != "abcd" {
            return Err("the real network profile editor must accept typing".into());
        }
        host.key("Left");
        host.key("Left");
        host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
            .map_err(|error| error.to_string())?;
        host.type_text("X");
        if profile.get() != "abXcd" {
            return Err("resizing must retain the network editor's actual caret and focus".into());
        }
        Ok(())
    }

    #[test]
    fn network_pane_adopts_model_changes_after_layout_reuse() -> Result<(), String> {
        use crate::panes::model::ModelPane;
        use std::cell::RefCell;
        use ui::headless::{Framed, viewport};

        let fake = Arc::new(FakeNetwork::default());
        let network = Rc::new(RefCell::new(model(&fake)));
        let root = ModelPane::new(Rc::clone(&network));
        let mut app = Framed::new(
            ui::State::new(),
            root,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        let host = Framed::harness_mut(&mut app);
        host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
            .map_err(|error| error.to_string())?;
        {
            let mut model = network.borrow_mut();
            model.enterprise_open = true;
            model.changed = true;
        }
        host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
            .map_err(|error| error.to_string())?;
        host.click_named("network.enterprise.profile")?;
        host.type_text("New profile");
        if network.borrow().enterprise.profile.get() != "New profile" {
            return Err("a changed model must expose its newly composed controls".into());
        }
        Ok(())
    }

    #[test]
    fn network_picker_remains_open_through_window_resize() -> Result<(), String> {
        use crate::panes::model::ModelPane;
        use std::cell::RefCell;
        use ui::headless::{Framed, viewport};

        fn expanded(host: &ui::headless::FrameHarness) -> Result<bool, String> {
            ui::widget::find_test_id(host.root_widget(), "network.hotspot.band")
                .and_then(Widget::accessibility)
                .and_then(|node| node.state.expanded)
                .ok_or_else(|| "the actual band picker must expose its expanded state".into())
        }
        let fake = Arc::new(FakeNetwork::default());
        let mut network = model(&fake);
        // The band is in the hotspot's setup, open.
        network.hotspot_open = true;
        let root = ModelPane::new(Rc::new(RefCell::new(network)));
        let mut app = Framed::new(
            ui::State::new(),
            root,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        let host = Framed::harness_mut(&mut app);
        host.click_named("network.hotspot.band")?;
        if !expanded(host)? {
            return Err("the real network band picker must open".into());
        }
        host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
            .map_err(|error| error.to_string())?;
        if !expanded(host)? {
            return Err("resizing must retain the open network picker".into());
        }
        Ok(())
    }

    struct SnapshotGate {
        started: crate::channel::Sender<()>,
        release: crate::channel::Receiver<()>,
    }

    struct ReleaseSnapshot(Option<crate::channel::Sender<()>>);

    impl Drop for ReleaseSnapshot {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn pending_network() -> Result<(Shared<NetworkModel>, ReleaseSnapshot), String> {
        let (started, starts) = crate::channel::channel();
        let (release, releases) = crate::channel::channel();
        let fake = Arc::new(FakeNetwork::default());
        *fake.snapshot_gate.lock().unwrap() = Some(SnapshotGate {
            started,
            release: releases,
        });
        let profile = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut network =
            NetworkModel::with_service(PaneCx::in_file(profile.path().join("settings.json")), fake);
        let release = ReleaseSnapshot(Some(release));
        network.start_snapshot();
        starts
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("the actual network read must start: {error}"))?;
        Ok((Rc::new(std::cell::RefCell::new(network)), release))
    }

    #[test]
    fn pending_network_read_does_not_animate_the_pane() -> Result<(), String> {
        use crate::panes::model::ModelPane;
        use ui::headless::{Framed, viewport};

        let (network, _release) = pending_network()?;
        let pane = ModelPane::new(network);
        let app = Framed::new(
            ui::State::new(),
            pane,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        if Framed::harness(&app).frame_schedule().needs_animation_frame {
            return Err(
                "waiting for a network read must not request continuous animation frames".into(),
            );
        }
        Ok(())
    }

    #[test]
    fn completed_network_read_wakes_the_owner_and_updates_the_pane() -> Result<(), String> {
        use crate::panes::model::ModelPane;
        use ui::headless::{Framed, viewport};

        let mut scope = ui::WakeScope::new();
        let (woke, wakes) = crate::channel::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .map_err(|error| error.to_string())?;
        let _entry = scope.enter();
        let (network, release) = pending_network()?;
        let pane = ModelPane::new(Rc::clone(&network));
        let mut app = Framed::new(
            ui::State::new(),
            pane,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        drop(release);
        wakes
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| {
                format!("a completed network read must wake its real owner: {error}")
            })?;
        Framed::harness_mut(&mut app)
            .relayout_at(viewport((1200.0, 1800.0)))
            .map_err(|error| error.to_string())?;
        if !network.borrow().loaded {
            return Err("the awakened production pane must adopt its completed snapshot".into());
        }
        Ok(())
    }

    /// A device whose `NetworkManager` answers from what the test sets,
    /// recording each change.
    #[derive(Default)]
    struct FakeNetwork {
        networks: Mutex<Vec<WifiNetworkInfo>>,
        unreadable: Mutex<Option<String>>,
        refuse: Mutex<Option<String>>,
        shared_password: Mutex<Option<String>>,
        changes: Mutex<Vec<String>>,
        continuity: Mutex<Vec<WifiContinuityCommand>>,
        vpns: Mutex<Vec<VpnInfo>>,
        profiles: Mutex<Vec<SavedNetworkProfile>>,
        snapshot_gate: Mutex<Option<SnapshotGate>>,
        snapshot_reads: std::sync::atomic::AtomicUsize,
        connectivity_rechecks: std::sync::atomic::AtomicUsize,
        /// Scans, each recorded with the snapshot reads made before it.
        scans: Mutex<Vec<usize>>,
        scan_refusal: Mutex<Option<String>>,
    }

    impl NetworkService for FakeNetwork {
        /// The periodic look reads only the networks, not a whole snapshot.
        fn networks(&self) -> NetworkSnapshot {
            let unreadable = self.unreadable.lock().unwrap().clone();
            NetworkSnapshot::networks_only(match unreadable {
                Some(reason) => Err(reason),
                None => Ok(self.networks.lock().unwrap().clone()),
            })
        }

        fn scan(&self) -> Result<(), String> {
            let reads = self
                .snapshot_reads
                .load(std::sync::atomic::Ordering::Relaxed);
            self.scans.lock().unwrap().push(reads);
            self.scan_refusal
                .lock()
                .unwrap()
                .clone()
                .map_or(Ok(()), Err)
        }

        fn recheck_connectivity(&self) {
            self.connectivity_rechecks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        fn snapshot(&self, _initial: bool) -> NetworkSnapshot {
            self.snapshot_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let gate = self.snapshot_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.started.send(()).unwrap();
                gate.release
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
            let unreadable = self.unreadable.lock().unwrap().clone();
            if let Some(reason) = unreadable {
                fn failed<T>(reason: &str) -> Result<T, String> {
                    Err(reason.to_string())
                }
                return NetworkSnapshot {
                    networks: Err(reason.clone()),
                    wifi_enabled: Some(failed(&reason)),
                    sims: Some(failed(&reason)),
                    wired: Some(failed(&reason)),
                    radios: Some(failed(&reason)),
                    connectivity: Some(failed(&reason)),
                    profiles: Some(failed(&reason)),
                    profile_settings: HashMap::new(),
                    wake_settings: HashMap::new(),
                    hotspot_clients: Some(failed(&reason)),
                    vpns: Some(failed(&reason)),
                };
            }
            let mut snapshot = NetworkSnapshot::of_networks(self.networks.lock().unwrap().clone());
            snapshot.vpns = Some(Ok(self.vpns.lock().unwrap().clone()));
            snapshot.profiles = Some(Ok(self.profiles.lock().unwrap().clone()));
            snapshot
        }

        fn change(&self, change: &NetworkChange) -> Result<(), String> {
            let said = match change {
                NetworkChange::Wifi(on) => format!("wifi {on}"),
                NetworkChange::Radio(radio, on) => format!("{radio:?} {on}"),
                NetworkChange::Vpn { name, on } => format!("vpn {name} {on}"),
                NetworkChange::Connect { ssid, password } => format!(
                    "connect {ssid} {}",
                    password.as_deref().map_or("-", String::as_str)
                ),
                NetworkChange::Wake { uuid, on } => format!("wake {uuid} {on}"),
                NetworkChange::Forget { uuid, .. } => format!("forget {uuid}"),
                NetworkChange::Hotspot(config) => format!("hotspot {}", config.ssid),
                _ => "other".into(),
            };
            self.changes.lock().unwrap().push(said);
            self.refuse.lock().unwrap().clone().map_or(Ok(()), Err)
        }

        fn continuity(
            &self,
            command: WifiContinuityCommand,
        ) -> Result<WifiContinuityReply, String> {
            self.continuity.lock().unwrap().push(command.clone());
            Ok(match command {
                WifiContinuityCommand::Lookup { .. } => {
                    let shared = self.shared_password.lock().unwrap().clone();
                    shared.map_or(WifiContinuityReply::NotFound, |password| {
                        WifiContinuityReply::Password { password }
                    })
                }
                WifiContinuityCommand::NetworkWake { profile_uuid, .. } => {
                    WifiContinuityReply::NetworkWake {
                        status: NetworkWakeStatus {
                            profile: profile_uuid,
                            interface: None,
                            configured: false,
                            supported: None,
                            applied: None,
                            problem: None,
                        },
                    }
                }
                WifiContinuityCommand::Share { ssid, .. }
                | WifiContinuityCommand::ShareSavedProfile { profile_uuid: ssid } => {
                    WifiContinuityReply::Shared { ssid }
                }
            })
        }

        fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String> {
            Err("no bus".into())
        }

        fn sign_in_page(&self) -> Result<String, String> {
            Err("no portal".into())
        }

        fn open_tool(&self, intent: ToolIntent) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            let _ = intent;
            self.changes.lock().unwrap().push("tool".into());
            Ok(())
        }
    }

    fn networks() -> Vec<WifiNetworkInfo> {
        SampleNetwork::networks()
    }

    fn model(fake: &Arc<FakeNetwork>) -> NetworkModel {
        let profile = tempfile::tempdir().unwrap().keep();
        NetworkModel::loaded_from(
            PaneCx::in_file(profile.join("settings.json")),
            Arc::clone(fake) as Arc<dyn NetworkService>,
        )
    }

    fn mount_network(
        app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
        observed: NetworkModel,
    ) -> Option<()> {
        app.with_surface::<SettingsApp, _>(|settings| {
            *settings.network().model().borrow_mut() = observed;
        })
    }

    #[test]
    fn declared_network_inspection_reads_the_mounted_model_without_rescanning() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().unwrap() = networks();
        let mut observed = model(&fake);
        observed.enterprise_open = true;
        *observed.enterprise.password.lock().unwrap() = "private-enterprise-input".into();
        *observed.hotspot_password.lock().unwrap() = "private-hotspot-input".into();
        *observed.sim_pin.lock().unwrap() = "private-pin-input".into();
        let mut app =
            ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(ui::Role::Viewer)
                .expect("read-only Settings opens");
        mount_network(&mut app, observed).expect("the ordinary mounted network model");
        crate::effects::show_pane(&mut app, crate::categories::Category::Network);
        app.settle();
        let before = app
            .call("inspect-network-state", serde_json::json!({}))
            .expect("the network state must have a declared read-only door")
            .expect("the inspector returns the observed model");
        assert_eq!(before["network_count"], 3);
        assert_eq!(before["connectivity"]["state"], "full");
        assert_eq!(
            before["source"], "preview",
            "test stand-ins cannot masquerade as device observations"
        );
        let row_ids = app
            .surface::<SettingsApp, _>(|settings| {
                settings
                    .network()
                    .rows()
                    .into_iter()
                    .map(|row| row.id().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        for network in before["networks"].as_array().unwrap() {
            assert!(
                row_ids
                    .iter()
                    .any(|id| id == network["resource"].as_str().unwrap())
            );
        }
        let shown = crate::widgets::announced_text(app.harness().root_widget());
        assert!(
            shown.contains(
                &before["labels"]["enterprise_entry"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            ),
            "the Inspector and mounted pane use the same resolved caption"
        );
        let encoded = serde_json::to_string(&before).unwrap();
        for secret in [
            "private-enterprise-input",
            "private-hotspot-input",
            "private-pin-input",
        ] {
            assert!(
                !encoded.contains(secret),
                "explicit network reads omit private input buffers"
            );
        }
        assert!(
            app.call("refresh-networks", serde_json::json!({})).is_err(),
            "Read does not grant scan/write authority"
        );
        for actor in ["person:foreign", "agent:foreign"] {
            assert!(
                app.call_by(
                    ui::tilo_actions::ActionInvoker::actor(actor),
                    "settings.root",
                    "inspect-network-state",
                    serde_json::json!({})
                )
                .is_err(),
                "device-only inspection refuses unrelated callers"
            );
        }
        assert!(
            app.call("inspect-network-state", serde_json::json!({"on":false}))
                .is_err(),
            "Read accepts no mutation-shaped arguments"
        );
        let after = app
            .call("inspect-network-state", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(
            before, after,
            "inspection never refreshes or changes network state"
        );
        assert_eq!(
            fake.snapshot_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(fake.changes.lock().unwrap().is_empty());
        assert!(app.harness().component_contract_violations().is_empty());
    }

    #[test]
    fn network_inspection_observes_the_actual_pending_refresh_and_failed_snapshot() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().unwrap() = networks();
        let observed = model(&fake);
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        mount_network(&mut app, observed).unwrap();
        app.settle();
        let (started, starts) = crate::channel::channel();
        let (release, releases) = crate::channel::channel();
        *fake.snapshot_gate.lock().unwrap() = Some(SnapshotGate {
            started,
            release: releases,
        });
        let mut release = ReleaseSnapshot(Some(release));
        *fake.unreadable.lock().unwrap() = Some("NetworkManager query refused".into());
        let effect = SettingsEffect::from(NetworkEffect::RefreshNetworks);
        assert!(
            app.operate(ui::tilo_resource::Invoke::effect(&effect))
                .unwrap()
                .is_none(),
            "scan completion awaits its actual worker"
        );
        starts
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let pending = app
            .call("inspect-network-state", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(pending["scan"]["jobs_pending"], 1);
        assert_eq!(
            pending["network_count"], 3,
            "pending scans preserve the last adopted rows"
        );
        release.0.take().unwrap().send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let failed = loop {
            app.settle();
            let snapshot = app
                .call("inspect-network-state", serde_json::json!({}))
                .unwrap()
                .unwrap();
            if snapshot["scan"]["jobs_pending"] == 0 {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the real worker outcome must land"
            );
            std::thread::yield_now();
        };
        assert_eq!(failed["errors"]["networks"], "NetworkManager query refused");
        assert_eq!(
            failed["network_count"], 0,
            "failed scans do not publish stale rows as current"
        );
        assert!(
            failed["wifi"]["enabled"].is_null(),
            "unreadable radio state is never fabricated as off"
        );
        assert!(failed["connectivity"]["state"].is_null());
        assert_eq!(
            fake.snapshot_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
    }

    #[test]
    fn network_inspection_does_not_initialize_an_unopened_area_and_bounds_private_reads() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = NetworkArea::new(&cx);
        assert_eq!(area.inspection().unwrap()["state"], "unopened");
        assert!(
            area.model.get().is_none(),
            "Read must not lazily start a device scan"
        );
        let fake = Arc::new(FakeNetwork::default());
        let mut observed = NetworkModel::with_service(cx, fake);
        let initial = observed.inspection().unwrap();
        assert_eq!(initial["state"], "loading");
        assert!(initial["wifi"]["enabled"].is_null());
        assert!(initial["connectivity"]["state"].is_null());
        observed.networks = vec![
            WifiNetworkInfo {
                ssid: "x".repeat(1024),
                signal: 0,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: false,
                known: false,
            };
            257
        ];
        assert!(
            observed.inspection().is_err(),
            "the explicit Read has a finite byte bound"
        );
        for row in &mut observed.networks {
            row.ssid = "bounded".into();
        }
        let bounded = observed.inspection().unwrap();
        assert_eq!(bounded["network_count"], 257);
        assert_eq!(bounded["networks"].as_array().unwrap().len(), 256);
        assert_eq!(bounded["networks_truncated"], true);
    }

    /// Begin `effect` and run its work here, as a worker would; what it
    /// answered.
    fn run(model: &mut NetworkModel, effect: NetworkEffect) -> Result<Option<String>, String> {
        match model.begin(effect)? {
            Begun::Done => Ok(None),
            Begun::Work(work) => match model.land(work())? {
                Effected::Done(output) => Ok(output
                    .value
                    .and_then(|value| value.as_str().map(str::to_owned))),
                Effected::Pending => Ok(None),
            },
        }
    }

    /// Opening the VPN studio asks the shell on a worker, never while the
    /// effect is begun on the UI thread; the shell's answer is the effect's.
    #[test]
    fn the_vpn_studio_is_opened_off_the_ui_thread() {
        let fake = Arc::new(FakeNetwork::default());
        let mut model = model(&fake);
        let Ok(Begun::Work(work)) = model.begin(NetworkEffect::OpenVpnStudio { vpn: None }) else {
            panic!("opening the VPN studio runs on a worker");
        };
        assert!(
            fake.changes.lock().unwrap().is_empty(),
            "the shell is not asked before the worker runs"
        );
        let landed = std::thread::spawn(work).join().unwrap();
        assert_eq!(fake.changes.lock().unwrap().as_slice(), ["tool"]);
        let notice = model.notice.clone();
        assert!(matches!(model.land(landed), Ok(Effected::Done(_))));
        assert_eq!(model.notice, notice, "the tool answers for itself");
    }

    /// The ordinary worker completion must honor the action a caller invoked,
    /// while keeping its human-readable outcome in the pane.
    #[test]
    fn network_workers_complete_their_published_unit_jobs() -> Result<(), String> {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().map_err(|error| error.to_string())? = networks();
        let mut model = model(&fake);
        for (effect, notice) in [
            (NetworkEffect::RefreshNetworks, t("net-notice-refreshed")),
            (NetworkEffect::SetWifi { on: true }, t("net-notice-wifi-on")),
        ] {
            let contract = NetworkEffect::specs()
                .into_iter()
                .find(|spec| spec.id == effect.id())
                .ok_or("the invoked action must be published")?
                .contract;
            if contract.result != ui::tilo_actions::ResultSpec::Job {
                return Err("the network action must declare a unit job".into());
            }
            let Begun::Work(work) = model.begin(effect)? else {
                return Err("the invoked network action must start real work".into());
            };
            let Effected::Done(output) = model.land(work())? else {
                return Err("finished network work must complete".into());
            };
            contract.result.validate(&output)?;
            if output.value.is_some() || !output.resources.is_empty() {
                return Err("the unit job must not return a value or resources".into());
            }
            if model.notice.as_deref() != Some(notice.as_str()) {
                return Err("the completed job must retain its status notice".into());
            }
            if !shown(&model).contains(&notice) {
                return Err("the completed job's notice must remain visible".into());
            }
        }
        if model.networks().len() != 3 || !model.wifi_enabled {
            return Err("the completed workers must adopt networks and enable Wi-Fi".into());
        }
        if fake
            .changes
            .lock()
            .map_err(|error| error.to_string())?
            .as_slice()
            != ["wifi true"]
        {
            return Err("the worker must issue exactly one Wi-Fi enable request".into());
        }
        Ok(())
    }

    fn shown(model: &NetworkModel) -> Vec<String> {
        crate::widgets::announced_text(model.build_composition().as_ref())
    }

    /// Wait for the read a landed change started, as the pane's poll does.
    fn settle(model: &mut NetworkModel) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            model.poll_background();
            if model.snapshot.is_none() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the read never landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The Wi-Fi switch as composed: whether it is on, and whether it can
    /// be used.
    fn wifi_switch(model: &NetworkModel) -> (Option<bool>, bool) {
        let composed = model.build_composition();
        let node = ui::widget::find_test_id(composed.as_ref(), "network.wifi")
            .and_then(Widget::accessibility)
            .expect("the Wi-Fi switch is composed");
        (node.state.checked, !node.state.disabled)
    }

    /// `NetworkManager`'s flag and the kernel's kill switch, as far as Wi-Fi
    /// on and off goes: turning on fails while a hardware switch holds the
    /// radio off, and every read says how the radio stands.
    #[derive(Default)]
    struct NmRadio {
        on: Mutex<bool>,
        hard: Mutex<bool>,
    }

    impl NetworkService for NmRadio {
        fn snapshot(&self, _initial: bool) -> NetworkSnapshot {
            let on = *self.on.lock().unwrap() && !*self.hard.lock().unwrap();
            let mut snapshot =
                NetworkSnapshot::of_networks(if on { networks() } else { Vec::new() });
            snapshot.wifi_enabled = Some(Ok(on));
            snapshot.radios = Some(Ok(os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: on,
                wifi_hardware_blocked: *self.hard.lock().unwrap(),
                ..os_actions::RadioState::default()
            }));
            snapshot
        }
        fn change(&self, change: &NetworkChange) -> Result<(), String> {
            if let NetworkChange::Wifi(on) = change {
                if *on && *self.hard.lock().unwrap() {
                    return Err("Wi-Fi is switched off with the hardware switch".into());
                }
                *self.on.lock().unwrap() = *on;
            }
            Ok(())
        }
        fn continuity(&self, _: WifiContinuityCommand) -> Result<WifiContinuityReply, String> {
            Ok(WifiContinuityReply::NotFound)
        }
        fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String> {
            Err("no bus".into())
        }
        fn sign_in_page(&self) -> Result<String, String> {
            Err("no portal".into())
        }
        fn open_tool(&self, _: ToolIntent) -> Result<(), String> {
            Ok(())
        }
    }

    /// The Wi-Fi switch is the radio as `NetworkManager` and the kernel
    /// report it, read back after every change: off and on again lands
    /// each time, and a radio a hardware switch holds off shows off, with
    /// the switch unusable and the reason in place of the networks.
    #[test]
    fn the_wifi_switch_follows_the_radio_and_not_the_request() {
        let nm = Arc::new(NmRadio::default());
        let profile = tempfile::tempdir().unwrap().keep();
        let mut model = NetworkModel::loaded_from(
            PaneCx::in_file(profile.join("settings.json")),
            Arc::clone(&nm) as Arc<dyn NetworkService>,
        );
        assert_eq!(wifi_switch(&model), (Some(false), true));
        let text = shown(&model).join("\n");
        assert!(
            text.contains(&t("net-turn-on-wifi")),
            "the status offers the fix: {text}"
        );

        for on in [true, false, true] {
            run(&mut model, NetworkEffect::SetWifi { on }).unwrap();
            settle(&mut model);
            assert_eq!(wifi_switch(&model), (Some(on), true), "after turning {on}");
            assert_eq!(*nm.on.lock().unwrap(), on);
        }
        assert!(
            shown(&model).join("\n").contains("Home_Network"),
            "on, the networks in range are listed"
        );

        run(&mut model, NetworkEffect::SetWifi { on: false }).unwrap();
        settle(&mut model);
        *nm.hard.lock().unwrap() = true;
        let refused = run(&mut model, NetworkEffect::SetWifi { on: true });
        assert!(refused.is_err());
        settle(&mut model);
        assert_eq!(
            wifi_switch(&model),
            (Some(false), false),
            "the radio's state, not the request; nothing to switch"
        );
        assert!(
            shown(&model)
                .join("\n")
                .contains(&t("net-wifi-hardware-switch"))
        );
    }

    /// A known network's details open in place: joining by itself, metered
    /// data and forgetting it, each a control of its own.
    #[test]
    fn a_known_network_opens_its_details_in_place() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.profiles.lock().unwrap() = vec![SavedNetworkProfile {
            uuid: "1234-abcd".into(),
            name: "Home_Network".into(),
            kind: "802-11-wireless".into(),
            autoconnect: true,
        }];
        let mut model = model(&fake);
        // The details are open when the row holding them is.
        fn open_holding(widget: &dyn Widget, id: &str) -> bool {
            if let Some(row) = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<DisclosureRow>())
                && widget
                    .children()
                    .iter()
                    .any(|child| ui::widget::find_test_id(*child, id).is_some())
            {
                return row.is_open();
            }
            widget
                .children()
                .iter()
                .any(|child| open_holding(*child, id))
        }
        let details = |model: &NetworkModel| {
            let _settled = ui::motion::testing::settled();
            let composed = model.build_composition();
            open_holding(composed.as_ref(), "network.profile.autoconnect.1234-abcd")
        };
        assert!(!details(&model), "closed until asked");
        assert!(shown(&model).join("\n").contains(&t("net-known")));
        run(
            &mut model,
            NetworkEffect::ShowKnownNetwork {
                profile: "1234-abcd".into(),
            },
        )
        .unwrap();
        assert!(details(&model), "open in place");
        let text = shown(&model).join("\n");
        assert!(text.contains(&t("net-forget-network")), "{text}");
        assert!(!text.contains("Connection policy"), "{text}");
        run(
            &mut model,
            NetworkEffect::ShowKnownNetwork {
                profile: "1234-abcd".into(),
            },
        )
        .unwrap();
        assert!(!details(&model), "and closed again");
    }

    /// A wired-only machine names its missing adapter without treating it as
    /// a read failure or offering a radio switch.
    #[test]
    fn a_machine_without_wifi_says_so_instead_of_reporting_a_read_error() {
        let radios = Ok(os_actions::RadioState::default());
        assert_eq!(wifi_enabled_from(&radios), Ok(false));
        assert_eq!(
            wifi_enabled_from(&Err("nmcli not installed".into())),
            Err("nmcli not installed".to_string())
        );
        let fake = Arc::new(FakeNetwork::default());
        let mut model = model(&fake);
        let mut snapshot = NetworkSnapshot::of_networks(Vec::new());
        snapshot.wifi_enabled = Some(wifi_enabled_from(&radios));
        snapshot.radios = Some(radios);
        model.adopt_snapshot(snapshot).unwrap();
        let text = shown(&model);
        assert!(
            text.iter()
                .any(|line| line == "No Wi-Fi adapter on this device"),
            "{text:#?}"
        );
        assert!(
            !text
                .iter()
                .any(|line| line.contains("Can't read network status")),
            "{text:#?}"
        );
        assert!(!text.contains(&t("net-wifi-off")), "{text:#?}");
    }

    /// A status that could not be read is shown as that failure, never as a
    /// state; a later successful read clears it.
    #[test]
    fn unreadable_network_state_is_shown_as_an_error_not_as_a_state() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.unreadable.lock().unwrap() = Some("org.freedesktop.NetworkManager is gone".into());
        let mut model = model(&fake);
        let text = shown(&model).join("\n");
        assert!(
            text.contains("org.freedesktop.NetworkManager is gone"),
            "{text}"
        );
        assert!(!text.contains(&t("net-not-connected")), "{text}");
        assert!(
            model.begin(NetworkEffect::SetWifi { on: true }).is_err(),
            "an unread radio is not switched"
        );

        assert_eq!(
            fake.connectivity_rechecks
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "opening Network reads the last verdict; it does not probe the internet"
        );

        *fake.unreadable.lock().unwrap() = None;
        *fake.networks.lock().unwrap() = networks();
        run(&mut model, NetworkEffect::RefreshNetworks).unwrap();
        assert_eq!(model.read_errors, NetworkReadErrors::default());
        assert_eq!(model.networks().len(), 3);
        assert_eq!(
            fake.connectivity_rechecks
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the explicit refresh asks for a fresh connectivity verdict"
        );
    }

    /// Opening Network shows the connected network from what
    /// `NetworkManager` already knows; it never waits for a radio scan,
    /// which takes over ten seconds on some adapters and once timed the
    /// status out ("Can't read network status: nmcli timed out after
    /// 12000 ms") while the internet worked. Only Refresh scans, before it
    /// reads, and a failed scan leaves the status and list readable.
    #[test]
    fn network_status_never_waits_for_a_scan_and_refresh_scans_first() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().unwrap() = networks();
        let mut model = model(&fake);
        assert!(
            fake.scans.lock().unwrap().is_empty(),
            "opening Network must not scan"
        );
        let connected = t_args("net-connected-to", &[("ssid", "Home_Network".into())]);
        let text = shown(&model).join("\n");
        assert!(text.contains(&connected), "{text}");
        assert_eq!(model.wifi_status_error(), None);

        run(&mut model, NetworkEffect::RefreshNetworks).unwrap();
        assert_eq!(
            *fake.scans.lock().unwrap(),
            vec![1],
            "Refresh scans once, before the snapshot it adopts"
        );
        assert_eq!(
            fake.snapshot_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );

        *fake.scan_refusal.lock().unwrap() = Some("Scanning not allowed".into());
        let refused = run(&mut model, NetworkEffect::RefreshNetworks)
            .expect_err("a failed scan is reported to whoever pressed Refresh");
        assert!(refused.contains("Scanning not allowed"), "{refused}");
        assert_eq!(model.read_errors, NetworkReadErrors::default());
        assert_eq!(model.wifi_status_error(), None);
        assert_eq!(model.networks().len(), 3);
        let text = shown(&model).join("\n");
        assert!(text.contains(&connected), "{text}");
        assert!(!text.contains("Can't read network status"), "{text}");
    }

    /// A secure network first asks the person's devices for its password,
    /// opens its prompt when they share none, and joins with what is typed
    /// there; the typed password is zeroed and the prompt closes.
    #[test]
    fn joining_a_secure_network_asks_then_uses_the_typed_password() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().unwrap() = networks();
        let mut model = model(&fake);
        let connect = || NetworkEffect::ConnectNetwork {
            ssid: "Office_5G".into(),
            password: None,
        };
        run(&mut model, connect()).unwrap();
        assert_eq!(model.expanded.as_deref(), Some("Office_5G"));
        assert!(
            fake.changes.lock().unwrap().is_empty(),
            "nothing joined yet"
        );

        let buffer = Arc::clone(&model.join_passwords["Office_5G"]);
        *buffer.lock().unwrap() = "hunter22".into();
        run(&mut model, connect()).unwrap();
        assert_eq!(
            fake.changes.lock().unwrap().as_slice(),
            ["connect Office_5G hunter22"]
        );
        assert!(
            buffer.lock().unwrap().is_empty(),
            "the typed password is zeroed"
        );
        assert_eq!(model.expanded, None);
        assert!(
            fake.continuity
                .lock()
                .unwrap()
                .iter()
                .any(|command| matches!(command, WifiContinuityCommand::Share { .. })),
            "the password is sealed for the person's devices"
        );
    }

    /// A password the person's devices share joins without a prompt; an
    /// open network joins at once.
    #[test]
    fn a_shared_password_or_an_open_network_joins_directly() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().unwrap() = networks();
        *fake.shared_password.lock().unwrap() = Some("from-phone".into());
        let mut model = model(&fake);
        run(
            &mut model,
            NetworkEffect::ConnectNetwork {
                ssid: "Office_5G".into(),
                password: None,
            },
        )
        .unwrap();
        run(
            &mut model,
            NetworkEffect::ConnectNetwork {
                ssid: "Cafe_Guest".into(),
                password: None,
            },
        )
        .unwrap();
        assert_eq!(
            fake.changes.lock().unwrap().as_slice(),
            ["connect Office_5G from-phone", "connect Cafe_Guest -"]
        );
        assert_eq!(model.expanded, None);
        assert!(
            model
                .begin(NetworkEffect::ConnectNetwork {
                    ssid: "Elsewhere".into(),
                    password: None,
                })
                .is_err(),
            "a network out of range is refused"
        );
    }

    /// A switch shows the requested state at once and puts it back when the
    /// device refuses.
    #[test]
    fn a_refused_switch_is_put_back() {
        let fake = Arc::new(FakeNetwork::default());
        let mut model = model(&fake);
        run(&mut model, NetworkEffect::SetAirplaneMode { on: true }).unwrap();
        assert!(model.radios.airplane_mode);

        *fake.refuse.lock().unwrap() = Some("rfkill is blocked".into());
        let refused = run(&mut model, NetworkEffect::SetAirplaneMode { on: false });
        assert!(refused.unwrap_err().contains("rfkill is blocked"));
        assert!(model.radios.airplane_mode, "the switch is put back");
        assert_eq!(
            fake.changes.lock().unwrap().as_slice(),
            ["Airplane true", "Airplane false"]
        );
    }

    /// Each nearby network, saved network and VPN is a row offering what can
    /// be done with it; switching a VPN reaches the device.
    #[test]
    fn networks_saved_profiles_and_vpns_are_rows() {
        let fake = Arc::new(FakeNetwork::default());
        *fake.networks.lock().unwrap() = networks();
        *fake.vpns.lock().unwrap() = vec![VpnInfo {
            name: "Work".into(),
            active: false,
        }];
        *fake.profiles.lock().unwrap() = vec![SavedNetworkProfile {
            uuid: "1234-abcd".into(),
            name: "Home_Network".into(),
            kind: "802-11-wireless".into(),
            autoconnect: true,
        }];
        let profile = tempfile::tempdir().unwrap().keep();
        let cx = PaneCx::in_file(profile.join("settings.json"));
        let area = NetworkArea::new(&cx);
        let _ = area
            .model
            .set(Rc::new(std::cell::RefCell::new(model(&fake))));
        let ids: Vec<String> = area.rows().iter().map(|row| row.id().to_owned()).collect();
        for id in [
            "settings.network:Home_Network",
            "settings.network:Office_5G",
            "settings.network-profile:1234-abcd",
            "settings.vpn:Work",
        ] {
            assert!(ids.iter().any(|row| row == id), "{id} in {ids:?}");
        }

        let mut model = area.model().borrow_mut();
        run(
            &mut model,
            NetworkEffect::SetVpn {
                vpn: "Work".into(),
                on: true,
            },
        )
        .unwrap();
        run(
            &mut model,
            NetworkEffect::ForgetNetwork {
                profile: "Home_Network".into(),
            },
        )
        .unwrap();
        assert_eq!(
            fake.changes.lock().unwrap().as_slice(),
            ["vpn Work true", "forget 1234-abcd"]
        );
        assert!(model.profiles.is_empty(), "a forgotten network is gone");
    }

    #[test]
    fn supported_network_wake_is_named_and_routes_through_the_worker() {
        fn named_switch(widget: &dyn Widget, name: &str) -> bool {
            widget.accessibility().is_some_and(|node| {
                node.role == ui::a11y::Role::Switch && node.name.as_deref() == Some(name)
            }) || widget
                .children()
                .iter()
                .any(|child| named_switch(*child, name))
        }
        let fake = Arc::new(FakeNetwork::default());
        let uuid = "11111111-1111-4111-8111-111111111111";
        *fake.profiles.lock().unwrap() = vec![SavedNetworkProfile {
            uuid: uuid.into(),
            name: "Home".into(),
            kind: "802-11-wireless".into(),
            autoconnect: true,
        }];
        let mut model = model(&fake);
        model.wake_settings.insert(
            uuid.into(),
            Ok(NetworkWakeStatus {
                profile: uuid.into(),
                interface: Some("wlan0".into()),
                configured: false,
                supported: Some(true),
                applied: Some(false),
                problem: None,
            }),
        );
        let area = NetworkArea::new(&model.cx);
        let _ = area.model.set(Rc::new(std::cell::RefCell::new(model)));
        let row = area
            .rows()
            .into_iter()
            .find(|row| row.id() == format!("settings.network-profile:{uuid}"))
            .unwrap()
            .into_resource();
        assert!(
            row.actions
                .iter()
                .any(|action| action.label == "Wake over network")
        );
        assert!(
            row.presentation
                .unwrap()
                .preview
                .unwrap()
                .contains("must be tested separately")
        );
        let mut model = area.model().borrow_mut();
        // A known network's wake is among its details, open in place.
        model.known_open = Some(uuid.into());
        assert!(named_switch(
            model.build_composition().as_ref(),
            "Wake over Home"
        ));
        let text = shown(&model);
        assert!(
            text.iter().any(|text| text.contains("Wake over Home")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|text| text.contains("must be tested separately")),
            "{text:?}"
        );
        run(
            &mut model,
            NetworkEffect::SetNetworkWake {
                profile: uuid.into(),
                on: true,
            },
        )
        .unwrap();
        assert_eq!(
            fake.changes.lock().unwrap().as_slice(),
            [format!("wake {uuid} true")]
        );
        assert!(
            model
                .begin(NetworkEffect::SetNetworkWake {
                    profile: "missing".into(),
                    on: true
                })
                .is_err()
        );
        *fake.refuse.lock().unwrap() = Some("not authorized".into());
        assert!(
            run(
                &mut model,
                NetworkEffect::SetNetworkWake {
                    profile: uuid.into(),
                    on: false
                }
            )
            .unwrap_err()
            .contains("not authorized")
        );
    }

    /// The hotspot starts from its form, and a password shorter than eight
    /// characters is refused before anything reaches the device.
    #[test]
    fn the_hotspot_starts_from_its_form() {
        let fake = Arc::new(FakeNetwork::default());
        let mut model = model(&fake);
        *model.hotspot_password.lock().unwrap() = "short".into();
        assert!(
            model
                .begin(NetworkEffect::StartHotspot {
                    name: None,
                    password: None,
                    band: None,
                })
                .is_err()
        );
        *model.hotspot_password.lock().unwrap() = "long enough secret".into();
        model.hotspot_name.set("Kitchen".into());
        run(
            &mut model,
            NetworkEffect::StartHotspot {
                name: None,
                password: None,
                band: None,
            },
        )
        .unwrap();
        assert_eq!(fake.changes.lock().unwrap().as_slice(), ["hotspot Kitchen"]);
        assert!(model.radios.hotspot_active);
    }

    /// The availability Settings publishes for `id`, as agents, the
    /// palette and assistive technology read it.
    fn published(
        app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
        id: &str,
    ) -> ui::tilo_actions::Availability {
        app.actions()
            .expect("the actions are published")
            .into_iter()
            .find(|action| action.address.action == id)
            .unwrap_or_else(|| panic!("`{id}` is published"))
            .contract
            .availability
    }

    fn unavailable(code: &str, reason: &str) -> ui::tilo_actions::Availability {
        ui::tilo_actions::Availability::Unavailable {
            code: code.into(),
            reason: reason.into(),
        }
    }

    /// A wired-only machine publishes Wi-Fi, the hotspot, mobile broadband
    /// and airplane mode unavailable with why, and refuses them; a Wi-Fi
    /// dongle plugged in is offered at once, before `NetworkManager` is
    /// read again.
    #[test]
    fn without_a_wifi_radio_wifi_is_unavailable_until_one_is_plugged_in() {
        use crate::radios::Present;
        use ui::tilo_actions::Availability;

        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let fake = Arc::new(FakeNetwork::default());
        let mut wired_only = model(&fake);
        let mut snapshot = NetworkSnapshot::of_networks(Vec::new());
        snapshot.radios = Some(Ok(os_actions::RadioState::default()));
        snapshot.wifi_enabled = Some(Ok(false));
        wired_only.adopt_snapshot(snapshot).unwrap();
        mount_network(&mut app, wired_only).unwrap();
        let radios = app.surface_mut::<SettingsApp>().host().radios().clone();

        radios.set(Present::NONE);
        app.settle();
        let no_wifi = unavailable("no_adapter", "No Wi-Fi adapter on this device");
        for id in ["set-wifi", "set-hotspot", "start-hotspot"] {
            assert_eq!(published(&mut app, id), no_wifi, "`{id}`");
        }
        assert_eq!(
            published(&mut app, "set-mobile-broadband"),
            unavailable("no_modem", "No mobile broadband modem on this device")
        );
        assert_eq!(
            published(&mut app, "set-airplane-mode"),
            unavailable(
                "no_radio",
                "No Wi-Fi or mobile broadband radio on this device"
            )
        );
        assert_eq!(
            published(&mut app, "refresh-networks"),
            Availability::Available,
            "a wired connection is still read"
        );
        app.effect(&SettingsEffect::from(NetworkEffect::SetWifi { on: true }))
            .expect_err("an unavailable radio is refused, not tried");
        assert!(fake.changes.lock().unwrap().is_empty(), "nothing was tried");

        radios.set(Present {
            wifi: true,
            ..Present::NONE
        });
        app.settle();
        for id in [
            "set-wifi",
            "set-hotspot",
            "start-hotspot",
            "set-airplane-mode",
        ] {
            assert_eq!(published(&mut app, id), Availability::Available, "`{id}`");
        }
        assert_eq!(
            published(&mut app, "set-mobile-broadband"),
            unavailable("no_modem", "No mobile broadband modem on this device")
        );
    }

    /// Either the kernel or `NetworkManager` seeing a radio is enough: a
    /// modem `ModemManager` drives over a serial port is listed by
    /// `NetworkManager` alone. Neither having said offers everything, and
    /// the area's model is never read for it.
    #[test]
    fn a_radio_either_source_sees_is_offered() {
        use crate::radios::Present;

        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = NetworkArea::new(&cx);
        assert!(
            area.statuses().is_empty(),
            "radios not read offer everything"
        );
        assert!(!area.is_open(), "the networks were not read");

        cx.radios().set(Present::ALL);
        assert!(
            area.statuses().is_empty(),
            "with every radio nothing changes"
        );

        let reported = os_actions::RadioState {
            wwan_available: true,
            ..os_actions::RadioState::default()
        };
        cx.radios().set(Present::NONE);
        assert_eq!(
            NetworkRadios::of(&cx, Some(&reported)),
            NetworkRadios {
                wifi: Some(false),
                wwan: Some(true),
            }
        );
        assert_eq!(
            NetworkRadios::of(&PaneCx::in_file(profile.path().join("other.json")), None),
            NetworkRadios::default(),
            "neither has said"
        );
    }
}
