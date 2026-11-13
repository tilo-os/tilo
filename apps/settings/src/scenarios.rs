//! Screenshot scenarios: which pane a capture shows, at which size, and the
//! stand-ins it shows in place of this device's hardware and accounts. Every
//! pane is a scenario of its own, named by its id (`network`,
//! `region_formats`), captured wide.

use std::path::PathBuf;
use std::sync::LazyLock;

use ui::framework::Fixture;

use crate::accounts::AccountKind;
use crate::categories::Category;
use crate::resource::Settings;

pub const WIDE_VIEWPORT: (u32, u32) = (1200, 760);

pub const NARROW_VIEWPORT: (u32, u32) = (440, 820);

pub const SEARCH_QUERY: &str = "light";
pub const SEARCH_KEYS_QUERY: &str = "keys";
pub const SEARCH_SOUND_QUERY: &str = "mute";
pub const SEARCH_DISPLAY_QUERY: &str = "refresh";

const CODEC_SCENARIOS: &[&str] = &[
    "codecsdemo-off",
    "codecsdemo-installing",
    "codecsdemo-installed",
    "codecsdemo-waiting",
    "codecsdemo-failed",
];
const WINDOWS_SCENARIOS: &[&str] = &[
    "windowsdemo-off",
    "windowsdemo-installing",
    "windowsdemo-installed",
    "windowsdemo-waiting",
    "windowsdemo-failed",
];
const DEVICE_DISPLAYS: &str = "device-displays";
const DISPLAYS_TWO: &str = "displays-two";
const DISPLAYS_KEYBOARD: &str = "displays-keyboard";
const DISPLAYS_CANVAS: &str = "displays-canvas";
const DISPLAYS_DRAGGING: &str = "displays-dragging";
const DISPLAYS_IDENTIFY: &str = "displays-identify";
const STORAGE_DRIVES: &str = "storage-drives";
const PERMISSIONS_ALL: &str = "permissions-all";

const NETWORK_WIFI_OFF: &str = "network-wifi-off";
const NETWORK_SCANNING: &str = "network-scanning";
const NETWORK_KNOWN: &str = "network-known";
const NETWORK_MOBILE: &str = "network-mobile";
const NETWORK_MOBILE_ROAMING: &str = "network-mobile-roaming";
const NETWORK_MOBILE_LOCKED: &str = "network-mobile-locked";
const NETWORK_MOBILE_PUK: &str = "network-mobile-puk";
const NETWORK_LIST: &str = "network-list";
const NETWORK_WIRED: &str = "network-wired";
const NETWORK_USAGE: &str = "network-usage";
const NETWORK_ALL: &str = "network-all";
const NETWORK_PAGE: &str = "network-page";
const NETWORK_VPN: &str = "network-vpn";
const NETWORK_VPN_PROFILE: &str = "network-vpn-profile";
const NETWORK_VPN_OPENVPN: &str = "network-vpn-openvpn";
const NETWORK_VPN_ADD: &str = "network-vpn-add";
const NETWORK_VPN_DEVICES: &str = "network-vpn-devices";
const NETWORK_VPN_KILL_SWITCH: &str = "network-vpn-kill-switch";
const NETWORK_VPN_BLOCKING: &str = "network-vpn-blocking";
#[cfg(test)]
#[cfg(feature = "tilo-os")]
const NETWORK_SCENARIOS: [&str; 19] = [
    NETWORK_LIST,
    NETWORK_WIRED,
    NETWORK_USAGE,
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

const BILLING_NEW: &str = "billing-new";
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
                NETWORK_WIRED,
                NETWORK_USAGE,
                NETWORK_ALL,
                NETWORK_PAGE,
                NETWORK_VPN,
                NETWORK_VPN_PROFILE,
                NETWORK_VPN_OPENVPN,
                NETWORK_VPN_ADD,
                NETWORK_VPN_DEVICES,
                NETWORK_VPN_KILL_SWITCH,
                NETWORK_VPN_BLOCKING,
                BILLING_NEW,
                SPEAKERS_LAYOUT,
                SPEAKERS_LAYOUT_PICKED,
                SPEAKERS_LAYOUT_DRAG,
            ])
            .chain(CODEC_SCENARIOS.iter().copied())
            .chain(WINDOWS_SCENARIOS.iter().copied())
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
        Some(
            NETWORK_MOBILE | NETWORK_MOBILE_ROAMING | NETWORK_MOBILE_LOCKED | NETWORK_MOBILE_PUK,
        ) => (1200, 2600),
        Some(SPEAKERS_LAYOUT | SPEAKERS_LAYOUT_PICKED | SPEAKERS_LAYOUT_DRAG) => (1200, 980),
        _ => WIDE_VIEWPORT,
    }
}

#[derive(Clone, Debug)]
pub struct CaptureScene {
    #[cfg(feature = "tilo-os")]
    pub codec_status: Option<crate::codec_pack::CodecPackStatus>,
    #[cfg(feature = "tilo-os")]
    pub windows_support: Option<os_actions::windows_compat::Readiness>,
    pub account_root: Option<PathBuf>,
    pub account_form: Option<AccountKind>,
    pub displays: Option<DisplaysSample>,
    pub device_displays: Vec<settings_store::device_displays::DeviceDisplay>,
    pub person_devices: Vec<settings_store::person_displays::PersonDevice>,
    pub input_hardware: Option<settings_store::hardware::InputHardwareSnapshot>,
    #[cfg(feature = "tilo-os")]
    pub storage_devices: Vec<DeviceResidency>,
    #[cfg(feature = "tilo-os")]
    pub storage_capacity: Option<os_actions::DiskCapacity>,
    #[cfg(feature = "tilo-os")]
    pub remembered_drives: Vec<ntfs_mount::protocol::RememberedDrive>,
    pub access: Option<crate::panes::devices_access::AccessSample>,
    pub linked_account: Option<ui::linked_accounts::LinkState>,
    pub fleet_devices: Vec<ui::tilo_actions::remote::identity_admin::FleetDevice>,
    pub home_relay: Option<ui::tilo_actions::remote::home_relay_hosting::HomeRelayHosting>,
    pub billing: Option<ui::tilo_actions::remote::host_account::HostAccount>,
    pub network: Option<NetworkSample>,
    pub mobile: Option<MobileSample>,
    pub speakers: Option<SpeakersSample>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakersSample {
    Layout,
    Picked,
    Dragging,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplaysSample {
    Default,
    ChosenByKeyboard,
    PickedOnCanvas,
    Dragging,
    Identifying,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkSample {
    WifiOff,
    Scanning,
    Known,
    Wired,
    Usage,
    List,
    AllNetworks,
    NetworkPage,
    VpnList,
    VpnDevices,
    VpnProfile,
    VpnKillSwitch,
    VpnBlocking,
    PluginVpnProfile,
    AddVpn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobileSample {
    Registered,
    Roaming,
    Locked,
    Puk,
}

pub struct CaptureBridge(pub ui::linked_accounts::LinkState);

impl ui::linked_accounts::LinkedAccountsHost for CaptureBridge {
    fn accounts(&self) -> Vec<ui::linked_accounts::LinkedAccount> {
        vec![ui::linked_accounts::LinkedAccount {
            app: "so.tilo.discord".into(),
            service: "Discord".into(),
            state: self.0.clone(),
            problem: None,
        }]
    }
    fn revision(&self) -> u64 {
        0
    }
    fn refresh(&self) {}
    fn link(&self, _app: &str) {}
    fn unlink(&self, _app: &str) {}
}

fn capture_link_state(scenario: Option<&str>) -> Option<ui::linked_accounts::LinkState> {
    let quinn = ui::linked_accounts::AccountName {
        user_name: "quinn.davis".into(),
        display_name: Some("Quinn".into()),
    };
    match scenario {
        Some(ACCOUNTS_LINKING) => Some(ui::linked_accounts::LinkState::Offered(
            ui::linked_accounts::OfferReading {
                service: "Discord".into(),
                account: quinn,
                started_here: false,
            },
        )),
        Some(ACCOUNTS_LINKED) => Some(ui::linked_accounts::LinkState::Linked(quinn)),
        _ => None,
    }
}

pub fn prepare(fixture: &mut Fixture<Settings>) {
    let account_form = (fixture.scenario() == Some(ACCOUNT_FORM)).then_some(AccountKind::Nextcloud);
    let account_root = fixture
        .directory()
        .ok()
        .map(|directory| directory.join("tilo"));
    fixture.provide(CaptureScene {
        #[cfg(feature = "tilo-os")]
        codec_status: codec_status(fixture.scenario()),
        #[cfg(feature = "tilo-os")]
        windows_support: windows_support(fixture.scenario()),
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
        #[cfg(feature = "tilo-os")]
        storage_devices: if fixture.scenario() == Some(Category::Storage.slug()) {
            storage_fixture()
        } else {
            Vec::new()
        },
        #[cfg(feature = "tilo-os")]
        storage_capacity: (fixture.scenario() == Some(Category::Storage.slug())).then_some(
            os_actions::DiskCapacity {
                total_bytes: 512_000_000_000,
                available_bytes: 187_400_000_000,
            },
        ),
        #[cfg(feature = "tilo-os")]
        remembered_drives: if fixture.scenario() == Some(STORAGE_DRIVES) {
            drives_fixture()
        } else {
            Vec::new()
        },
        access: access_fixture(fixture.scenario()),
        linked_account: capture_link_state(fixture.scenario()),
        home_relay: (fixture.scenario() == Some(Category::DeviceMesh.slug()))
            .then(home_relay_fixture),
        billing: billing_fixture(fixture.scenario()),
        fleet_devices: if matches!(
            fixture.scenario(),
            Some(scenario) if scenario == Category::DeviceMesh.slug()
        ) {
            fleet_fixture()
        } else {
            Vec::new()
        },
        network: match fixture.scenario() {
            Some(NETWORK_WIFI_OFF) => Some(NetworkSample::WifiOff),
            Some(NETWORK_SCANNING) => Some(NetworkSample::Scanning),
            Some(NETWORK_KNOWN) => Some(NetworkSample::Known),
            Some(NETWORK_WIRED) => Some(NetworkSample::Wired),
            Some(NETWORK_USAGE) => Some(NetworkSample::Usage),
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

fn fleet_fixture() -> Vec<ui::tilo_actions::remote::identity_admin::FleetDevice> {
    use ui::tilo_actions::remote::identity_admin::FleetDevice;
    let device = |label: &str, key: &str, fingerprint: &str, current, can_host| FleetDevice {
        label: label.into(),
        key_hex: key.repeat(8),
        fingerprint: fingerprint.into(),
        current,
        revoked: false,
        can_host,
        kind: ui::tilo_actions::remote::identity_admin::KeyKind::Device,
        browser: None,
        agent: None,
        issued_ms: None,
        expires_ms: None,
        last_active_ms: None,
        online: false,
        workspace: None,
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

fn home_relay_fixture() -> ui::tilo_actions::remote::home_relay_hosting::HomeRelayHosting {
    use ui::tilo_actions::remote::home_relay_hosting::{HomeRelayHosting, HostedWorkspace};
    let workspace = |room: &str, name: &str, on_relay| HostedWorkspace {
        workspace: room.into(),
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

pub(crate) fn access_fixture(
    scenario: Option<&str>,
) -> Option<crate::panes::devices_access::AccessSample> {
    use crate::device_fleet::DeviceView;
    use crate::panes::devices_access::{AccessSample, Asking};
    use ui::tilo_actions::remote::agent_door::{Door, Grant, Reach, Run};
    use ui::tilo_actions::remote::identity_admin::KeyKind;
    const MINUTE: i64 = 60_000;
    const DAY: i64 = 86_400_000;
    const NOW: i64 = 1_794_400_000_000;
    if scenario != Some(Category::DevicesAccess.slug()) {
        return None;
    }
    let device = |n: u8, label: &str| DeviceView {
        label: label.into(),
        key: [n; 32],
        fingerprint: format!("{n:02X} {n:02X} {n:02X} {n:02X} {n:02X} {n:02X}"),
        issued_ms: Some(NOW - 40 * DAY),
        ..DeviceView::default()
    };
    let tab = |n: u8| DeviceView {
        browser: Some("0123456789abcdef0123456789abcdef".into()),
        last_active_ms: Some(NOW - DAY - 3 * 60 * MINUTE),
        issued_ms: Some(NOW - 12 * DAY),
        ..device(n, "Chrome on Linux")
    };
    Some(AccessSample {
        devices: vec![
            DeviceView {
                current: true,
                can_host: true,
                ..device(1, "Framework 16")
            },
            DeviceView {
                last_active_ms: Some(NOW - 3 * 60 * MINUTE),
                ..device(2, "Pixel 9")
            },
            tab(3),
            tab(6),
            DeviceView {
                revoked: true,
                ..device(4, "Old phone")
            },
            DeviceView {
                kind: KeyKind::Agent,
                agent: Some("Claude Code".into()),
                online: true,
                issued_ms: Some(NOW - 9 * DAY),
                expires_ms: Some(NOW + 21 * DAY),
                ..device(5, "Claude Code on laptop")
            },
        ],
        door: Door {
            endpoint: "https://mcp.tilo.sh".into(),
            grants: vec![
                Grant {
                    id: "bb".repeat(32),
                    agent: "ChatGPT".into(),
                    reach: Reach::Workspace {
                        id: "6f1c0a52-8e9d-4c1b-9d2e-0c8a7b5e4f31".into(),
                        name: "Launch plan".into(),
                    },
                    granted_ms: NOW - 2 * DAY,
                    last_ms: Some(NOW - DAY),
                    runs: Vec::new(),
                    runs_on: String::new(),
                    expires_ms: Some(NOW + 28 * DAY),
                    browser_pages: false,
                    script_key: false,
                },
                Grant {
                    id: "cc".repeat(32),
                    agent: "Nightly export".into(),
                    reach: Reach::Account,
                    granted_ms: NOW - 30 * DAY,
                    last_ms: Some(NOW - 7 * 60 * MINUTE),
                    runs: vec![Run {
                        name: "Nightly export #12".into(),
                        started_ms: NOW - 2 * MINUTE,
                    }],
                    runs_on: "build-server".into(),
                    expires_ms: None,
                    browser_pages: false,
                    script_key: true,
                },
            ],
            workspaces: Vec::new(),
        },
        asking: vec![Asking {
            ask: "dd".repeat(32),
            label: "Safari on iPad".into(),
            code: "482 913".into(),
            via: "Pixel 9".into(),
        }],
        now_ms: NOW,
    })
}

pub(crate) fn billing_fixture(
    scenario: Option<&str>,
) -> Option<ui::tilo_actions::remote::host_account::HostAccount> {
    use ui::tilo_actions::remote::host_account::{
        Entitlement, HostAccount, HostKind, Invoice, InvoiceStatus, Limits, PaymentMethod,
        PlanOffer, Standing, Usage, WorkspaceUsage,
    };
    let pro = Limits {
        max_running: 2,
        max_vms: 5,
        sizes: vec!["small".into(), "medium".into()],
        included_vm_seconds: Some(100 * 3600),
        ..Limits::default()
    };
    let standing = match scenario? {
        "billing" => Standing {
            account: "acct-ada".into(),
            plan: Some("pro".into()),
            payment_method: Some(PaymentMethod {
                brand: Some("Visa".into()),
                last4: Some("4242".into()),
            }),
            entitlements: vec![Entitlement::LinuxVms],
            limits: Some(pro.clone()),
            usage: Usage {
                workspaces: vec![WorkspaceUsage {
                    workspace: "4d6f1a2c-0000-4000-8000-000000000002".into(),
                    vm_seconds: 37 * 3600,
                    running: true,
                    ..WorkspaceUsage::default()
                }],
                ..Usage::default()
            },
            credit: Some("$12.40".into()),
            invoices: vec![
                Invoice {
                    id: "in_2".into(),
                    issued_ms: 1_793_491_200_000,
                    amount: "$24.10".into(),
                    status: InvoiceStatus::Open,
                    url: "https://tilo.sh/billing/invoices/in_2".into(),
                },
                Invoice {
                    id: "in_1".into(),
                    issued_ms: 1_790_812_800_000,
                    amount: "$20.00".into(),
                    status: InvoiceStatus::Paid,
                    url: "https://tilo.sh/billing/invoices/in_1".into(),
                },
            ],
        },
        BILLING_NEW => Standing {
            account: "acct-ada".into(),
            ..Standing::default()
        },
        _ => return None,
    };
    Some(HostAccount {
        host: "Tilo Cloud".into(),
        kind: HostKind::Provider,
        plans: vec![PlanOffer {
            id: "pro".into(),
            name: "Pro".into(),
            price: Some("$20 a month".into()),
            limits: pro,
        }],
        standing: Some(standing),
        manage_billing: Some("https://tilo.sh/billing".into()),
    })
}

#[cfg(feature = "tilo-os")]
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

#[cfg(feature = "tilo-os")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceResidency {
    pub key: String,
    pub label: String,
    pub current: bool,
    pub summary: runtime_fs::storage::ResidencySummary,
}

#[cfg(feature = "tilo-os")]
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

#[cfg(feature = "tilo-os")]
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

#[cfg(feature = "tilo-os")]
fn windows_support(scenario: Option<&str>) -> Option<os_actions::windows_compat::Readiness> {
    use os_actions::windows_compat::{Readiness, SizeDisclosure};
    let disclosure = SizeDisclosure {
        download_bytes: 1_900_000_000,
        installed_bytes: 3_100_000_000,
    };
    let json = match scenario? {
        "windowsdemo-off" => r#"{"state":"not-installed"}"#,
        "windowsdemo-installing" => {
            r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":5,"label":"Downloading","bytes":900000000,"total":1900000000}}"#
        }
        "windowsdemo-installed" => r#"{"state":"installed","desired":true}"#,
        "windowsdemo-waiting" => r#"{"state":"waiting-for-network","desired":true}"#,
        "windowsdemo-failed" => {
            r#"{"state":"failed","desired":true,"last_error":"Could not download the manifest (connection reset)."}"#
        }
        _ => return None,
    };
    Some(Readiness::from_facts(
        false,
        Some(disclosure),
        &os_actions::windows_compat::ExtensionStatus::parse(json),
    ))
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staged {
    Pane(Category),
    List(Category),
    Search(Category, &'static str),
    Focused(Category, &'static str),
}

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
            | NETWORK_WIRED
            | NETWORK_USAGE
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
        Some(BILLING_NEW) => Staged::Pane(Category::Billing),
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
        Some(slug) if CODEC_SCENARIOS.contains(&slug) || WINDOWS_SCENARIOS.contains(&slug) => {
            Staged::Pane(Category::Updates)
        }
        Some(slug) => Staged::Pane(Category::from_slug(slug).unwrap_or(Category::Appearance)),
    }
}

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
#[cfg(feature = "tilo-os")]
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
                let mut live =
                    TestApp::<crate::resource::SettingsShell>::new().expect("live settings opens");
                let status = codec_status(Some(scenario)).expect("formats fixture status");
                live.with_widget::<crate::window::Inspected, ()>(|settings| {
                    let model =
                        crate::panes::updates::UpdatesModel::sample(settings.sections().clone())
                            .with_codec_pack(
                                std::sync::Arc::new(move || status.clone()),
                                std::sync::Arc::new(|_| Ok(())),
                            );
                    *crate::panes::updates::area(settings)
                        .unwrap()
                        .model()
                        .borrow_mut() = model;
                    settings.stage(Staged::Pane(Category::Updates));
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

    #[test]
    fn scenarios_name_their_pane_and_size() {
        assert_eq!(
            names().len(),
            Category::ALL.len() + 46 + CODEC_SCENARIOS.len() + WINDOWS_SCENARIOS.len()
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
        assert_eq!(staged(Some(BILLING_NEW)), Staged::Pane(Category::Billing));
        for scenario in CODEC_SCENARIOS {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Updates));
            assert!(codec_status(Some(scenario)).is_some());
        }
        for scenario in WINDOWS_SCENARIOS {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Updates));
            assert!(windows_support(Some(scenario)).is_some());
        }
        assert!(windows_support(None).is_none());
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
