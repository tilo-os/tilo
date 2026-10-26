//! Screenshot scenarios: which pane a capture shows, at which size, and the
//! stand-ins it shows in place of this device's hardware and accounts.
//!
//! Every pane is a scenario of its own, named by its id (`network`,
//! `region_formats`), captured wide. The narrow ones are drawn at a phone's
//! width: `narrow` keeps a pane's content on screen with the back arrow (what
//! a collapse shows), `narrow-list` the category list one back-tap away.
//! `search` shows the sidebar filtered by a query, `account-form` the form
//! for adding an account, `accounts-narrow` the account cards at a phone's
//! width. A plain `--shots` shows Appearance. The `displays-*` captures show
//! Displays with a desk monitor beside the panel: as it opens, with the
//! monitor chosen in the picker or on the canvas, held mid-drag, and while
//! Identify shows the numbers.

use std::path::PathBuf;
use std::sync::LazyLock;

use ui::framework::Fixture;

use crate::accounts::AccountKind;
use crate::categories::Category;
use crate::resource::Settings;

/// The wide viewport every capture is drawn at unless it is narrow.
pub const WIDE_VIEWPORT: (u32, u32) = (1200, 760);

/// The phone-sized viewport of the narrow scenarios.
pub const NARROW_VIEWPORT: (u32, u32) = (440, 820);

/// The query the `search` scenario types into the window bar's search.
pub const SEARCH_QUERY: &str = "light";
/// The query the `search-keys` scenario types: switches of one page and
/// settings another page draws its own way.
pub const SEARCH_KEYS_QUERY: &str = "keys";
/// The query the `search-sound` scenario types with Sound read: a switch
/// of the sound server.
pub const SEARCH_SOUND_QUERY: &str = "mute";
/// The query the `search-display` scenario types with Displays read: a
/// setting of the display, with its value.
pub const SEARCH_DISPLAY_QUERY: &str = "refresh";

const CODEC_SCENARIOS: &[&str] = &[
    "codecsdemo-off",
    "codecsdemo-installing",
    "codecsdemo-installed",
    "codecsdemo-waiting",
    "codecsdemo-failed",
];
const DEVICE_DISPLAYS: &str = "device-displays";
/// Displays with two displays, in each state its design names.
const DISPLAYS_TWO: &str = "displays-two";
const DISPLAYS_KEYBOARD: &str = "displays-keyboard";
const DISPLAYS_CANVAS: &str = "displays-canvas";
const DISPLAYS_DRAGGING: &str = "displays-dragging";
const DISPLAYS_IDENTIFY: &str = "displays-identify";
/// Storage with remembered NTFS drives, drawn tall enough to reach them.
const STORAGE_DRIVES: &str = "storage-drives";
/// Device privacy & security drawn tall enough to reach the terminal tabs
/// controlling the desktop and the workspace grants below them.
const PERMISSIONS_ALL: &str = "permissions-all";

/// Network with Wi-Fi off, looking for networks, and with a known
/// network's details open.
const NETWORK_WIFI_OFF: &str = "network-wifi-off";
const NETWORK_SCANNING: &str = "network-scanning";
const NETWORK_KNOWN: &str = "network-known";
/// Network with a modem: registered at home, roaming, its SIM asking for
/// the PIN, and asking for the PUK after too many wrong PINs.
const NETWORK_MOBILE: &str = "network-mobile";
const NETWORK_MOBILE_ROAMING: &str = "network-mobile-roaming";
const NETWORK_MOBILE_LOCKED: &str = "network-mobile-locked";
const NETWORK_MOBILE_PUK: &str = "network-mobile-puk";
/// Network on its first page: the Wi-Fi list and the controls under it.
const NETWORK_LIST: &str = "network-list";
const NETWORK_ALL: &str = "network-all";
const NETWORK_PAGE: &str = "network-page";
const NETWORK_VPN: &str = "network-vpn";
const NETWORK_VPN_PROFILE: &str = "network-vpn-profile";
/// An `OpenVPN` profile's page: its protocol, its own MTU and name servers.
const NETWORK_VPN_OPENVPN: &str = "network-vpn-openvpn";
const NETWORK_VPN_ADD: &str = "network-vpn-add";
/// The VPN list with the person's devices as exits, in use both ways.
const NETWORK_VPN_DEVICES: &str = "network-vpn-devices";
/// A connected `WireGuard` tunnel's page with its kill switch on.
const NETWORK_VPN_KILL_SWITCH: &str = "network-vpn-kill-switch";
/// That tunnel dropped: its kill switch blocks traffic until it is back.
const NETWORK_VPN_BLOCKING: &str = "network-vpn-blocking";
/// Every network capture.
#[cfg(test)]
const NETWORK_SCENARIOS: [&str; 17] = [
    NETWORK_LIST,
    NETWORK_WIFI_OFF,
    NETWORK_SCANNING,
    NETWORK_KNOWN,
    NETWORK_MOBILE,
    NETWORK_MOBILE_ROAMING,
    NETWORK_MOBILE_LOCKED,
    NETWORK_MOBILE_PUK,
    NETWORK_ALL,
    NETWORK_PAGE,
    NETWORK_VPN,
    NETWORK_VPN_PROFILE,
    NETWORK_VPN_OPENVPN,
    NETWORK_VPN_ADD,
    NETWORK_VPN_DEVICES,
    NETWORK_VPN_KILL_SWITCH,
    NETWORK_VPN_BLOCKING,
];

/// Devices & sync with a device's row open, asking whether to remove it.
const DEVICES_REMOVE: &str = "devices-remove";
/// Speakers' room layout step: the first speaker selected, a speaker
/// chosen in the picker (open), and a speaker held mid drag on a guide.
const SPEAKERS_LAYOUT: &str = "speakers-layout";
const SPEAKERS_LAYOUT_PICKED: &str = "speakers-layout-picked";
const SPEAKERS_LAYOUT_DRAG: &str = "speakers-layout-drag";

const NARROW: &str = "narrow";
const NARROW_LIST: &str = "narrow-list";
const SEARCH: &str = "search";
const SEARCH_KEYS: &str = "search-keys";
const SEARCH_SOUND: &str = "search-sound";
const SEARCH_DISPLAY: &str = "search-display";
const SEARCH_NARROW: &str = "search-narrow";
const ACCOUNT_FORM: &str = "account-form";
const ACCOUNTS_NARROW: &str = "accounts-narrow";
const ACCOUNTS_LINKING: &str = "accounts-linking";
const ACCOUNTS_LINKED: &str = "accounts-linked";

/// Every scenario: one per pane, then the layouts and flows.
pub fn names() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        Category::ALL
            .iter()
            .map(|category| category.slug())
            .chain([
                NARROW,
                NARROW_LIST,
                SEARCH,
                SEARCH_KEYS,
                SEARCH_SOUND,
                SEARCH_DISPLAY,
                SEARCH_NARROW,
                ACCOUNT_FORM,
                ACCOUNTS_NARROW,
                ACCOUNTS_LINKING,
                ACCOUNTS_LINKED,
                DEVICE_DISPLAYS,
                DISPLAYS_TWO,
                DISPLAYS_KEYBOARD,
                DISPLAYS_CANVAS,
                DISPLAYS_DRAGGING,
                DISPLAYS_IDENTIFY,
                STORAGE_DRIVES,
                PERMISSIONS_ALL,
                "keyboard-single",
                "keyboard-many",
                "keyboard-touch",
                "keyboard-osk",
                NETWORK_WIFI_OFF,
                NETWORK_SCANNING,
                NETWORK_KNOWN,
                NETWORK_MOBILE,
                NETWORK_MOBILE_ROAMING,
                NETWORK_MOBILE_LOCKED,
                NETWORK_MOBILE_PUK,
                NETWORK_LIST,
                NETWORK_ALL,
                NETWORK_PAGE,
                NETWORK_VPN,
                NETWORK_VPN_PROFILE,
                NETWORK_VPN_OPENVPN,
                NETWORK_VPN_ADD,
                NETWORK_VPN_DEVICES,
                NETWORK_VPN_KILL_SWITCH,
                NETWORK_VPN_BLOCKING,
                DEVICES_REMOVE,
                SPEAKERS_LAYOUT,
                SPEAKERS_LAYOUT_PICKED,
                SPEAKERS_LAYOUT_DRAG,
            ])
            .chain(CODEC_SCENARIOS.iter().copied())
            .collect()
    });
    &NAMES
}

pub fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some(NARROW | NARROW_LIST | ACCOUNTS_NARROW | SEARCH_NARROW) => NARROW_VIEWPORT,
        Some(DEVICE_DISPLAYS) => (1200, 1100),
        Some(
            DISPLAYS_TWO | DISPLAYS_KEYBOARD | DISPLAYS_CANVAS | DISPLAYS_DRAGGING
            | DISPLAYS_IDENTIFY,
        ) => (1200, 1300),
        Some(STORAGE_DRIVES) => (1200, 1450),
        Some(PERMISSIONS_ALL) => (1200, 2000),
        // The Mobile section follows the rest of Network.
        Some(
            NETWORK_MOBILE | NETWORK_MOBILE_ROAMING | NETWORK_MOBILE_LOCKED | NETWORK_MOBILE_PUK,
        ) => (1200, 2600),
        Some(SPEAKERS_LAYOUT | SPEAKERS_LAYOUT_PICKED | SPEAKERS_LAYOUT_DRAG) => (1200, 980),
        _ => WIDE_VIEWPORT,
    }
}

/// What a capture shows in place of the person's own: accounts from the
/// bundled sample, kept in the capture's scratch directory, and the form a
/// scenario opens.
#[derive(Clone, Debug)]
pub struct CaptureScene {
    /// Fixed formats status for the explicit codecs capture scenarios.
    pub codec_status: Option<crate::codec_pack::CodecPackStatus>,
    /// Where the capture keeps its account stores.
    pub account_root: Option<PathBuf>,
    /// The account form the scenario shows open.
    pub account_form: Option<AccountKind>,
    /// The displays captures' two displays and what is done with them.
    pub displays: Option<DisplaysSample>,
    /// Synthetic certified-peer observations; available only to capture fixtures.
    pub device_displays: Vec<settings_store::device_displays::DeviceDisplay>,
    pub person_devices: Vec<settings_store::person_displays::PersonDevice>,
    /// Synthetic attached keyboards; captures never discover the real host.
    pub input_hardware: Option<settings_store::hardware::InputHardwareSnapshot>,
    /// Stand-in storage summaries of the person's devices, for the
    /// storage capture.
    pub storage_devices: Vec<DeviceResidency>,
    /// A stand-in for this device's disk, for the storage capture.
    pub storage_capacity: Option<os_actions::DiskCapacity>,
    pub remembered_drives: Vec<ntfs_mount::protocol::RememberedDrive>,
    /// A room bridged to Discord, and where the person's account there
    /// stands, for the linked-accounts captures.
    pub linked_account: Option<ui::linked_accounts::LinkState>,
    /// The person's trusted devices, as a stand-in for the host's device
    /// list, for the devices & sync capture.
    pub fleet_devices: Vec<ui::tilo_actions::remote::identity_admin::FleetDevice>,
    /// The person's home relay and the workspaces this device hosts, as a
    /// stand-in for the host's answer, for the devices & sync capture.
    pub home_relay: Option<ui::tilo_actions::remote::home_relay_hosting::HomeRelayHosting>,
    /// Whether the capture asks about removing the first other device.
    pub device_removal: bool,
    pub network: Option<NetworkSample>,
    /// The modem the mobile captures show; `None` for none.
    pub mobile: Option<MobileSample>,
    /// Where the speaker setup stands, for the room layout captures.
    pub speakers: Option<SpeakersSample>,
}

/// Where the speaker captures' setup stands: on the room layout step of a
/// new set of four speakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakersSample {
    /// The first speaker selected.
    Layout,
    /// The third speaker chosen in the picker, its list open.
    Picked,
    /// The second speaker held mid drag, on the mirror of the first.
    Dragging,
}

/// What the displays captures show done with their two displays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplaysSample {
    /// The pane as it opens: the main display selected.
    Default,
    ChosenByKeyboard,
    PickedOnCanvas,
    /// The monitor held mid-drag, lined up with the panel.
    Dragging,
    /// Identify showing the numbers.
    Identifying,
}

/// How the network captures' sample stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkSample {
    WifiOff,
    /// Wi-Fi is on and looking, nothing found yet.
    Scanning,
    /// The known networks.
    Known,
    /// The first page: the Wi-Fi list and the controls under it.
    List,
    /// Every network in range.
    AllNetworks,
    /// The page of the network in use.
    NetworkPage,
    /// The VPN list.
    VpnList,
    /// The VPN list routing through one of the person's devices, and this
    /// device a VPN for another of theirs.
    VpnDevices,
    /// A `WireGuard` tunnel's page.
    VpnProfile,
    /// That page with the tunnel's kill switch on, connected.
    VpnKillSwitch,
    /// That page with the tunnel dropped, its kill switch blocking traffic.
    VpnBlocking,
    /// An `OpenVPN` profile's page.
    PluginVpnProfile,
    /// The kinds of VPN that can be added.
    AddVpn,
}

/// Where the mobile captures' modem stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobileSample {
    /// Registered with its home network on LTE, data on.
    Registered,
    /// On another operator's network abroad.
    Roaming,
    /// The SIM asks for its PIN.
    Locked,
    /// The SIM asks for its PUK after too many wrong PINs.
    Puk,
}

/// A capture's room bridged to Discord, its person's account standing as
/// the scenario says.
pub struct CaptureBridge(pub ui::linked_accounts::LinkState);

impl ui::linked_accounts::LinkedAccountsHost for CaptureBridge {
    fn accounts(&self) -> Vec<ui::linked_accounts::LinkedAccount> {
        vec![ui::linked_accounts::LinkedAccount {
            app: "so.tilo.discord".into(),
            service: "Discord".into(),
            state: self.0.clone(),
        }]
    }
    fn revision(&self) -> u64 {
        0
    }
    // A capture asks no host: what it shows stands.
    fn refresh(&self) {}
    fn link(&self, _app: &str) {}
    fn unlink(&self, _app: &str) {}
}

fn capture_link_code() -> ui::linked_accounts::LinkCode {
    ui::linked_accounts::LinkCode {
        code: "K7QMW2XRB4TD".into(),
        instruction: "In Discord, type /link with this code. It works for 10 minutes.".into(),
    }
}

/// Prepare `fixture`'s capture: every capture shows the sample accounts.
pub fn prepare(fixture: &mut Fixture<Settings>) {
    let account_form = (fixture.scenario() == Some(ACCOUNT_FORM)).then_some(AccountKind::Nextcloud);
    let account_root = fixture
        .directory()
        .ok()
        .map(|directory| directory.join("tilo"));
    fixture.provide(CaptureScene {
        codec_status: codec_status(fixture.scenario()),
        account_root,
        account_form,
        input_hardware: keyboard_fixture(fixture.scenario()),
        displays: match fixture.scenario() {
            Some(DISPLAYS_TWO) => Some(DisplaysSample::Default),
            Some(DISPLAYS_KEYBOARD) => Some(DisplaysSample::ChosenByKeyboard),
            Some(DISPLAYS_CANVAS) => Some(DisplaysSample::PickedOnCanvas),
            Some(DISPLAYS_DRAGGING) => Some(DisplaysSample::Dragging),
            Some(DISPLAYS_IDENTIFY) => Some(DisplaysSample::Identifying),
            _ => None,
        },
        person_devices: if fixture.scenario() == Some(DEVICE_DISPLAYS) {
            person_display_fixture()
        } else {
            Vec::new()
        },
        device_displays: if fixture.scenario() == Some(DEVICE_DISPLAYS) {
            device_display_fixture()
        } else {
            Vec::new()
        },
        storage_devices: if fixture.scenario() == Some(Category::Storage.slug()) {
            storage_fixture()
        } else {
            Vec::new()
        },
        storage_capacity: (fixture.scenario() == Some(Category::Storage.slug())).then_some(
            os_actions::DiskCapacity {
                total_bytes: 512_000_000_000,
                available_bytes: 187_400_000_000,
            },
        ),
        remembered_drives: if fixture.scenario() == Some(STORAGE_DRIVES) {
            drives_fixture()
        } else {
            Vec::new()
        },
        linked_account: match fixture.scenario() {
            Some(ACCOUNTS_LINKING) => {
                Some(ui::linked_accounts::LinkState::Linking(capture_link_code()))
            }
            Some(ACCOUNTS_LINKED) => Some(ui::linked_accounts::LinkState::Linked(
                ui::linked_accounts::AccountName {
                    user_name: "quinn.davis".into(),
                    display_name: Some("Quinn".into()),
                },
            )),
            _ => None,
        },
        device_removal: fixture.scenario() == Some(DEVICES_REMOVE),
        home_relay: (fixture.scenario() == Some(Category::DeviceMesh.slug()))
            .then(home_relay_fixture),
        fleet_devices: if matches!(
            fixture.scenario(),
            Some(scenario) if scenario == Category::DeviceMesh.slug() || scenario == DEVICES_REMOVE
        ) {
            fleet_fixture()
        } else {
            Vec::new()
        },
        network: match fixture.scenario() {
            Some(NETWORK_WIFI_OFF) => Some(NetworkSample::WifiOff),
            Some(NETWORK_SCANNING) => Some(NetworkSample::Scanning),
            Some(NETWORK_KNOWN) => Some(NetworkSample::Known),
            Some(NETWORK_LIST) => Some(NetworkSample::List),
            Some(NETWORK_ALL) => Some(NetworkSample::AllNetworks),
            Some(NETWORK_PAGE) => Some(NetworkSample::NetworkPage),
            Some(NETWORK_VPN) => Some(NetworkSample::VpnList),
            Some(NETWORK_VPN_PROFILE) => Some(NetworkSample::VpnProfile),
            Some(NETWORK_VPN_OPENVPN) => Some(NetworkSample::PluginVpnProfile),
            Some(NETWORK_VPN_ADD) => Some(NetworkSample::AddVpn),
            Some(NETWORK_VPN_DEVICES) => Some(NetworkSample::VpnDevices),
            Some(NETWORK_VPN_KILL_SWITCH) => Some(NetworkSample::VpnKillSwitch),
            Some(NETWORK_VPN_BLOCKING) => Some(NetworkSample::VpnBlocking),
            _ => None,
        },
        mobile: match fixture.scenario() {
            Some(NETWORK_MOBILE) => Some(MobileSample::Registered),
            Some(NETWORK_MOBILE_ROAMING) => Some(MobileSample::Roaming),
            Some(NETWORK_MOBILE_LOCKED) => Some(MobileSample::Locked),
            Some(NETWORK_MOBILE_PUK) => Some(MobileSample::Puk),
            _ => None,
        },
        speakers: match fixture.scenario() {
            Some(SPEAKERS_LAYOUT) => Some(SpeakersSample::Layout),
            Some(SPEAKERS_LAYOUT_PICKED) => Some(SpeakersSample::Picked),
            Some(SPEAKERS_LAYOUT_DRAG) => Some(SpeakersSample::Dragging),
            _ => None,
        },
    });
}

/// The person's trusted devices: the laptop in use, a phone just paired
/// from its browser, and a home server.
fn fleet_fixture() -> Vec<ui::tilo_actions::remote::identity_admin::FleetDevice> {
    use ui::tilo_actions::remote::identity_admin::FleetDevice;
    let device = |label: &str, key: &str, fingerprint: &str, current, can_host| FleetDevice {
        label: label.into(),
        key_hex: key.repeat(8),
        fingerprint: fingerprint.into(),
        current,
        revoked: false,
        can_host,
    };
    vec![
        device(
            "Ada's MacBook",
            "4f2a9c1e",
            "4F 2A 9C 1E 7B D3",
            true,
            false,
        ),
        device(
            "Safari on iPhone",
            "b81d03e6",
            "B8 1D 03 E6 5A 92",
            false,
            false,
        ),
        device("Home server", "6c47f2a0", "6C 47 F2 A0 E1 38", false, true),
    ]
}

/// The person's home server and three workspaces this device hosts, one of
/// them kept on the server too.
fn home_relay_fixture() -> ui::tilo_actions::remote::home_relay_hosting::HomeRelayHosting {
    use ui::tilo_actions::remote::home_relay_hosting::{HomeRelayHosting, HostedWorkspace};
    let workspace = |room: &str, name: &str, on_relay| HostedWorkspace {
        room: room.into(),
        name: name.into(),
        on_relay,
    };
    HomeRelayHosting {
        relay: Some("Home server".into()),
        workspaces: vec![
            workspace("4d6f1a2c-0000-4000-8000-000000000001", "Personal", true),
            workspace("4d6f1a2c-0000-4000-8000-000000000002", "Studio", false),
            workspace("4d6f1a2c-0000-4000-8000-000000000003", "Taxes", false),
        ],
    }
}

/// Three devices of one person: a laptop holding work nothing else has
/// yet, a desktop that keeps one workspace and caches another, and a home
/// server that holds copies of both.
/// Two remembered drives, for the drives capture.
pub fn drives_fixture() -> Vec<ntfs_mount::protocol::RememberedDrive> {
    use ntfs_mount::protocol::{Mode, Placement, RememberedDrive};
    vec![
        RememberedDrive {
            grant: "fixture-windows".into(),
            label: "Windows".into(),
            placement: Placement::Internal,
            mode: Mode::Open,
            mounted: Some("/run/media/person/Windows".into()),
        },
        RememberedDrive {
            grant: "fixture-stick".into(),
            label: String::new(),
            placement: Placement::External,
            mode: Mode::OpenSafely,
            mounted: None,
        },
    ]
}

/// One device's storage summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceResidency {
    /// The device's key, in hex; what its summaries are kept by.
    pub key: String,
    /// The name the person gave the device.
    pub label: String,
    /// Whether it is the device Settings runs on.
    pub current: bool,
    /// What the device keeps of each workspace.
    pub summary: runtime_fs::storage::ResidencySummary,
}

pub fn storage_fixture() -> Vec<DeviceResidency> {
    use runtime_fs::storage::{ResidencySplit, ResidencySummary, WorkspaceResidency};
    const MB: u64 = 1_000_000;
    let workspace = |name: &str, kept_here, split| WorkspaceResidency {
        workspace: name.into(),
        kept_here,
        split,
    };
    let device = |key: &str, label: &str, current, workspaces| DeviceResidency {
        key: key.into(),
        label: label.into(),
        current,
        summary: ResidencySummary { workspaces },
    };
    vec![
        device(
            "fixture-laptop",
            "Laptop",
            true,
            vec![
                workspace(
                    "Thesis",
                    false,
                    ResidencySplit {
                        replicated_bytes: 820 * MB,
                        only_here_bytes: 48 * MB,
                        only_here_files: 3,
                        cache_bytes: 1_260 * MB,
                        ..ResidencySplit::default()
                    },
                ),
                workspace(
                    "Family photos",
                    true,
                    ResidencySplit {
                        pinned_bytes: 6_400 * MB,
                        ..ResidencySplit::default()
                    },
                ),
            ],
        ),
        device(
            "fixture-desktop",
            "Desktop",
            false,
            vec![workspace(
                "Thesis",
                true,
                ResidencySplit {
                    pinned_bytes: 2_080 * MB,
                    ..ResidencySplit::default()
                },
            )],
        ),
        device(
            "fixture-server",
            "Home server",
            false,
            vec![
                workspace(
                    "Thesis",
                    false,
                    ResidencySplit {
                        replicated_bytes: 2_080 * MB,
                        ..ResidencySplit::default()
                    },
                ),
                workspace(
                    "Family photos",
                    false,
                    ResidencySplit {
                        replicated_bytes: 6_400 * MB,
                        ..ResidencySplit::default()
                    },
                ),
            ],
        ),
    ]
}

fn codec_status(scenario: Option<&str>) -> Option<crate::codec_pack::CodecPackStatus> {
    let json = match scenario? {
        "codecsdemo-off" => r#"{"state":"not-installed"}"#,
        "codecsdemo-installing" => {
            r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":5,"label":"Downloading","bytes":14000000,"total":38000000}}"#
        }
        "codecsdemo-installed" => {
            r#"{"state":"installed","desired":true,"parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"}}"#
        }
        "codecsdemo-waiting" => {
            r#"{"state":"installed","desired":true,"parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"}}"#
        }
        "codecsdemo-failed" => {
            r#"{"state":"failed","desired":true,"last_error":"Could not download the manifest (connection reset)."}"#
        }
        _ => return None,
    };
    Some(crate::codec_pack::CodecPackStatus::parse(json))
}

/// Rich synthetic observations for the explicit display capture only.
fn person_display_fixture() -> Vec<settings_store::person_displays::PersonDevice> {
    use settings_store::person_displays::{DeviceOutput, PersonDevice};
    device_display_fixture()
        .into_iter()
        .map(|screen| PersonDevice {
            device: screen.device,
            label: screen.device_label,
            local: screen.local,
            online: screen.online,
            outputs: vec![DeviceOutput {
                id: screen.connector,
                name: screen.label,
                builtin: screen.local,
                x: 0,
                y: 0,
                width: screen.width,
                height: screen.height,
                mode_width: screen.width * u32::from(screen.scale_milli) / 1000,
                mode_height: screen.height * u32::from(screen.scale_milli) / 1000,
                refresh_mhz: 60_000,
                scale_milli: screen.scale_milli,
                connected: true,
                enabled: true,
            }],
        })
        .collect()
}

/// Two fixture devices with device-qualified connector identities and logical sizes.
fn device_display_fixture() -> Vec<settings_store::device_displays::DeviceDisplay> {
    use settings_store::device_displays::DeviceDisplay;
    vec![
        DeviceDisplay {
            device: "fixture-laptop".into(),
            device_label: "Laptop".into(),
            connector: "eDP-1".into(),
            output_id: Some("eDP-1".into()),
            label: "Built-in display".into(),
            online: true,
            local: true,
            x: 0,
            y: 0,
            width: 960,
            height: 540,
            scale_milli: 2000,
            revision: 1,
        },
        DeviceDisplay {
            device: "fixture-desktop".into(),
            device_label: "Desktop".into(),
            connector: "DP-1".into(),
            output_id: Some("DP-1".into()),
            label: "External display".into(),
            online: true,
            local: false,
            x: 960,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 1,
        },
    ]
}

/// How a scenario lays the app out once its pane is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staged {
    /// `category`'s content.
    Pane(Category),
    /// The narrow category list, one back-tap from `category`.
    List(Category),
    /// `category` searched for the query: the sidebar narrowed to the pages
    /// it finds, and the settings it finds shown as their rows.
    Search(Category, &'static str),
    /// `category`'s content with the keyboard on the control with this id,
    /// as Tab leaves it.
    Focused(Category, &'static str),
}

/// What `scenario` shows.
pub fn staged(scenario: Option<&str>) -> Staged {
    match scenario {
        None => Staged::Pane(Category::Appearance),
        Some(
            DEVICE_DISPLAYS | DISPLAYS_TWO | DISPLAYS_CANVAS | DISPLAYS_DRAGGING
            | DISPLAYS_IDENTIFY,
        ) => Staged::Pane(Category::Displays),
        Some(DISPLAYS_KEYBOARD) => Staged::Focused(Category::Displays, "display.arrangement"),
        Some(STORAGE_DRIVES) => Staged::Pane(Category::Storage),
        Some(PERMISSIONS_ALL) => Staged::Pane(Category::Permissions),
        Some("keyboard-single" | "keyboard-many" | "keyboard-touch" | "keyboard-osk") => {
            Staged::Pane(Category::Keyboard)
        }
        Some(
            NETWORK_WIFI_OFF
            | NETWORK_SCANNING
            | NETWORK_KNOWN
            | NETWORK_LIST
            | NETWORK_ALL
            | NETWORK_PAGE
            | NETWORK_VPN
            | NETWORK_VPN_PROFILE
            | NETWORK_VPN_OPENVPN
            | NETWORK_VPN_ADD
            | NETWORK_VPN_DEVICES
            | NETWORK_VPN_KILL_SWITCH
            | NETWORK_VPN_BLOCKING
            | NETWORK_MOBILE
            | NETWORK_MOBILE_ROAMING
            | NETWORK_MOBILE_LOCKED
            | NETWORK_MOBILE_PUK,
        ) => Staged::Pane(Category::Network),
        Some(DEVICES_REMOVE) => Staged::Pane(Category::DeviceMesh),
        Some(SPEAKERS_LAYOUT | SPEAKERS_LAYOUT_PICKED | SPEAKERS_LAYOUT_DRAG) => {
            Staged::Pane(Category::Speakers)
        }
        Some(NARROW) => Staged::Pane(Category::RegionFormats),
        Some(NARROW_LIST) => Staged::List(Category::RegionFormats),
        Some(SEARCH | SEARCH_NARROW) => Staged::Search(Category::Appearance, SEARCH_QUERY),
        Some(SEARCH_KEYS) => Staged::Search(Category::Accessibility, SEARCH_KEYS_QUERY),
        Some(SEARCH_SOUND) => Staged::Search(Category::Sound, SEARCH_SOUND_QUERY),
        Some(SEARCH_DISPLAY) => Staged::Search(Category::Displays, SEARCH_DISPLAY_QUERY),
        Some(ACCOUNT_FORM | ACCOUNTS_NARROW | ACCOUNTS_LINKING | ACCOUNTS_LINKED) => {
            Staged::Pane(Category::ExternalAccounts)
        }
        Some(slug) if CODEC_SCENARIOS.contains(&slug) => Staged::Pane(Category::Updates),
        Some(slug) => Staged::Pane(Category::from_slug(slug).unwrap_or(Category::Appearance)),
    }
}

/// Synthetic input observations use the same snapshot contract as the compositor.
pub fn keyboard_fixture(
    scenario: Option<&str>,
) -> Option<settings_store::hardware::InputHardwareSnapshot> {
    use settings_store::hardware::{InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot};
    let kinds: &[InputDeviceKind] = match scenario? {
        "keyboard-single" => &[InputDeviceKind::Keyboard],
        "keyboard-many" => &[
            InputDeviceKind::Keyboard,
            InputDeviceKind::Keyboard,
            InputDeviceKind::Touchscreen,
        ],
        "keyboard-touch" => &[InputDeviceKind::Keyboard, InputDeviceKind::Touchscreen],
        "keyboard-osk" => &[InputDeviceKind::Touchscreen],
        _ => return None,
    };
    Some(InputHardwareSnapshot {
        devices: kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| InputDeviceSnapshot {
                key: format!("fixture-input-{index}"),
                name: if *kind == InputDeviceKind::Keyboard {
                    format!("Keyboard {}", index + 1)
                } else {
                    "Touchscreen".into()
                },
                kind: *kind,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_captures_publish_the_rendered_card_and_controls() {
        use ui::framework::testing::TestApp;
        let cases = [
            ("codecsdemo-off", "Off", "Install"),
            ("codecsdemo-installing", "Downloading 14 of 38 MB", ""),
            (
                "codecsdemo-installed",
                "On, including hardware H.264/HEVC",
                "Remove",
            ),
            (
                "codecsdemo-waiting",
                "follows once a matching driver is published",
                "Check for hardware support again",
            ),
            ("codecsdemo-failed", "Could not install:", "Try again"),
        ];
        for theme in [
            ui::Theme::dark(),
            ui::Theme::light(),
            ui::Theme::high_contrast(),
        ] {
            for (scenario, status, action) in cases {
                let mut app = TestApp::<crate::resource::SettingsShell>::capture(Some(scenario))
                    .expect("formats capture opens");
                app.harness()
                    .relayout_themed(WIDE_VIEWPORT, theme.clone())
                    .expect("formats capture lays out");
                app.settle();
                let nodes = app.harness().a11y_nodes();
                let labels: Vec<_> = nodes.iter().filter_map(|(_, node)| node.label()).collect();
                assert!(
                    labels.contains(&"Play more video and audio formats"),
                    "{scenario}: {labels:?}"
                );
                assert!(labels.contains(&"Downloaded to this device from the formats' publishers. You can turn it off here at any time."), "{scenario}: {labels:?}");
                assert!(
                    labels.contains(&"Status"),
                    "{scenario}: the status field is rendered"
                );
                let values: Vec<_> = nodes.iter().filter_map(|(_, node)| node.value()).collect();
                assert!(
                    labels
                        .iter()
                        .chain(values.iter())
                        .any(|said| said.contains(status)),
                    "{scenario}: {labels:?} {values:?}"
                );
                // Captures intentionally have no effect runner. Assert
                // activation and admission on the live framework host with
                // the same status source and a hermetic operation runner.
                let mut live =
                    TestApp::<crate::resource::SettingsShell>::new().expect("live settings opens");
                let status = codec_status(Some(scenario)).expect("formats fixture status");
                live.with_surface::<crate::SettingsApp, ()>(|settings| {
                    let model =
                        crate::panes::updates::UpdatesModel::sample(settings.host().clone())
                            .with_codec_pack(
                                std::sync::Arc::new(move || status.clone()),
                                std::sync::Arc::new(|_| Ok(())),
                            );
                    *crate::panes::updates::area(settings)
                        .unwrap()
                        .model()
                        .borrow_mut() = model;
                    settings.stage_scene(Staged::Pane(Category::Updates));
                })
                .expect("mounted Settings surface");
                live.harness()
                    .relayout_themed(WIDE_VIEWPORT, theme.clone())
                    .expect("live formats card lays out");
                live.settle();
                let live_nodes = live.harness().a11y_nodes();
                if !action.is_empty() {
                    assert!(
                        live_nodes
                            .iter()
                            .any(|(_, node)| node.label() == Some(action)
                                && node.supports_action(ui::accesskit::Action::Click)),
                        "{scenario}: {action} is activatable"
                    );
                }
                assert!(
                    live.harness().component_contract_violations().is_empty(),
                    "{scenario}: the formats actions retain their published contract"
                );
            }
        }
    }

    /// Each scenario names one layout: its pane, its screen, its size.
    #[test]
    fn scenarios_name_their_pane_and_size() {
        assert_eq!(
            names().len(),
            Category::ALL.len() + 44 + CODEC_SCENARIOS.len()
        );
        assert_eq!(
            staged(Some(DISPLAYS_KEYBOARD)),
            Staged::Focused(Category::Displays, "display.arrangement")
        );
        for scenario in [
            DISPLAYS_TWO,
            DISPLAYS_CANVAS,
            DISPLAYS_DRAGGING,
            DISPLAYS_IDENTIFY,
        ] {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Displays));
        }
        for scenario in NETWORK_SCENARIOS {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Network));
        }
        for scenario in [
            SPEAKERS_LAYOUT,
            SPEAKERS_LAYOUT_PICKED,
            SPEAKERS_LAYOUT_DRAG,
        ] {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Speakers));
        }
        assert_eq!(
            staged(Some(STORAGE_DRIVES)),
            Staged::Pane(Category::Storage)
        );
        assert_eq!(
            staged(Some(PERMISSIONS_ALL)),
            Staged::Pane(Category::Permissions)
        );
        assert_eq!(
            staged(Some(DEVICES_REMOVE)),
            Staged::Pane(Category::DeviceMesh)
        );
        for scenario in CODEC_SCENARIOS {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Updates));
            assert!(codec_status(Some(scenario)).is_some());
        }
        assert!(codec_status(None).is_none());
        assert!(codec_status(Some("updates")).is_none());
        for category in Category::ALL {
            assert_eq!(staged(Some(category.slug())), Staged::Pane(*category));
            assert_eq!(viewport(Some(category.slug())), WIDE_VIEWPORT);
        }
        assert_eq!(staged(None), Staged::Pane(Category::Appearance));
        assert_eq!(
            staged(Some("narrow")),
            Staged::Pane(Category::RegionFormats)
        );
        assert_eq!(viewport(Some("narrow")), NARROW_VIEWPORT);
        assert_eq!(
            staged(Some("narrow-list")),
            Staged::List(Category::RegionFormats)
        );
        assert_eq!(
            staged(Some("search")),
            Staged::Search(Category::Appearance, SEARCH_QUERY)
        );
        assert_eq!(viewport(Some("accounts-narrow")), NARROW_VIEWPORT);
    }
}
