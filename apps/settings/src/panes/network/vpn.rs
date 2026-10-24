//! VPN: the tunnels `NetworkManager` knows, each switched on or off from
//! the list, opened on a page of its own to change, export or delete it, and
//! added from a `WireGuard` form, an installed plugin's type or a file.
//!
//! The list also offers the person's other devices as exits, and this
//! device as one for them ([`devices`], docs/device-vpn.md). It takes
//! further ways out from a [`VpnEntries`] source beside those.
//!
//! Keys and configuration files live in process-local buffers that are
//! zeroed once used, and reach `NetworkManager` through a private file or
//! its standard input, never an argument.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use os_actions::network_settings::{
    MAX_VPN_CONFIG_BYTES, VpnPlugin, VpnProfilePatch, VpnProfileSettings, WireGuardTunnel,
};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, LiveRow};
use ui::icons::Icon;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, ButtonRole, ListRow, NumberInput, TextInput, Toggle};
use zeroize::{Zeroize, Zeroizing};

use super::{
    Begun, Landed, NetworkChange, NetworkModel, VpnInfo, VpnSetup, list_of, non_blank, take_secret,
};
use crate::categories::Category;
use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, Item, SettingsSection, block, card, error_caption, field_row, note, row, rows, setting_row,
    setting_row_described, value_row,
};

mod devices;
mod kill_switch;

/// A key, under its label: a key is longer than the room a trailing value
/// has beside a label, so it takes the row's measure and breaks across two
/// lines in a narrow window.
fn key_row(label: &str, key: &str) -> Item {
    row(Box::new(
        ListRow::new(format!("setting/{label}"), label)
            .secondary(key)
            .secondary_lines(2),
    ))
}

crate::effects::effect_family!(VpnEffect => answer);

/// The VPN list.
pub(crate) const PAGE_VPN: &str = "vpn";
/// Adding a VPN: its kinds.
pub(crate) const PAGE_ADD: &str = "vpn-add";
/// A new `WireGuard` tunnel.
const PAGE_ADD_WIREGUARD: &str = "vpn-add:wireguard";
/// A new profile of a plugin's type: `vpn-add:<service>`.
const PAGE_ADD_PLUGIN: &str = "vpn-add:";
/// A VPN's page: `vpn:<uuid>`.
const PAGE_PROFILE: &str = "vpn:";

pub(crate) fn profile_page_key(uuid: &str) -> String {
    format!("{PAGE_PROFILE}{uuid}")
}

fn plugin_page_key(service: &str) -> String {
    format!("{PAGE_ADD_PLUGIN}{service}")
}

/// What `allowed_ips` a new tunnel routes unless told otherwise: everything.
const ALL_ADDRESSES: &str = "0.0.0.0/0, ::/0";

/// Whether `text` is an address a tunnel can route: an IP address, or one
/// with a prefix no longer than the address.
fn is_routed_address(text: &str) -> bool {
    let (address, prefix) = text
        .split_once('/')
        .map_or((text, None), |(address, prefix)| (address, Some(prefix)));
    let Ok(address) = address.parse::<std::net::IpAddr>() else {
        return false;
    };
    let longest = if address.is_ipv4() { 32 } else { 128 };
    prefix.is_none_or(|prefix| prefix.parse::<u8>().is_ok_and(|prefix| prefix <= longest))
}

/// What VPN does on this device. The import, connect, disconnect and delete
/// effects keep the ids the VPN app published, so what asked for them
/// before finds them here.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum VpnEffect {
    /// Show the VPNs.
    #[effect(view, label = "VPN")]
    ShowVpns,
    /// Show a VPN's page: its settings, Export and Delete.
    #[effect(view, row, label = "VPN settings")]
    ShowVpnProfile {
        #[param("The VPN's id or name")]
        connection: String,
    },
    /// Show the kinds of VPN that can be added.
    #[effect(view, label = "Add VPN")]
    ShowAddVpn,
    /// Show the form of a new `WireGuard` tunnel.
    #[effect(view, label = "Add WireGuard tunnel")]
    ShowAddWireguard,
    /// Show the form of a new profile of an installed VPN plugin's type.
    #[effect(view, label = "Add VPN of a plugin's type")]
    ShowAddPluginVpn {
        #[param("The plugin's service type")]
        service: String,
    },
    /// Pick a `WireGuard` `.conf` file and set it up.
    #[effect(attended, label = "Import WireGuard profile")]
    ImportWireguard,
    /// Pick an `OpenVPN` `.ovpn` file and set it up.
    #[effect(attended, label = "Import OpenVPN profile")]
    ImportOpenvpn,
    /// Pick a `WireGuard` or `OpenVPN` file and set it up.
    #[effect(attended, label = "Import from file")]
    ImportVpnFile,
    /// Ask for the private key of a new `WireGuard` tunnel with the password
    /// prompt.
    #[effect(view, label = "Private key")]
    AskWireguardPrivateKey,
    /// Make a key pair for a new `WireGuard` tunnel: the public key is the
    /// one to give the server.
    #[effect(label = "Generate keys")]
    GenerateWireguardKeys,
    /// Set up a `WireGuard` tunnel; what is omitted is what its form holds.
    #[effect(job, label = "Create tunnel")]
    CreateWireguard {
        name: Option<String>,
        #[param("This device's address in the tunnel with its prefix: 10.7.0.2/32")]
        address: Option<String>,
        #[param("The server's public key")]
        server_public_key: Option<String>,
        #[param("The server: host:port")]
        endpoint: Option<String>,
        #[param("What goes through the tunnel, comma-separated; everything when omitted")]
        allowed_ips: Option<String>,
        #[param("Name servers, comma-separated")]
        dns: Option<String>,
        #[param("Seconds between keepalives; 0 sends none")]
        keepalive: Option<u32>,
        #[param(secret, "This device's private key")]
        private_key: Option<String>,
    },
    /// Set up a `WireGuard` tunnel from a pasted configuration; what is
    /// omitted is what its form holds.
    #[effect(job, label = "Import configuration")]
    ImportWireguardConfig {
        name: Option<String>,
        #[param(secret, "The configuration file's text")]
        config: Option<String>,
    },
    /// Make a profile of an installed plugin's type; what is omitted is what
    /// its form holds.
    #[effect(job, label = "Create VPN")]
    CreatePluginVpn {
        #[param("The plugin's service type")]
        service: String,
        name: Option<String>,
        #[param("The server: a host name or address")]
        server: Option<String>,
    },
    /// Bring a VPN up or down.
    #[effect(job, row, idempotent, label = "VPN connection")]
    SetVpnConnected {
        #[param("The VPN's id or name")]
        connection: String,
        on: bool,
    },
    /// Bring a VPN up.
    #[effect(job, row, label = "Connect")]
    Connect {
        #[param("The VPN's id or name")]
        connection: String,
    },
    /// Bring a VPN down.
    #[effect(job, row, label = "Disconnect")]
    Disconnect {
        #[param("The VPN's id or name")]
        connection: String,
    },
    /// Delete a VPN from this device, with its keys.
    #[effect(job, row, destructive, label = "Delete VPN")]
    DeleteConnection {
        #[param("The VPN's id or name")]
        connection: String,
    },
    /// Rename a VPN.
    #[effect(job, row, idempotent, label = "Name")]
    SetVpnName {
        #[param("The VPN's id or name")]
        connection: String,
        name: String,
    },
    /// Bring a VPN up by itself, or not.
    #[effect(job, row, idempotent, label = "Connect automatically")]
    SetVpnAutoconnect {
        #[param("The VPN's id or name")]
        connection: String,
        on: bool,
    },
    /// Send every connection through a VPN, or only what it routes.
    #[effect(job, row, idempotent, label = "Send all traffic")]
    SetVpnAllTraffic {
        #[param("The VPN's id or name")]
        connection: String,
        on: bool,
    },
    /// Have a VPN ask this name server too; without any it asks its own.
    #[effect(job, row, idempotent, label = "Add DNS server")]
    AddVpnDnsServer {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("The name server's IP address")]
        server: String,
    },
    /// Stop a VPN asking this name server.
    #[effect(job, row, idempotent, label = "Remove DNS server")]
    RemoveVpnDnsServer {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("The name server's IP address")]
        server: String,
    },
    /// The largest packet a VPN sends, in bytes; chosen automatically when
    /// omitted.
    #[effect(job, row, idempotent, label = "Packet size")]
    SetVpnMtu {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("Bytes, 576 to 9000")]
        mtu: Option<u32>,
    },
    /// Where a `WireGuard` tunnel's server is.
    #[effect(job, row, idempotent, label = "Server")]
    SetVpnServer {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("host:port")]
        endpoint: String,
    },
    /// Have a `WireGuard` tunnel route this address too.
    #[effect(job, row, idempotent, label = "Add routed address")]
    AddVpnRoutedAddress {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("An address with its prefix: 10.20.0.0/16")]
        address: String,
    },
    /// Stop a `WireGuard` tunnel routing this address.
    #[effect(job, row, idempotent, label = "Remove routed address")]
    RemoveVpnRoutedAddress {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("An address with its prefix, as the tunnel lists it")]
        address: String,
    },
    /// Block this device's traffic while a VPN is set to connect and is not
    /// connected, or not; and whether the local network stays reachable
    /// meanwhile (kept as it was when omitted).
    #[effect(idempotent, row, label = "Block traffic while the VPN is down")]
    SetKillSwitch {
        #[param("The VPN's id or name")]
        connection: String,
        on: bool,
        #[param("Keep printers, file shares and the router reachable")]
        allow_local: Option<bool>,
    },
    /// How often a `WireGuard` tunnel tells its server it is there; 0 never.
    #[effect(job, row, idempotent, label = "Keepalive")]
    SetVpnKeepalive {
        #[param("The VPN's id or name")]
        connection: String,
        #[param("Seconds")]
        seconds: u32,
    },
    /// Save a VPN as a file to set it up elsewhere: its keys are in it.
    #[effect(attended, row, label = "Export")]
    ExportVpn {
        #[param("The VPN's id or name")]
        connection: String,
    },
    /// Send this device's traffic through another of the person's devices,
    /// or stop.
    #[effect(job, row, idempotent, label = "Through your device")]
    SetDeviceVpn {
        #[param("The other device's id")]
        device: String,
        on: bool,
    },
    /// Block this device's traffic while the tunnel to the device it routes
    /// through is down, or let it go out directly.
    #[effect(idempotent, label = "Block traffic while the tunnel is down")]
    SetDeviceVpnKillSwitch { on: bool },
    /// Let the person's other devices use this one as a VPN, or not.
    #[effect(idempotent, label = "Let my other devices use this device as a VPN")]
    SetDeviceExit { on: bool },
    /// Stop another of the person's devices using this one as a VPN.
    #[effect(row, label = "Stop")]
    StopDeviceExit {
        #[param("The other device's id")]
        device: String,
    },
}

/// Answer `effect`.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: VpnEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let begun = super::area(app)
        .ok_or_else(crate::area::missing)?
        .model()
        .borrow_mut()
        .begin_vpn(effect)?;
    Ok(super::answer_begun(begun, cx))
}

/// A way out the VPN list offers beside the profiles, from another source
/// ([`VpnEntries`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VpnEntry {
    /// Stable, unique among the source's entries.
    pub id: String,
    pub name: String,
    /// What it is.
    pub detail: String,
    pub active: bool,
}

/// What else the VPN list offers, from another provider. The source binds
/// each entry's switch to its own effect.
pub(crate) trait VpnEntries {
    fn entries(&self) -> Vec<VpnEntry>;
    /// `toggle`, bound to switching `entry`.
    fn bind(&self, cx: &PaneCx, entry: &VpnEntry, toggle: Toggle) -> Toggle;
}

/// The form of a new `WireGuard` tunnel.
struct WireguardForm {
    name: LocalState<String>,
    address: LocalState<String>,
    server_public_key: LocalState<String>,
    endpoint: LocalState<String>,
    dns: LocalState<String>,
    keepalive: LocalState<String>,
    private_key: Arc<Mutex<String>>,
    /// The public key of the private key generated here.
    public_key: Option<String>,
    /// A pasted configuration and its name.
    config_name: LocalState<String>,
    config: Arc<Mutex<String>>,
}

impl Default for WireguardForm {
    fn default() -> Self {
        Self {
            name: LocalState::new(String::new()),
            address: LocalState::new(String::new()),
            server_public_key: LocalState::new(String::new()),
            endpoint: LocalState::new(String::new()),
            dns: LocalState::new(String::new()),
            keepalive: LocalState::new("25".into()),
            private_key: Arc::new(Mutex::new(String::new())),
            public_key: None,
            config_name: LocalState::new(String::new()),
            config: Arc::new(Mutex::new(String::new())),
        }
    }
}

impl Drop for WireguardForm {
    fn drop(&mut self) {
        for buffer in [&self.private_key, &self.config] {
            if let Ok(mut secret) = buffer.lock() {
                secret.zeroize();
            }
        }
    }
}

/// The form of a new plugin profile.
struct PluginForm {
    name: LocalState<String>,
    server: LocalState<String>,
}

impl Default for PluginForm {
    fn default() -> Self {
        Self {
            name: LocalState::new(String::new()),
            server: LocalState::new(String::new()),
        }
    }
}

/// What the VPN part of the model keeps.
#[derive(Default)]
pub(crate) struct VpnState {
    /// Each VPN's settings, read when its page opens.
    pub(super) settings: HashMap<String, VpnProfileSettings>,
    /// Why a VPN's settings could not be read.
    pub(super) errors: HashMap<String, String>,
    /// VPNs whose settings are being read.
    reading: HashSet<String>,
    /// VPNs being brought up or down.
    changing: HashSet<String>,
    wireguard: WireguardForm,
    plugin: PluginForm,
    /// A file being picked to import, with the kind it must be.
    import: Option<(ui::portal::PortalChannel, Option<&'static str>)>,
    /// Where a VPN is being exported to.
    export: Option<(ui::portal::OpenFileChannel, String)>,
    /// The other ways out the list offers.
    entries: Option<Box<dyn VpnEntries>>,
    /// The VPN whose page was asked for before the VPNs were read.
    wanted: Option<String>,
    /// The person's devices as exits, and this one as theirs.
    pub(super) devices: devices::DeviceVpnState,
}

/// A configuration file's text, read whole up to the size a VPN profile can
/// be, into a buffer zeroed when dropped.
fn read_config(path: &Path) -> Result<Zeroizing<String>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut text = Zeroizing::new(String::new());
    file.take(MAX_VPN_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| t("net-vpn-file-unreadable"))?;
    Ok(text)
}

/// The kind of tunnel `path` sets up, by its name or, with none, `wanted`.
fn kind_of_path(path: &Path, wanted: Option<&'static str>) -> Result<&'static str, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match (
        os_actions::network_settings::vpn_kind_of_file(&name),
        wanted,
    ) {
        (Some(kind), Some(wanted)) if kind != wanted => Err(t("net-vpn-file-wrong-kind")),
        (Some(kind), _) => Ok(kind),
        (None, Some(wanted)) => Ok(wanted),
        (None, None) => Err(t("net-vpn-file-unknown")),
    }
}

/// Check the file at `path` and set it up as a tunnel: what an import lands.
fn import_file(
    service: &dyn super::NetworkService,
    path: &Path,
    wanted: Option<&'static str>,
) -> Result<String, String> {
    let kind = kind_of_path(path, wanted)?;
    let text = read_config(path)?;
    os_actions::network_settings::validate_vpn_config(kind, &text)?;
    drop(text);
    service.create_vpn(&VpnSetup::File {
        kind,
        path: path.to_path_buf(),
    })
}

/// What a tunnel's protocol is called: `WireGuard`, an installed plugin's
/// name, else what its service type names.
fn kind_shown(kind: &str, service: Option<&str>, plugins: &[VpnPlugin]) -> String {
    match (kind, service) {
        ("wireguard", _) => "WireGuard".into(),
        (_, Some(service)) => plugins
            .iter()
            .find(|plugin| plugin.service == service)
            .map_or_else(
                || os_actions::network_settings::vpn_service_name(service),
                |plugin| plugin.name.clone(),
            ),
        _ => t("net-vpn-kind-other"),
    }
}

/// Turn off each of `active` (ids and names) that carries all traffic;
/// what could not be turned off is said.
pub(super) fn full_tunnels_off(
    service: &dyn super::NetworkService,
    active: &[(String, String)],
) -> Result<(), String> {
    let mut failed = Vec::new();
    for (uuid, name) in active {
        if !service
            .vpn_settings(uuid)
            .is_ok_and(|settings| settings.all_traffic)
        {
            continue;
        }
        let off = NetworkChange::Vpn {
            uuid: uuid.clone(),
            name: name.clone(),
            on: false,
        };
        if let Err(error) = service.change(&off) {
            tracing::warn!(target: "tilo::settings", vpn = %uuid, %error, "a VPN carrying all traffic did not turn off");
            failed.push(t_args(
                "net-vpn-switch-err",
                &[("name", name.as_str().into()), ("error", error.into())],
            ));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join(" "))
    }
}

/// The packet sizes a tunnel may be set to: what IPv4 must carry, up to
/// jumbo frames.
const MTU_RANGE: std::ops::RangeInclusive<u32> = 576..=9000;

/// What a tunnel's MTU starts at when the person takes it over: what
/// `WireGuard` picks for itself, and Ethernet's for the others.
fn custom_mtu(kind: &str) -> u32 {
    if kind == "wireguard" { 1420 } else { 1500 }
}

/// A number of seconds or bytes from a number field: whole, never negative.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=65535 and rounded first"
)]
fn whole(value: f64) -> u32 {
    value.clamp(0.0, 65_535.0).round() as u32
}

impl VpnState {
    /// Where a new `WireGuard` tunnel's private key is kept.
    pub(super) fn wireguard_private_key(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.wireguard.private_key)
    }

    /// The public key of the private key given for a new tunnel.
    pub(super) fn set_wireguard_public_key(&mut self, public: String) {
        self.wireguard.public_key = Some(public);
    }
}

impl NetworkModel {
    /// Offer the VPN list `entries` beside the profiles.
    #[allow(dead_code, reason = "the seam another source of exits plugs into")]
    pub(crate) fn set_vpn_entries(&mut self, entries: Box<dyn VpnEntries>) {
        self.vpn.entries = Some(entries);
        self.changed = true;
    }

    /// The VPN `connection` names: by its id, else by its name.
    fn vpn(&self, connection: &str) -> Result<VpnInfo, String> {
        self.vpns
            .iter()
            .find(|vpn| vpn.uuid == connection)
            .or_else(|| self.vpns.iter().find(|vpn| vpn.name == connection))
            .cloned()
            .ok_or_else(|| t_args("net-vpn-unknown", &[("name", connection.into())]))
    }

    /// The VPN in use, or Off: what the VPN row on the first page says.
    pub(super) fn vpn_summary(&self) -> String {
        if self.read_errors.vpns.is_some() {
            return t("net-vpn-unread");
        }
        self.vpns
            .iter()
            .find(|vpn| vpn.active)
            .map_or_else(|| t("net-vpn-off"), |vpn| vpn.name.clone())
    }

    /// Read `uuid`'s settings on a worker, unless they are being read.
    fn read_vpn(&mut self, uuid: &str) {
        if !self.vpn.reading.insert(uuid.to_owned()) {
            return;
        }
        let service = Arc::clone(&self.service);
        let uuid = uuid.to_owned();
        self.start_job(
            "settings-vpn-read",
            Box::new(move || Landed::VpnRead {
                result: service.vpn_settings(&uuid),
                uuid,
            }),
        );
    }

    /// Take in `uuid`'s settings.
    pub(super) fn adopt_vpn_settings(&mut self, uuid: &str, settings: VpnProfileSettings) {
        self.vpn.reading.remove(uuid);
        self.vpn.errors.remove(uuid);
        self.vpn.settings.insert(uuid.to_owned(), settings);
        self.changed = true;
    }

    /// Start a picker for a file of `kind` (either kind for `None`).
    fn pick_vpn_file(&mut self, kind: Option<&'static str>) {
        if self.vpn.import.is_some() {
            return;
        }
        let mut request =
            ui::portal::PickRequest::open(&t("net-vpn-import-title"), &t("net-vpn-import"))
                .thread_name("settings-vpn-import");
        if kind != Some("openvpn") {
            request = request.filter(ui::portal::FileFilter::new("WireGuard").glob("*.conf"));
        }
        if kind != Some("wireguard") {
            request = request.filter(ui::portal::FileFilter::new("OpenVPN").glob("*.ovpn"));
        }
        self.vpn.import = Some((request.all_files().send(), kind));
    }

    /// Set up the file at `path` on a worker: what a picked file starts.
    pub(super) fn import_vpn_path(&mut self, path: PathBuf, wanted: Option<&'static str>) {
        let service = Arc::clone(&self.service);
        self.notice = Some(t("net-vpn-importing"));
        self.start_job(
            "settings-vpn-import",
            Box::new(move || Landed::VpnCreated(import_file(service.as_ref(), &path, wanted))),
        );
    }

    /// Take in a file picked to import or a place picked to export to.
    pub(super) fn poll_vpn_pickers(&mut self) {
        if let Some(outcome) = self
            .vpn
            .import
            .as_ref()
            .and_then(|(picker, _)| picker.try_recv().ok())
        {
            let wanted = self.vpn.import.take().and_then(|(_, kind)| kind);
            match outcome {
                ui::portal::PortalOutcome::Opened(path) => self.import_vpn_path(path, wanted),
                ui::portal::PortalOutcome::Cancelled => {}
                ui::portal::PortalOutcome::Failed(error) => {
                    self.notice = Some(t_args("net-vpn-import-err", &[("error", error.into())]));
                }
            }
            self.changed = true;
        }
        if let Some(outcome) = self
            .vpn
            .export
            .as_ref()
            .and_then(|(picker, _)| picker.try_recv().ok())
        {
            let uuid = self
                .vpn
                .export
                .take()
                .map(|(_, uuid)| uuid)
                .unwrap_or_default();
            match outcome {
                ui::portal::OpenFileOutcome::Opened(opened) => {
                    let service = Arc::clone(&self.service);
                    let mut file = opened.into_file();
                    self.start_job(
                        "settings-vpn-export",
                        Box::new(move || {
                            Landed::VpnExported(service.export_vpn(&uuid).and_then(|text| {
                                use std::io::Write as _;
                                file.write_all(text.as_bytes())
                                    .and_then(|()| file.sync_all())
                                    .map_err(|error| error.to_string())
                            }))
                        }),
                    );
                }
                ui::portal::OpenFileOutcome::Cancelled => {}
                ui::portal::OpenFileOutcome::Failed(error) => {
                    self.notice = Some(t_args("net-vpn-export-err", &[("error", error.into())]));
                }
            }
            self.changed = true;
        }
        if self.loaded
            && let Some(wanted) = self.vpn.wanted.take()
            && let Ok(vpn) = self.vpn(&wanted)
            && self.page_key().as_deref() == Some(PAGE_VPN)
        {
            self.read_vpn(&vpn.uuid);
            self.open_page(&profile_page_key(&vpn.uuid), vpn.name);
        }
        // A VPN page shown before its settings were read reads them.
        if let Some(uuid) = self
            .page_key()
            .and_then(|key| key.strip_prefix(PAGE_PROFILE).map(str::to_owned))
            && !self.vpn.settings.contains_key(&uuid)
            && !self.vpn.errors.contains_key(&uuid)
            && self.vpns.iter().any(|vpn| vpn.uuid == uuid)
        {
            self.read_vpn(&uuid);
        }
    }

    /// A change to `vpn`'s profile.
    fn vpn_patch(&mut self, connection: &str, patch: VpnProfilePatch) -> Result<Begun, String> {
        let vpn = self.vpn(connection)?;
        if let Some(settings) = self.vpn.settings.get_mut(&vpn.uuid) {
            if let Some(name) = &patch.name {
                settings.name.clone_from(name);
            }
            if let Some(on) = patch.autoconnect {
                settings.autoconnect = on;
            }
            if let Some(on) = patch.all_traffic {
                settings.all_traffic = on;
            }
            if let Some(dns) = &patch.dns {
                settings.dns.clone_from(dns);
            }
            if let Some(mtu) = patch.mtu {
                settings.mtu = mtu;
            }
            if let Some(addresses) = &patch.allowed_ips {
                settings.allowed_ips.clone_from(addresses);
            }
        }
        Ok(self.change(
            NetworkChange::VpnProfile {
                uuid: vpn.uuid,
                name: vpn.name,
                patch,
            },
            None,
        ))
    }

    /// The name servers `connection` asks, as last read.
    fn vpn_dns(&self, connection: &str) -> Result<Vec<String>, String> {
        let vpn = self.vpn(connection)?;
        self.vpn
            .settings
            .get(&vpn.uuid)
            .map(|settings| settings.dns.clone())
            .ok_or_else(|| t("net-vpn-settings-unread"))
    }

    /// The addresses `connection`, a `WireGuard` tunnel, routes, as last
    /// read.
    fn vpn_routed(&self, connection: &str) -> Result<Vec<String>, String> {
        let vpn = self.vpn(connection)?;
        let settings = self
            .vpn
            .settings
            .get(&vpn.uuid)
            .ok_or_else(|| t("net-vpn-settings-unread"))?;
        if settings.kind != "wireguard" {
            return Err(t("net-vpn-allowed-wireguard"));
        }
        Ok(settings.allowed_ips.clone())
    }

    /// Bring `connection` up or down. Turning it off is on purpose: its
    /// kill switch's block lifts, and one blocking while it is not up only
    /// lifts.
    fn vpn_switch(&mut self, connection: &str, on: bool) -> Result<Begun, String> {
        let vpn = self.vpn(connection)?;
        if !on {
            self.release_profile(&vpn.uuid)?;
            if !vpn.active && self.unblock(&vpn.uuid) {
                self.notice = Some(t_args(
                    "net-vpn-disconnected",
                    &[("name", vpn.name.as_str().into())],
                ));
                return Ok(Begun::Done);
            }
        }
        if !self.vpn.changing.insert(vpn.uuid.clone()) {
            return Err(t_args(
                "net-vpn-busy",
                &[("name", vpn.name.as_str().into())],
            ));
        }
        let notice = t_args(
            if on {
                "net-vpn-connecting"
            } else {
                "net-vpn-disconnecting"
            },
            &[("name", vpn.name.as_str().into())],
        );
        let change = NetworkChange::Vpn {
            uuid: vpn.uuid.clone(),
            name: vpn.name.clone(),
            on,
        };
        if !on {
            return Ok(self.change(change, Some(notice)));
        }
        // One tunnel carries all traffic at a time: one that does turns off
        // the others that do, whose routes would fight over it.
        let others = self.active_vpns(Some(&vpn.uuid));
        self.notice = Some(notice);
        self.in_flight += 1;
        let service = Arc::clone(&self.service);
        Ok(Begun::work(move || {
            let full_tunnel = service
                .vpn_settings(&vpn.uuid)
                .is_ok_and(|settings| settings.all_traffic);
            if full_tunnel {
                // What stays on is read back and shown as it stands.
                let _ = full_tunnels_off(&*service, &others);
            }
            let result = service.change(&change);
            Landed::VpnOn {
                change,
                result,
                full_tunnel,
            }
        }))
    }

    /// The VPNs up now, but `except`: their ids and names.
    pub(super) fn active_vpns(&self, except: Option<&str>) -> Vec<(String, String)> {
        self.vpns
            .iter()
            .filter(|vpn| vpn.active && Some(vpn.uuid.as_str()) != except)
            .map(|vpn| (vpn.uuid.clone(), vpn.name.clone()))
            .collect()
    }

    /// Begin `effect`.
    #[expect(clippy::too_many_lines, reason = "one arm per effect")]
    pub(super) fn begin_vpn(&mut self, effect: VpnEffect) -> Result<Begun, String> {
        self.changed = true;
        let service = Arc::clone(&self.service);
        let open = |model: &mut Self, key: &str, title: String| {
            model.open_page(key, title);
            Ok(Begun::Done)
        };
        match effect {
            VpnEffect::ShowVpns => {
                self.cx.pages().clear();
                open(self, PAGE_VPN, t("net-vpn"))
            }
            VpnEffect::ShowVpnProfile { connection } => {
                let vpn = match self.vpn(&connection) {
                    Ok(vpn) => vpn,
                    // Asked for at launch: the page opens once the VPNs are
                    // read.
                    Err(_) if !self.loaded => {
                        self.vpn.wanted = Some(connection);
                        self.cx.pages().clear();
                        return open(self, PAGE_VPN, t("net-vpn"));
                    }
                    Err(error) => return Err(error),
                };
                if self.page_key().as_deref() != Some(PAGE_VPN) {
                    self.cx.pages().clear();
                    self.open_page(PAGE_VPN, t("net-vpn"));
                }
                self.vpn.errors.remove(&vpn.uuid);
                self.read_vpn(&vpn.uuid);
                open(self, &profile_page_key(&vpn.uuid), vpn.name)
            }
            VpnEffect::ShowAddVpn => open(self, PAGE_ADD, t("net-vpn-add")),
            VpnEffect::ShowAddWireguard => {
                open(self, PAGE_ADD_WIREGUARD, t("net-vpn-new-wireguard"))
            }
            VpnEffect::ShowAddPluginVpn { service } => {
                let plugin = self
                    .plugins
                    .iter()
                    .find(|plugin| plugin.service == service)
                    .cloned()
                    .ok_or_else(|| t("net-vpn-no-plugin"))?;
                open(
                    self,
                    &plugin_page_key(&plugin.service),
                    t_args("net-vpn-new-plugin", &[("kind", plugin.name.into())]),
                )
            }
            VpnEffect::ImportWireguard => {
                self.pick_vpn_file(Some("wireguard"));
                Ok(Begun::Done)
            }
            VpnEffect::ImportOpenvpn => {
                self.pick_vpn_file(Some("openvpn"));
                Ok(Begun::Done)
            }
            VpnEffect::ImportVpnFile => {
                self.pick_vpn_file(None);
                Ok(Begun::Done)
            }
            VpnEffect::GenerateWireguardKeys => {
                let (private, public) = os_actions::network_settings::generate_wireguard_keys()?;
                if let Ok(mut buffer) = self.vpn.wireguard.private_key.lock() {
                    buffer.zeroize();
                    buffer.push_str(&private);
                }
                self.vpn.wireguard.public_key = Some(public);
                Ok(Begun::Done)
            }
            VpnEffect::CreateWireguard {
                name,
                address,
                server_public_key,
                endpoint,
                allowed_ips,
                dns,
                keepalive,
                private_key,
            } => {
                let form = &self.vpn.wireguard;
                let name = name
                    .or_else(|| non_blank(form.name.get()))
                    .ok_or_else(|| t("net-vpn-needs-name"))?;
                let typed = take_secret(&form.private_key);
                let keepalive = match keepalive {
                    Some(seconds) => seconds,
                    None => form.keepalive.get().trim().parse::<u32>().unwrap_or(0),
                };
                let tunnel = WireGuardTunnel {
                    private_key: private_key.map_or(typed, Zeroizing::new),
                    address: address
                        .unwrap_or_else(|| form.address.get())
                        .trim()
                        .to_owned(),
                    dns: list_of(&dns.unwrap_or_else(|| form.dns.get())),
                    peer_public_key: server_public_key
                        .unwrap_or_else(|| form.server_public_key.get())
                        .trim()
                        .to_owned(),
                    endpoint: endpoint
                        .unwrap_or_else(|| form.endpoint.get())
                        .trim()
                        .to_owned(),
                    // A tunnel set up here routes everything; its profile
                    // page edits what it routes.
                    allowed_ips: list_of(allowed_ips.as_deref().unwrap_or(ALL_ADDRESSES)),
                    keepalive: (keepalive > 0).then_some(keepalive),
                };
                let text = tunnel.config()?;
                self.vpn.wireguard.public_key = None;
                self.notice = Some(t("net-vpn-creating"));
                Ok(Begun::work(move || {
                    Landed::VpnCreated(service.create_vpn(&VpnSetup::Text {
                        kind: "wireguard",
                        name,
                        text,
                    }))
                }))
            }
            VpnEffect::ImportWireguardConfig { name, config } => {
                let form = &self.vpn.wireguard;
                let name = name
                    .or_else(|| non_blank(form.config_name.get()))
                    .ok_or_else(|| t("net-vpn-needs-name"))?;
                let typed = take_secret(&form.config);
                let text = config.map_or(typed, Zeroizing::new);
                os_actions::network_settings::validate_vpn_config("wireguard", &text)?;
                self.notice = Some(t("net-vpn-creating"));
                Ok(Begun::work(move || {
                    Landed::VpnCreated(service.create_vpn(&VpnSetup::Text {
                        kind: "wireguard",
                        name,
                        text,
                    }))
                }))
            }
            VpnEffect::CreatePluginVpn {
                service: plugin,
                name,
                server,
            } => {
                if !self.plugins.iter().any(|known| known.service == plugin) {
                    return Err(t("net-vpn-no-plugin"));
                }
                let form = &self.vpn.plugin;
                let name = name
                    .or_else(|| non_blank(form.name.get()))
                    .ok_or_else(|| t("net-vpn-needs-name"))?;
                let server = server
                    .or_else(|| non_blank(form.server.get()))
                    .ok_or_else(|| t("net-vpn-needs-server"))?;
                self.notice = Some(t("net-vpn-creating"));
                Ok(Begun::work(move || {
                    Landed::VpnCreated(service.create_vpn(&VpnSetup::Plugin {
                        service: plugin,
                        name,
                        server: server.trim().to_owned(),
                    }))
                }))
            }
            VpnEffect::SetVpnConnected { connection, on } => self.vpn_switch(&connection, on),
            VpnEffect::Connect { connection } => self.vpn_switch(&connection, true),
            VpnEffect::Disconnect { connection } => self.vpn_switch(&connection, false),
            VpnEffect::DeleteConnection { connection } => {
                let vpn = self.vpn(&connection)?;
                Ok(self.change(
                    NetworkChange::VpnDelete {
                        uuid: vpn.uuid,
                        name: vpn.name,
                    },
                    None,
                ))
            }
            VpnEffect::SetVpnName { connection, name } => {
                let name = non_blank(name).ok_or_else(|| t("net-vpn-needs-name"))?;
                self.vpn_patch(
                    &connection,
                    VpnProfilePatch {
                        name: Some(name.trim().to_owned()),
                        ..VpnProfilePatch::default()
                    },
                )
            }
            VpnEffect::SetVpnAutoconnect { connection, on } => self.vpn_patch(
                &connection,
                VpnProfilePatch {
                    autoconnect: Some(on),
                    ..VpnProfilePatch::default()
                },
            ),
            VpnEffect::SetVpnAllTraffic { connection, on } => self.vpn_patch(
                &connection,
                VpnProfilePatch {
                    all_traffic: Some(on),
                    ..VpnProfilePatch::default()
                },
            ),
            VpnEffect::AddVpnDnsServer { connection, server } => {
                let server = server.trim();
                if server.parse::<std::net::IpAddr>().is_err() {
                    return Err(t_args("net-vpn-dns-not-ip", &[("server", server.into())]));
                }
                let mut dns = self.vpn_dns(&connection)?;
                if !dns.iter().any(|known| known == server) {
                    dns.push(server.to_owned());
                }
                self.vpn_patch(
                    &connection,
                    VpnProfilePatch {
                        dns: Some(dns),
                        ..VpnProfilePatch::default()
                    },
                )
            }
            VpnEffect::RemoveVpnDnsServer { connection, server } => {
                let mut dns = self.vpn_dns(&connection)?;
                dns.retain(|known| known != server.trim());
                self.vpn_patch(
                    &connection,
                    VpnProfilePatch {
                        dns: Some(dns),
                        ..VpnProfilePatch::default()
                    },
                )
            }
            VpnEffect::SetVpnMtu { connection, mtu } => {
                if mtu.is_some_and(|mtu| !MTU_RANGE.contains(&mtu)) {
                    return Err(t("net-vpn-mtu-range"));
                }
                self.vpn_patch(
                    &connection,
                    VpnProfilePatch {
                        mtu: Some(mtu),
                        ..VpnProfilePatch::default()
                    },
                )
            }
            VpnEffect::SetVpnServer {
                connection,
                endpoint,
            } => self.vpn_patch(
                &connection,
                VpnProfilePatch {
                    endpoint: Some(endpoint.trim().to_owned()),
                    ..VpnProfilePatch::default()
                },
            ),
            VpnEffect::AddVpnRoutedAddress {
                connection,
                address,
            } => {
                let address = address.trim();
                if !is_routed_address(address) {
                    return Err(t_args(
                        "net-vpn-allowed-not-network",
                        &[("address", address.into())],
                    ));
                }
                let mut addresses = self.vpn_routed(&connection)?;
                if !addresses.iter().any(|known| known == address) {
                    addresses.push(address.to_owned());
                }
                self.vpn_patch(
                    &connection,
                    VpnProfilePatch {
                        allowed_ips: Some(addresses),
                        ..VpnProfilePatch::default()
                    },
                )
            }
            VpnEffect::RemoveVpnRoutedAddress {
                connection,
                address,
            } => {
                let mut addresses = self.vpn_routed(&connection)?;
                addresses.retain(|known| known != address.trim());
                self.vpn_patch(
                    &connection,
                    VpnProfilePatch {
                        allowed_ips: Some(addresses),
                        ..VpnProfilePatch::default()
                    },
                )
            }
            VpnEffect::SetKillSwitch {
                connection,
                on,
                allow_local,
            } => self.set_kill_switch(&connection, on, allow_local),
            VpnEffect::SetVpnKeepalive {
                connection,
                seconds,
            } => self.vpn_patch(
                &connection,
                VpnProfilePatch {
                    keepalive: Some((seconds > 0).then_some(seconds)),
                    ..VpnProfilePatch::default()
                },
            ),
            VpnEffect::ExportVpn { connection } => {
                let vpn = self.vpn(&connection)?;
                if self.vpn.export.is_none() {
                    let suggested = if vpn.kind == "wireguard" {
                        format!(
                            "{}.conf",
                            os_actions::network_settings::interface_name(&vpn.name)
                        )
                    } else {
                        format!("{}.ovpn", vpn.name)
                    };
                    let picker = ui::portal::PickRequest::save(
                        &t("net-vpn-export-title"),
                        &t("net-vpn-export"),
                        &suggested,
                    )
                    .thread_name("settings-vpn-export")
                    .send_save_file();
                    self.vpn.export = Some((picker, vpn.uuid));
                }
                Ok(Begun::Done)
            }
            VpnEffect::AskWireguardPrivateKey => {
                self.ask_private_key();
                Ok(Begun::Done)
            }
            VpnEffect::SetDeviceVpn { device, on } => self.route_through_device(&device, on),
            VpnEffect::SetDeviceVpnKillSwitch { on } => self.set_device_kill_switch(on),
            VpnEffect::SetDeviceExit { on } => self.share_device(on),
            VpnEffect::StopDeviceExit { device } => self.stop_exit_user(&device),
        }
    }

    /// Report a landed VPN change.
    pub(super) fn vpn_changed(
        &mut self,
        change: &NetworkChange,
        result: Result<(), String>,
    ) -> Result<String, String> {
        match (change, result) {
            (NetworkChange::Vpn { uuid, name, on }, result) => {
                self.vpn.changing.remove(uuid);
                match result {
                    Ok(()) => {
                        if let Some(vpn) = self.vpns.iter_mut().find(|vpn| &vpn.uuid == uuid) {
                            vpn.active = *on;
                        }
                        Ok(t_args(
                            if *on {
                                "net-vpn-connected"
                            } else {
                                "net-vpn-disconnected"
                            },
                            &[("name", name.as_str().into())],
                        ))
                    }
                    Err(error) => Err(t_args(
                        "net-vpn-switch-err",
                        &[("name", name.as_str().into()), ("error", error.into())],
                    )),
                }
            }
            (NetworkChange::VpnProfile { uuid, name, .. }, result) => match result {
                Ok(()) => {
                    // What `NetworkManager` made of it is read again.
                    self.vpn.settings.remove(uuid);
                    self.read_vpn(uuid);
                    Ok(t_args(
                        "net-notice-policy-ok",
                        &[("name", name.as_str().into())],
                    ))
                }
                Err(error) => {
                    self.vpn.settings.remove(uuid);
                    self.read_vpn(uuid);
                    Err(t_args("net-notice-policy-err", &[("error", error.into())]))
                }
            },
            (NetworkChange::VpnDelete { uuid, name }, Ok(())) => {
                self.forget_kill_switch(uuid);
                self.vpns.retain(|vpn| &vpn.uuid != uuid);
                self.vpn.settings.remove(uuid);
                self.cx
                    .pages()
                    .close(Category::Network, &profile_page_key(uuid));
                Ok(t_args("net-vpn-deleted", &[("name", name.as_str().into())]))
            }
            (NetworkChange::VpnDelete { name, .. }, Err(error)) => Err(t_args(
                "net-vpn-delete-err",
                &[("name", name.as_str().into()), ("error", error.into())],
            )),
            (_, result) => result.map(|()| String::new()),
        }
    }

    /// Take in a VPN set up: its page opens in place of the form.
    pub(super) fn vpn_created(&mut self, result: Result<String, String>) -> Result<String, String> {
        match result {
            Ok(uuid) => {
                self.vpn.wireguard = WireguardForm::default();
                self.vpn.plugin = PluginForm::default();
                self.start_snapshot();
                self.read_vpn(&uuid);
                let pages = self.cx.pages();
                pages.clear();
                pages.open(Category::Network, PAGE_VPN, t("net-vpn"));
                pages.open(Category::Network, profile_page_key(&uuid), t("net-vpn"));
                Ok(t("net-vpn-created"))
            }
            Err(error) => Err(t_args("net-vpn-create-err", &[("error", error.into())])),
        }
    }

    /// The published row of `vpn`.
    fn vpn_live(&self, vpn: &VpnInfo) -> LiveRow {
        let offer = |effect: VpnEffect| SettingsEffect::from(effect).bound();
        // A setting is offered with its value to give; the connection is
        // the row's own.
        let asking =
            |effect: VpnEffect, asking: &[&str]| SettingsEffect::from(effect).bound_asking(asking);
        let connection = vpn.uuid.clone();
        let on = self.set_to_connect(vpn);
        let mut row = LiveRow::new(
            format!("vpn.connection:{}", vpn.uuid),
            "vpn.connection",
            vpn.name.clone(),
        )
        .operation(offer(VpnEffect::ShowVpnProfile {
            connection: connection.clone(),
        }))
        .operation(asking(
            VpnEffect::SetVpnConnected {
                connection: connection.clone(),
                on: !on,
            },
            &["on"],
        ))
        .operation(asking(
            VpnEffect::SetKillSwitch {
                connection: connection.clone(),
                on: self.kill_switch(&vpn.uuid).is_none(),
                allow_local: None,
            },
            &["on", "allow_local"],
        ))
        .operation(offer(if on {
            VpnEffect::Disconnect {
                connection: connection.clone(),
            }
        } else {
            VpnEffect::Connect {
                connection: connection.clone(),
            }
        }))
        .operation(offer(VpnEffect::DeleteConnection {
            connection: connection.clone(),
        }))
        .operation(offer(VpnEffect::ExportVpn {
            connection: connection.clone(),
        }));
        if let Some(settings) = self.vpn.settings.get(&vpn.uuid) {
            row = Self::setting_operations(row, connection, settings);
        }
        row
    }

    /// `row` offering each setting of `settings`, with the value to give.
    fn setting_operations(
        mut row: LiveRow,
        connection: String,
        settings: &VpnProfileSettings,
    ) -> LiveRow {
        let asking =
            |effect: VpnEffect, asking: &[&str]| SettingsEffect::from(effect).bound_asking(asking);
        row = row
            .operation(asking(
                VpnEffect::SetVpnName {
                    connection: connection.clone(),
                    name: settings.name.clone(),
                },
                &["name"],
            ))
            .operation(asking(
                VpnEffect::SetVpnAutoconnect {
                    connection: connection.clone(),
                    on: !settings.autoconnect,
                },
                &["on"],
            ))
            .operation(asking(
                VpnEffect::SetVpnAllTraffic {
                    connection: connection.clone(),
                    on: !settings.all_traffic,
                },
                &["on"],
            ))
            .operation(asking(
                VpnEffect::AddVpnDnsServer {
                    connection: connection.clone(),
                    server: String::new(),
                },
                &["server"],
            ))
            .operation(asking(
                // Omitted, the MTU is chosen automatically.
                VpnEffect::SetVpnMtu {
                    connection: connection.clone(),
                    mtu: None,
                },
                &["mtu"],
            ));
        if let Some(server) = settings.dns.first() {
            row = row.operation(asking(
                VpnEffect::RemoveVpnDnsServer {
                    connection: connection.clone(),
                    server: server.clone(),
                },
                &["server"],
            ));
        }
        if settings.kind == "wireguard" {
            row = row
                .operation(asking(
                    VpnEffect::SetVpnServer {
                        connection: connection.clone(),
                        endpoint: settings.endpoint.clone().unwrap_or_default(),
                    },
                    &["endpoint"],
                ))
                .operation(asking(
                    VpnEffect::AddVpnRoutedAddress {
                        connection: connection.clone(),
                        address: String::new(),
                    },
                    &["address"],
                ))
                .operation(asking(
                    VpnEffect::SetVpnKeepalive {
                        connection: connection.clone(),
                        seconds: settings.keepalive.unwrap_or(0),
                    },
                    &["seconds"],
                ));
            if let Some(address) = settings.allowed_ips.first() {
                row = row.operation(asking(
                    VpnEffect::RemoveVpnRoutedAddress {
                        connection,
                        address: address.clone(),
                    },
                    &["address"],
                ));
            }
        }
        row
    }

    /// One row per VPN, per device to route through and per device using
    /// this one.
    pub(super) fn vpn_rows(&self) -> Vec<LiveRow> {
        let mut rows: Vec<LiveRow> = self.vpns.iter().map(|vpn| self.vpn_live(vpn)).collect();
        rows.extend(self.device_vpn_rows());
        rows
    }

    /// The VPN pages, by key; `None` for a key that is not one.
    pub(super) fn vpn_pages(&self, key: &str) -> Option<Vec<SettingsSection>> {
        Some(match key {
            PAGE_VPN => std::iter::once(self.vpn_list())
                .chain(self.device_exit_section())
                .collect(),
            PAGE_ADD => vec![self.add_vpn()],
            PAGE_ADD_WIREGUARD => self.add_wireguard(),
            _ => {
                if let Some(service) = key.strip_prefix(PAGE_ADD_PLUGIN) {
                    vec![self.add_plugin(service)]
                } else {
                    self.vpn_profile(key.strip_prefix(PAGE_PROFILE)?)
                }
            }
        })
    }

    /// The switch that brings `vpn` up or down.
    fn vpn_switch_control(&self, vpn: &VpnInfo, live: &ui::tilo_actions::Resource) -> Toggle {
        let connection = vpn.uuid.clone();
        let toggle = Toggle::from_state(&LocalState::new(self.set_to_connect(vpn)))
            .with_id(&format!("settings.network.vpn.switch.{}", vpn.uuid))
            .with_accessible_name(&vpn.name);
        let toggle = self.cx.bind_on_row(
            live,
            toggle,
            effect(VpnEffect::SET_VPN_CONNECTED).map(move |on: bool| (connection.clone(), on)),
        );
        // While it changes the switch waits; it never turns back on a
        // switch its row does not offer.
        if self.vpn.changing.contains(&vpn.uuid) {
            toggle.with_enabled(false)
        } else {
            toggle
        }
    }

    fn vpn_state(&self, vpn: &VpnInfo) -> String {
        if !self.vpn.changing.contains(&vpn.uuid) && !vpn.active && self.blocking(&vpn.uuid) {
            return t("net-vpn-blocking");
        }
        if self.vpn.changing.contains(&vpn.uuid) {
            t(if vpn.active {
                "net-vpn-disconnecting-short"
            } else {
                "net-vpn-connecting-short"
            })
        } else {
            t(if vpn.active {
                "net-connected"
            } else {
                "net-vpn-off"
            })
        }
    }

    fn vpn_kind(&self, vpn: &VpnInfo) -> String {
        let service = vpn.service.as_deref().or_else(|| {
            self.vpn
                .settings
                .get(&vpn.uuid)
                .and_then(|settings| settings.service.as_deref())
        });
        kind_shown(&vpn.kind, service, &self.plugins)
    }

    /// The VPN list: every tunnel with its switch, the other ways out, and
    /// Add VPN.
    fn vpn_list(&self) -> SettingsSection {
        let mut content: Vec<Item> = Vec::new();
        if let Some(notice) = &self.notice {
            content.push(note(notice));
        }
        if let Some(error) = &self.read_errors.vpns {
            content.push(row(error_caption(&t_args(
                "net-vpn-error",
                &[("error", error.as_str().into())],
            ))));
        }
        let mut lines: Vec<El> = self
            .vpns
            .iter()
            .map(|vpn| {
                let resource = self.vpn_live(vpn).into_resource();
                let line = ListRow::new(
                    format!("settings.network.vpn.{}", vpn.uuid),
                    vpn.name.clone(),
                )
                .icon(Icon::Shield)
                .secondary(self.vpn_state(vpn))
                .meta(self.vpn_kind(vpn))
                .trailing(Box::new(self.vpn_switch_control(vpn, &resource)))
                .disclosure();
                let open = VpnEffect::ShowVpnProfile {
                    connection: vpn.uuid.clone(),
                };
                Box::new(match self.cx.row_invocation(&resource, open) {
                    Some(binding) => line.on_action(binding),
                    None => line.with_enabled(false),
                }) as El
            })
            .collect();
        lines.extend(self.through_lines());
        if let Some(source) = &self.vpn.entries {
            lines.extend(source.entries().into_iter().map(|entry| {
                let toggle = source.bind(
                    &self.cx,
                    &entry,
                    Toggle::from_state(&LocalState::new(entry.active))
                        .with_id(&format!("settings.network.vpn.entry.{}", entry.id))
                        .with_accessible_name(&entry.name),
                );
                Box::new(
                    ListRow::new(
                        format!("settings.network.vpn.entry.{}", entry.id),
                        entry.name,
                    )
                    .icon(Icon::MonitorSmartphone)
                    .secondary(entry.detail)
                    .trailing(Box::new(toggle)),
                ) as El
            }));
        }
        if lines.is_empty() && self.read_errors.vpns.is_none() {
            content.push(note(&t("net-vpn-none")));
        }
        lines.push(Box::new(self.drill_row(
            "settings.network.vpn.add",
            Icon::Plus,
            &t("net-vpn-add"),
            None,
            VpnEffect::ShowAddVpn,
        )));
        content.push(block(Box::new(ui::widget::Section::new(lines))));
        content.extend(self.kill_switch_row());
        card(&t("net-vpn"), rows(content))
    }

    /// The kinds of VPN that can be added.
    fn add_vpn(&self) -> SettingsSection {
        let mut lines: Vec<El> = vec![Box::new(self.drill_row(
            "settings.network.vpn.add.wireguard",
            Icon::Shield,
            "WireGuard",
            None,
            VpnEffect::ShowAddWireguard,
        ))];
        for plugin in &self.plugins {
            lines.push(Box::new(self.drill_row(
                &format!("settings.network.vpn.add.{}", plugin.service),
                Icon::Shield,
                &plugin.name,
                None,
                VpnEffect::ShowAddPluginVpn {
                    service: plugin.service.clone(),
                },
            )));
        }
        lines.push(Box::new(
            self.drill_row(
                "settings.network.vpn.import",
                Icon::Upload,
                &t("net-vpn-import-file"),
                None,
                VpnEffect::ImportVpnFile,
            )
            .secondary(t("net-vpn-import-caption")),
        ));
        let mut content = Vec::new();
        if let Some(notice) = &self.notice {
            content.push(note(notice));
        }
        content.push(block(Box::new(ui::widget::Section::new(lines))));
        card(&t("net-vpn-add"), rows(content))
    }

    /// A new `WireGuard` tunnel: from its keys and server, or from a pasted
    /// configuration.
    fn add_wireguard(&self) -> Vec<SettingsSection> {
        let form = &self.vpn.wireguard;
        let mut tunnel = vec![
            Self::text_field(&form.name, "network.vpn.wireguard.name", &t("net-vpn-name")),
            setting_row_described(
                &t("net-vpn-keys"),
                &t("net-vpn-keys-caption"),
                Box::new(self.button(
                    "settings.network.vpn.wireguard.keys",
                    &t("net-vpn-generate-keys"),
                    VpnEffect::GenerateWireguardKeys,
                )),
            ),
        ];
        if let Some(public) = &form.public_key {
            tunnel.push(key_row(&t("net-vpn-public-key"), public));
        }
        tunnel.extend([
            self.secret_row(
                "network.vpn.wireguard.private",
                &t("net-vpn-private-key"),
                &form.private_key,
                VpnEffect::AskWireguardPrivateKey,
            ),
            Self::text_field(
                &form.address,
                "network.vpn.wireguard.address",
                &t("net-vpn-address"),
            ),
            Self::text_field(
                &form.server_public_key,
                "network.vpn.wireguard.peer",
                &t("net-vpn-server-key"),
            ),
            Self::text_field(
                &form.endpoint,
                "network.vpn.wireguard.endpoint",
                &t("net-vpn-server"),
            ),
            Self::text_field(
                &form.dns,
                "network.vpn.wireguard.dns",
                &t("net-dns-servers"),
            ),
            Self::text_field(
                &form.keepalive,
                "network.vpn.wireguard.keepalive",
                &t("net-vpn-keepalive"),
            ),
            self.button_row(
                "settings.network.vpn.wireguard.create",
                &t("net-vpn-create"),
                VpnEffect::CreateWireguard {
                    name: None,
                    address: None,
                    server_public_key: None,
                    endpoint: None,
                    allowed_ips: None,
                    dns: None,
                    keepalive: None,
                    private_key: None,
                },
            ),
        ]);
        let mut first = Vec::new();
        if let Some(notice) = &self.notice {
            first.push(note(notice));
        }
        first.extend(tunnel);
        vec![
            card(&t("net-vpn-new-wireguard"), rows(first)),
            self.paste_wireguard(),
        ]
    }

    /// A `WireGuard` configuration pasted whole, kept in a secret buffer.
    fn paste_wireguard(&self) -> SettingsSection {
        let form = &self.vpn.wireguard;
        let initial = form.config.lock().map_or_else(
            |_| Zeroizing::new(String::new()),
            |value| Zeroizing::new(value.clone()),
        );
        card(
            &t("net-vpn-paste-title"),
            rows(vec![
                note(&t("net-vpn-paste-caption")),
                Self::text_field(
                    &form.config_name,
                    "network.vpn.wireguard.config-name",
                    &t("net-vpn-name"),
                ),
                field_row(
                    &t("net-vpn-config"),
                    Box::new(
                        TextInput::local()
                            .with_id("network.vpn.wireguard.config")
                            .with_external_buffer(Arc::clone(&form.config))
                            .with_initial_value(&initial)
                            .with_secret(true)
                            .multiline(true)
                            .with_placeholder(t("net-vpn-config")),
                    ),
                ),
                self.button_row(
                    "settings.network.vpn.wireguard.import",
                    &t("net-vpn-import"),
                    VpnEffect::ImportWireguardConfig {
                        name: None,
                        config: None,
                    },
                ),
            ]),
        )
    }

    /// A new profile of a plugin's type: its name and server; the rest is
    /// the plugin's own.
    fn add_plugin(&self, service: &str) -> SettingsSection {
        let form = &self.vpn.plugin;
        let name = self
            .plugins
            .iter()
            .find(|plugin| plugin.service == service)
            .map_or_else(|| service.to_owned(), |plugin| plugin.name.clone());
        let mut content = Vec::new();
        if let Some(notice) = &self.notice {
            content.push(note(notice));
        }
        content.extend([
            Self::text_field(&form.name, "network.vpn.plugin.name", &t("net-vpn-name")),
            Self::text_field(
                &form.server,
                "network.vpn.plugin.server",
                &t("net-vpn-server"),
            ),
            self.button_row(
                "settings.network.vpn.plugin.create",
                &t("net-vpn-create"),
                VpnEffect::CreatePluginVpn {
                    service: service.to_owned(),
                    name: None,
                    server: None,
                },
            ),
            note(&t("net-vpn-plugin-caption")),
        ]);
        card(&name, rows(content))
    }

    /// A VPN's page: its state, settings, Export and Delete.
    fn vpn_profile(&self, uuid: &str) -> Vec<SettingsSection> {
        let Some(vpn) = self.vpns.iter().find(|vpn| vpn.uuid == uuid) else {
            return vec![card(&t("net-vpn"), rows(vec![note(&t("net-vpn-gone"))]))];
        };
        let live = self.vpn_live(vpn).into_resource();
        let mut status = Vec::new();
        if let Some(notice) = &self.notice {
            status.push(note(notice));
        }
        status.push(row(Box::new(
            ListRow::new(
                format!("settings.network.vpn.state.{uuid}"),
                self.vpn_state(vpn),
            )
            .icon(Icon::Shield)
            .secondary(self.vpn_kind(vpn))
            .trailing(Box::new(self.vpn_switch_control(vpn, &live))),
        )));
        status.extend(self.kill_switch_rows(vpn, &live));
        let mut sections = vec![card(&t("net-status"), rows(status))];
        let settings = match (self.vpn.settings.get(uuid), self.vpn.errors.get(uuid)) {
            (Some(settings), _) => self.vpn_settings_rows(vpn, settings),
            (None, Some(error)) => vec![row(error_caption(&t_args(
                "net-vpn-read-err",
                &[("error", error.as_str().into())],
            )))],
            (None, None) => vec![note(&t("net-pending"))],
        };
        sections.push(card(&t("net-settings"), rows(settings)));
        if let Some(settings) = self.vpn.settings.get(uuid) {
            if settings.kind == "wireguard" {
                sections.push(card(
                    &t("net-vpn-allowed"),
                    rows(self.routed_rows(vpn, settings)),
                ));
            }
            sections.push(card(
                &t("net-dns-servers"),
                rows(self.dns_rows(vpn, settings)),
            ));
        }
        sections.push(card(
            &t("net-actions"),
            rows(vec![
                setting_row_described(
                    &t("net-vpn-export"),
                    &t("net-vpn-export-caption"),
                    Box::new(
                        self.cx.bind_row(
                            Button::new("settings.network.vpn.export", &t("net-vpn-export"))
                                .with_icon(Icon::Download),
                            &live,
                            VpnEffect::ExportVpn {
                                connection: uuid.to_owned(),
                            },
                        ),
                    ),
                ),
                setting_row(
                    &t("net-vpn-delete"),
                    Box::new(
                        self.cx.bind_row(
                            Button::new("settings.network.vpn.delete", &t("net-vpn-delete"))
                                .role(ButtonRole::Destructive),
                            &live,
                            VpnEffect::DeleteConnection {
                                connection: uuid.to_owned(),
                            },
                        ),
                    ),
                ),
            ]),
        ));
        sections
    }

    /// The editable settings of `vpn`, each its own action.
    fn vpn_settings_rows(&self, vpn: &VpnInfo, settings: &VpnProfileSettings) -> Vec<Item> {
        // Every setting here acts on the VPN's own row.
        let live = self.vpn_live(vpn).into_resource();
        let uuid = vpn.uuid.clone();
        let text = |id: &str, value: String, placeholder: String| {
            TextInput::from_state(&LocalState::new(value))
                .with_id(&format!("network.vpn.{id}.{uuid}"))
                .with_placeholder(placeholder)
        };
        let keyed = |uuid: &str| uuid.to_owned();
        let mut out = vec![
            field_row(
                &t("net-vpn-name"),
                Box::new(self.cx.bind_on_row(
                    &live,
                    text("name", settings.name.clone(), t("net-vpn-name")),
                    effect(VpnEffect::SET_VPN_NAME).map({
                        let uuid = keyed(&uuid);
                        move |name: String| (uuid.clone(), name)
                    }),
                )),
            ),
            setting_row(
                &t("net-autoconnect"),
                Box::new(
                    self.cx.bind_on_row(
                        &live,
                        Toggle::from_state(&LocalState::new(settings.autoconnect))
                            .with_id(&format!("network.vpn.autoconnect.{uuid}")),
                        effect(VpnEffect::SET_VPN_AUTOCONNECT).map({
                            let uuid = keyed(&uuid);
                            move |on: bool| (uuid.clone(), on)
                        }),
                    ),
                ),
            ),
            setting_row_described(
                &t("net-vpn-all-traffic"),
                &t("net-vpn-all-traffic-caption"),
                Box::new(
                    self.cx.bind_on_row(
                        &live,
                        Toggle::from_state(&LocalState::new(settings.all_traffic))
                            .with_id(&format!("network.vpn.all-traffic.{uuid}")),
                        effect(VpnEffect::SET_VPN_ALL_TRAFFIC).map({
                            let uuid = keyed(&uuid);
                            move |on: bool| (uuid.clone(), on)
                        }),
                    ),
                ),
            ),
        ];
        out.extend(self.mtu_rows(vpn, settings));
        if !settings.addresses.is_empty() {
            out.push(value_row(
                &t("net-vpn-address"),
                &settings.addresses.join(", "),
            ));
        }
        if settings.kind != "wireguard" {
            if let Some(server) = &settings.endpoint {
                out.push(value_row(&t("net-vpn-server"), server));
            }
            return out;
        }
        out.extend(self.wireguard_settings_rows(vpn, settings));
        out
    }

    /// The packet size: Automatic or Custom, and for Custom the size,
    /// in the same row.
    fn mtu_rows(&self, vpn: &VpnInfo, settings: &VpnProfileSettings) -> Vec<Item> {
        let live = self.vpn_live(vpn).into_resource();
        let uuid = vpn.uuid.clone();
        let label = t("net-vpn-mtu-size");
        let choices = [t("net-vpn-mtu-automatic"), t("net-vpn-mtu-custom")];
        let shown = if settings.mtu.is_some() {
            t("net-vpn-mtu-custom")
        } else {
            t("net-vpn-mtu-automatic")
        };
        let custom = custom_mtu(&vpn.kind);
        let choice = ui::widget::ToggleGroup::from_state(
            choices
                .iter()
                .map(|choice| (choice.clone(), ui::widget::Segment::new(choice.clone())))
                .collect(),
            &LocalState::new(shown),
        )
        .with_accessible_name(label.clone())
        .with_id(format!("network.vpn.mtu-choice.{uuid}"));
        let mut controls = ui::widget::Row::new().with_align_items(taffy::AlignItems::Center);
        if let Some(mtu) = settings.mtu {
            let uuid = uuid.clone();
            controls = controls.push(Box::new(
                self.cx.bind_on_row(
                    &live,
                    NumberInput::new(f64::from(mtu))
                        .with_range(f64::from(*MTU_RANGE.start()), f64::from(*MTU_RANGE.end()))
                        .with_step(1.0)
                        .with_precision(0)
                        .with_id(&format!("network.vpn.mtu.{uuid}"))
                        .with_accessible_name(label.clone()),
                    effect(VpnEffect::SET_VPN_MTU)
                        .map(move |value: f64| (uuid.clone(), Some(whole(value)))),
                ),
            ));
        }
        controls = controls.push(Box::new(
            self.cx.bind_on_row(
                &live,
                choice,
                effect(VpnEffect::SET_VPN_MTU)
                    .map(move |index: usize| (uuid.clone(), (index == 1).then_some(custom))),
            ),
        ));
        vec![setting_row_described(
            &label,
            &t("net-vpn-mtu-caption"),
            Box::new(controls),
        )]
    }

    /// The name servers the tunnel asks, each removable, and the field to
    /// add one; without any, it asks its own.
    fn dns_rows(&self, vpn: &VpnInfo, settings: &VpnProfileSettings) -> Vec<Item> {
        let live = self.vpn_live(vpn).into_resource();
        let uuid = vpn.uuid.as_str();
        let mut out: Vec<Item> = settings
            .dns
            .iter()
            .map(|server| {
                let remove = self.cx.bind_row(
                    Button::new(
                        &format!("settings.network.vpn.dns.remove.{server}"),
                        &t("net-vpn-dns-remove"),
                    ),
                    &live,
                    VpnEffect::RemoveVpnDnsServer {
                        connection: uuid.to_owned(),
                        server: server.clone(),
                    },
                );
                row(Box::new(
                    ListRow::new(format!("settings.network.vpn.dns.{server}"), server)
                        .trailing(Box::new(remove)),
                ))
            })
            .collect();
        if out.is_empty() {
            out.push(note(&t("net-vpn-dns-own")));
        }
        out.push(field_row(
            &t("net-vpn-dns-add"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    TextInput::from_state(&LocalState::new(String::new()))
                        .with_id(&format!("network.vpn.dns.add.{uuid}"))
                        .with_kind(ui::widget::TextInputKind::Code)
                        .with_placeholder(t("net-vpn-dns-add-placeholder")),
                    effect(VpnEffect::ADD_VPN_DNS_SERVER).map({
                        let uuid = uuid.to_owned();
                        move |server: String| (uuid.clone(), server)
                    }),
                ),
            ),
        ));
        out
    }

    /// The addresses a `WireGuard` tunnel routes, each removable, and the
    /// field to add one.
    fn routed_rows(&self, vpn: &VpnInfo, settings: &VpnProfileSettings) -> Vec<Item> {
        let live = self.vpn_live(vpn).into_resource();
        let uuid = vpn.uuid.as_str();
        let mut out: Vec<Item> = settings
            .allowed_ips
            .iter()
            .map(|address| {
                let remove = self.cx.bind_row(
                    Button::new(
                        &format!("settings.network.vpn.allowed.remove.{address}"),
                        &t("net-vpn-dns-remove"),
                    ),
                    &live,
                    VpnEffect::RemoveVpnRoutedAddress {
                        connection: uuid.to_owned(),
                        address: address.clone(),
                    },
                );
                row(Box::new(
                    ListRow::new(format!("settings.network.vpn.allowed.{address}"), address)
                        .trailing(Box::new(remove)),
                ))
            })
            .collect();
        if out.is_empty() {
            out.push(note(&t("net-vpn-allowed-none")));
        }
        out.push(field_row(
            &t("net-vpn-allowed-add"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    TextInput::from_state(&LocalState::new(String::new()))
                        .with_id(&format!("network.vpn.allowed.add.{uuid}"))
                        .with_kind(ui::widget::TextInputKind::Code)
                        .with_placeholder(t("net-vpn-allowed-add-placeholder")),
                    effect(VpnEffect::ADD_VPN_ROUTED_ADDRESS).map({
                        let uuid = uuid.to_owned();
                        move |address: String| (uuid.clone(), address)
                    }),
                ),
            ),
        ));
        out
    }

    /// A `WireGuard` tunnel's own settings: its server and how often it
    /// tells it it is there; what it routes has a card of its own.
    fn wireguard_settings_rows(&self, vpn: &VpnInfo, settings: &VpnProfileSettings) -> Vec<Item> {
        // Every setting here acts on the VPN's own row.
        let live = self.vpn_live(vpn).into_resource();
        let uuid = vpn.uuid.as_str();
        let text = |id: &str, value: String, placeholder: String| {
            TextInput::from_state(&LocalState::new(value))
                .with_id(&format!("network.vpn.{id}.{uuid}"))
                .with_placeholder(placeholder)
        };
        let keyed = |uuid: &str| uuid.to_owned();
        let mut out = Vec::new();
        if let Some(key) = &settings.peer_public_key {
            out.push(key_row(&t("net-vpn-server-key"), key));
        }
        out.extend([
            field_row(
                &t("net-vpn-server"),
                Box::new(self.cx.bind_on_row(
                    &live,
                    text(
                        "endpoint",
                        settings.endpoint.clone().unwrap_or_default(),
                        t("net-vpn-server"),
                    ),
                    effect(VpnEffect::SET_VPN_SERVER).map({
                        let uuid = keyed(uuid);
                        move |endpoint: String| (uuid.clone(), endpoint)
                    }),
                )),
            ),
            setting_row_described(
                &t("net-vpn-keepalive"),
                &t("net-vpn-keepalive-caption"),
                Box::new(
                    self.cx.bind_on_row(
                        &live,
                        NumberInput::new(f64::from(settings.keepalive.unwrap_or(0)))
                            .with_range(0.0, 3600.0)
                            .with_step(1.0)
                            .with_precision(0)
                            .with_id(&format!("network.vpn.keepalive.{uuid}"))
                            .with_accessible_name(t("net-vpn-keepalive")),
                        effect(VpnEffect::SET_VPN_KEEPALIVE).map({
                            let uuid = keyed(uuid);
                            move |value: f64| (uuid.clone(), whole(value))
                        }),
                    ),
                ),
            ),
        ]);
        out
    }
}

#[cfg(test)]
impl NetworkModel {
    /// The public key generated for a new tunnel.
    pub(super) fn vpn_public_key(&self) -> Option<String> {
        self.vpn.wireguard.public_key.clone()
    }

    /// Fill the new tunnel's form as a person would.
    pub(super) fn fill_wireguard_form(
        &self,
        name: &str,
        address: &str,
        peer: &str,
        endpoint: &str,
    ) {
        let form = &self.vpn.wireguard;
        form.name.set(name.into());
        form.address.set(address.into());
        form.server_public_key.set(peer.into());
        form.endpoint.set(endpoint.into());
    }

    /// Whether the new tunnel's private key buffer holds nothing.
    pub(super) fn vpn_private_key_is_empty(&self) -> bool {
        self.vpn
            .wireguard
            .private_key
            .lock()
            .is_ok_and(|key| key.is_empty())
    }
}
