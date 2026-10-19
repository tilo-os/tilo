//! Screenshot scenarios: which pane a capture shows, at which size, and the
//! stand-ins it shows in place of this device's hardware and accounts.
//!
//! Every pane is a scenario of its own, named by its id (`network`,
//! `region_formats`), captured wide. The narrow ones are drawn at a phone's
//! width: `narrow` keeps a pane's content on screen with the back arrow (what
//! a collapse shows), `narrow-list` the category list one back-tap away.
//! `search` shows the sidebar filtered by a query, `account-form` the form
//! for adding an account, `accounts-narrow` the account cards at a phone's
//! width. A plain `--shots` shows Appearance.

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

/// The query the `search` scenario types into the sidebar's field.
pub const SEARCH_QUERY: &str = "light";

const CODEC_SCENARIOS: &[&str] = &[
    "codecsdemo-off",
    "codecsdemo-installing",
    "codecsdemo-installed",
    "codecsdemo-waiting",
    "codecsdemo-failed",
];
const DEVICE_DISPLAYS: &str = "device-displays";
/// Storage with remembered NTFS drives, drawn tall enough to reach them.
const STORAGE_DRIVES: &str = "storage-drives";

/// Network with Wi-Fi off, looking for networks, and with a known
/// network's details open.
const NETWORK_WIFI_OFF: &str = "network-wifi-off";
const NETWORK_SCANNING: &str = "network-scanning";
const NETWORK_KNOWN: &str = "network-known";

const NARROW: &str = "narrow";
const NARROW_LIST: &str = "narrow-list";
const SEARCH: &str = "search";
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
                ACCOUNT_FORM,
                ACCOUNTS_NARROW,
                ACCOUNTS_LINKING,
                ACCOUNTS_LINKED,
                DEVICE_DISPLAYS,
                STORAGE_DRIVES,
                "keyboard-single",
                "keyboard-many",
                "keyboard-touch",
                "keyboard-osk",
                NETWORK_WIFI_OFF,
                NETWORK_SCANNING,
                NETWORK_KNOWN,
            ])
            .chain(CODEC_SCENARIOS.iter().copied())
            .collect()
    });
    &NAMES
}

/// The viewport `scenario` is drawn at.
pub fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some(NARROW | NARROW_LIST | ACCOUNTS_NARROW) => NARROW_VIEWPORT,
        Some(DEVICE_DISPLAYS) => (1200, 1100),
        Some(STORAGE_DRIVES) => (1200, 1450),
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
    /// Synthetic certified-peer observations; available only to capture fixtures.
    pub device_displays: Vec<settings_store::device_displays::DeviceDisplay>,
    pub person_devices: Vec<settings_store::person_displays::PersonDevice>,
    /// Synthetic attached keyboards; captures never discover the real host.
    pub input_hardware: Option<settings_store::hardware::InputHardwareSnapshot>,
    /// Stand-in storage summaries of the person's devices, for the
    /// storage capture.
    pub storage_devices: Vec<crate::panes::storage::DeviceResidency>,
    /// Stand-in remembered NTFS drives, for the drives capture.
    pub remembered_drives: Vec<ntfs_mount::protocol::RememberedDrive>,
    /// A room bridged to Discord, and where the person's account there
    /// stands, for the linked-accounts captures.
    pub linked_account: Option<ui::linked_accounts::LinkState>,
    /// The person's trusted devices, as a stand-in for the host's device
    /// list, for the devices & sync capture.
    pub fleet_devices: Vec<ui::tilo_actions::remote::identity_admin::FleetDevice>,
    /// How the sample networks stand, for the network captures.
    pub network: Option<NetworkSample>,
}

/// How the network captures' sample stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkSample {
    /// Wi-Fi is off.
    WifiOff,
    /// Wi-Fi is on and looking, nothing found yet.
    Scanning,
    /// Connected, with a known network's details open.
    Known,
}

/// A capture's room bridged to Discord, its person's account standing as
/// the scenario says.
pub(crate) struct CaptureBridge(pub ui::linked_accounts::LinkState);

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

/// The capture code the linking scenario shows.
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
        remembered_drives: if fixture.scenario() == Some(STORAGE_DRIVES) {
            crate::panes::drives::capture_fixture()
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
        fleet_devices: if fixture.scenario() == Some(Category::DeviceMesh.slug()) {
            fleet_fixture()
        } else {
            Vec::new()
        },
        network: match fixture.scenario() {
            Some(NETWORK_WIFI_OFF) => Some(NetworkSample::WifiOff),
            Some(NETWORK_SCANNING) => Some(NetworkSample::Scanning),
            Some(NETWORK_KNOWN) => Some(NetworkSample::Known),
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

/// Three devices of one person: a laptop holding work nothing else has
/// yet, a desktop that keeps one workspace and caches another, and a home
/// server that holds copies of both.
pub(crate) fn storage_fixture() -> Vec<crate::panes::storage::DeviceResidency> {
    use crate::panes::storage::DeviceResidency;
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
    /// `category` with the sidebar filtered by [`SEARCH_QUERY`].
    Search(Category),
}

/// What `scenario` shows.
pub fn staged(scenario: Option<&str>) -> Staged {
    match scenario {
        None => Staged::Pane(Category::Appearance),
        Some(DEVICE_DISPLAYS) => Staged::Pane(Category::Displays),
        Some(STORAGE_DRIVES) => Staged::Pane(Category::Storage),
        Some("keyboard-single" | "keyboard-many" | "keyboard-touch" | "keyboard-osk") => {
            Staged::Pane(Category::Keyboard)
        }
        Some(NETWORK_WIFI_OFF | NETWORK_SCANNING | NETWORK_KNOWN) => {
            Staged::Pane(Category::Network)
        }
        Some(NARROW) => Staged::Pane(Category::RegionFormats),
        Some(NARROW_LIST) => Staged::List(Category::RegionFormats),
        Some(SEARCH) => Staged::Search(Category::Appearance),
        Some(ACCOUNT_FORM | ACCOUNTS_NARROW | ACCOUNTS_LINKING | ACCOUNTS_LINKED) => {
            Staged::Pane(Category::ExternalAccounts)
        }
        Some(slug) if CODEC_SCENARIOS.contains(&slug) => Staged::Pane(Category::Updates),
        Some(slug) => Staged::Pane(Category::from_slug(slug).unwrap_or(Category::Appearance)),
    }
}

/// Synthetic input observations use the same snapshot contract as the compositor.
pub(crate) fn keyboard_fixture(
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
                assert!(
                    labels.iter().any(|label| label.contains(status)),
                    "{scenario}: {labels:?}"
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
                    *settings.updates().model().borrow_mut() = model;
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
            Category::ALL.len() + 16 + CODEC_SCENARIOS.len()
        );
        for scenario in [NETWORK_WIFI_OFF, NETWORK_SCANNING, NETWORK_KNOWN] {
            assert_eq!(staged(Some(scenario)), Staged::Pane(Category::Network));
        }
        assert_eq!(
            staged(Some(STORAGE_DRIVES)),
            Staged::Pane(Category::Storage)
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
        assert_eq!(staged(Some("search")), Staged::Search(Category::Appearance));
        assert_eq!(viewport(Some("accounts-narrow")), NARROW_VIEWPORT);
    }
}
